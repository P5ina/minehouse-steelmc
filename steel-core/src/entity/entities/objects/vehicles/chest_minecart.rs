//! The chest minecart (vanilla `MinecartChest`, `AbstractMinecartContainer`
//! and `ContainerEntity`).

use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{AbstractMinecartEntityData, ChestMinecartEntityData};
use steel_registry::vanilla_game_rules::ENTITY_DROPS;
use steel_registry::{vanilla_game_events, vanilla_items};
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::{IntoShared as _, Shared, SyncMutex};
use steel_utils::translations;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Direction, DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
use crate::behavior::InteractionResult;
use crate::behavior::blocks::anger_nearby_piglins;
use crate::block_entity::container_loot::ContainerLoot;
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, MoveResult,
    RemovalReason, WeakEntity, reset_forward_direction_of_relative_portal_position,
};
use crate::inventory::container::{Container, SimpleContainer};
use crate::inventory::lock::{ContainerId, ContainerRef};
use crate::inventory::menu::MenuBehavior;
use crate::inventory::menu::MenuKind;
use crate::inventory::menu::kinds::{ChestKind, chest_with_kind};
use crate::physics::MoverType;
use crate::player::Player;
use crate::portal::portal_shape::PortalShape;
use crate::world::game_event::GameEventContext;
use crate::world::{Explosion, World};

/// Slots in a chest minecart (vanilla `MinecartChest.getContainerSize`).
pub const CHEST_MINECART_SLOTS: usize = 27;
/// Vanilla `ContainerEntity.isChestVehicleStillValid` reach buffer.
const CONTAINER_REACH_BUFFER: f64 = 4.0;
/// Vanilla `MinecartChest.getDefaultDisplayOffset`.
const CHEST_DISPLAY_OFFSET: i32 = 8;

/// A minecart carrying a 27-slot chest.
#[entity_behavior(class = "MinecartChest")]
pub struct ChestMinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ChestMinecartEntityData>,
    minecart: MinecartCore,
    container: Shared<SimpleContainer>,
    loot: ContainerLoot,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ChestMinecartEntity`.
unsafe impl DowncastType for ChestMinecartEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/chest_minecart");
}

impl ChestMinecartEntity {
    /// Creates a new chest minecart entity.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a chest minecart entity from saved data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let mut entity_data = ChestMinecartEntityData::new();
        entity_data
            .abstract_minecart
            .id_display_offset
            .set(CHEST_DISPLAY_OFFSET);
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(entity_data),
            minecart: MinecartCore::new(),
            container: SimpleContainer::new(CHEST_MINECART_SLOTS).into_shared(),
            loot: ContainerLoot::new(),
        }
    }

    /// Vanilla `setLootTable(table, seed)`: the loot rolled when the chest is first reached.
    pub fn set_loot_table(&self, loot_table: Identifier, seed: i64) {
        self.loot.set(loot_table, seed);
    }

    /// The chest's inventory, rolling a pending loot table first.
    #[must_use]
    pub fn container_ref(&self) -> ContainerRef {
        self.unpack_chest_vehicle_loot_table(None);
        ContainerRef::from(&self.container)
    }

    /// Vanilla `AbstractContainerMenu.getRedstoneSignalFromContainer` over
    /// this cart's chest, which rolls a pending loot table first.
    #[must_use]
    pub fn redstone_signal(&self) -> i32 {
        self.unpack_chest_vehicle_loot_table(None);
        redstone_signal_from_container(&*self.container.lock())
    }

    /// Vanilla `ContainerEntity.unpackChestVehicleLootTable`.
    fn unpack_chest_vehicle_loot_table(&self, player: Option<&Player>) {
        if !self.loot.is_pending() {
            return;
        }
        let pos = {
            let position = self.position();
            BlockPos::containing(position.x, position.y, position.z)
        };
        let mut container = self.container.lock();
        // Vanilla rolls with the cart's exact position as origin; the loot
        // origin only feeds location conditions, which use its block.
        self.loot.unpack(&mut *container, pos, player);
    }

    /// Vanilla `Containers.dropContents(level, entity, container)`.
    fn drop_contents(&self, world: &Arc<World>) {
        self.unpack_chest_vehicle_loot_table(None);
        let items: Vec<ItemStack> = {
            let mut container = self.container.lock();
            container.items_mut().iter_mut().map(mem::take).collect()
        };
        let pos = {
            let position = self.position();
            BlockPos::containing(position.x, position.y, position.z)
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    /// Vanilla `ContainerEntity.interactWithContainerVehicle` and
    /// `AbstractMinecartContainer.createMenu`.
    fn open_chest_menu(&self, player: &Player) {
        if self.loot.is_pending() && player.is_spectator() {
            return;
        }
        self.unpack_chest_vehicle_loot_table(Some(player));
        let Some(cart) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return;
        };
        let container = ContainerRef::from(&self.container);
        let inventory = Arc::clone(&player.inventory);
        let title = self.custom_name().unwrap_or_else(|| {
            TextComponent::translated(translations::ENTITY_MINECRAFT_CHEST_MINECART.msg())
        });
        player.open_menu(title, move |context| {
            chest_with_kind(
                inventory,
                context.container_id,
                container.clone(),
                3,
                ChestMinecartMenuKind {
                    cart: Arc::downgrade(&cart),
                    chest: ChestKind::new(vec![container]),
                },
            )
        });
    }
}

/// `ChestMenu` over a chest minecart: valid while the cart is in reach, and
/// emitting `container_close` when it closes (vanilla `MinecartChest.stopOpen`).
struct ChestMinecartMenuKind {
    cart: WeakEntity,
    chest: ChestKind,
}

// SAFETY: This key is owned by Steel and uniquely identifies the chest minecart menu kind.
unsafe impl DowncastType for ChestMinecartMenuKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/chest_minecart");
}

impl MenuKind for ChestMinecartMenuKind {
    /// Vanilla `ContainerEntity.isChestVehicleStillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.cart.upgrade().is_some_and(|cart| {
            !cart.is_removed()
                && player.is_within_entity_interaction_range_with_buffer(
                    cart.bounding_box(),
                    CONTAINER_REACH_BUFFER,
                )
        })
    }

    fn start_open(&mut self, player: &Player) {
        self.chest.start_open(player);
    }

    fn chest_menu_containers(&self) -> Vec<ContainerId> {
        self.chest.chest_menu_containers()
    }

    fn removed(&mut self, behavior: &mut MenuBehavior, player: &Player) {
        self.chest.removed(behavior, player);
        let Some(cart) = self.cart.upgrade() else {
            return;
        };
        let Some(world) = cart.level() else {
            return;
        };
        world.game_event_at(
            &vanilla_game_events::CONTAINER_CLOSE,
            cart.position(),
            &GameEventContext::new(Some(player as &dyn Entity), None),
        );
    }
}

impl AbstractMinecart for ChestMinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(&mut self.entity_data.lock().abstract_minecart);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::CHEST_MINECART
    }

    fn default_display_offset(&self) -> i32 {
        CHEST_DISPLAY_OFFSET
    }

    /// Vanilla `AbstractMinecartContainer.applyNaturalSlowdown`: a fuller
    /// chest rolls further.
    fn apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        let mut keep = 0.98_f32;
        if !self.loot.is_pending() {
            let emptiness = 15 - redstone_signal_from_container(&*self.container.lock());
            #[expect(clippy::cast_precision_loss, reason = "emptiness is between 0 and 15")]
            {
                keep += emptiness as f32 * 0.001;
            }
        }
        if self.is_in_water() {
            keep *= 0.95;
        }
        DVec3::new(
            movement.x * f64::from(keep),
            0.0,
            movement.z * f64::from(keep),
        )
    }

    /// Vanilla `AbstractMinecartContainer.destroy` and `ContainerEntity.chestVehicleDestroyed`.
    fn destroy(&self, world: &World, source: &DamageSource) {
        self.default_destroy(world);
        if !world.get_game_rule(&ENTITY_DROPS) {
            return;
        }
        if let Some(world) = self.level() {
            self.drop_contents(&world);
            if let Some(player) = source.direct_entity().and_then(|entity| entity.as_player()) {
                anger_nearby_piglins(&world, player, true);
            }
        }
    }
}

/// Vanilla `AbstractContainerMenu.getRedstoneSignalFromContainer`.
#[must_use]
pub fn redstone_signal_from_container(container: &dyn Container) -> i32 {
    let mut total = 0.0_f32;
    let size = container.get_container_size();
    for slot in 0..size {
        let item = container.get_item(slot);
        if !item.is_empty() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "stack counts and sizes are small"
            )]
            {
                total += item.count() as f32 / container.get_max_stack_size_for_item(item) as f32;
            }
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "container sizes are small")]
    let total = total / size as f32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla's Mth.lerpDiscrete truncates"
    )]
    let signal = (total * 14.0).floor() as i32 + i32::from(total > 0.0);
    signal
}

impl Entity for ChestMinecartEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        tick_minecart(&self);
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        let result = Arc::clone(&self).default_move_entity(mover_type, delta);
        self.apply_effects_from_blocks();
        result
    }

    fn apply_effects_from_blocks(&self) {
        let position = self.position();
        self.apply_effects_from_blocks_between(position, position);
        self.base().clear_movement_this_tick();
    }

    /// Vanilla `MinecartChest.interact`.
    fn interact(
        &self,
        player: &Player,
        _hand: InteractionHand,
        _location: DVec3,
    ) -> InteractionResult {
        self.open_chest_menu(player);
        self.game_event_with_source_entity(
            &vanilla_game_events::CONTAINER_OPEN,
            Some(player as &dyn Entity),
        );
        if let Some(world) = self.level() {
            anger_nearby_piglins(&world, player, true);
        }
        InteractionResult::Success
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_vehicle(world, source, amount)
    }

    /// Vanilla `AbstractMinecartContainer.remove`: a killed or discarded
    /// cart spills its chest.
    fn set_removed(&self, reason: RemovalReason) {
        if matches!(reason, RemovalReason::Killed | RemovalReason::Discarded)
            && let Some(world) = self.level()
        {
            self.drop_contents(&world);
        }
        self.default_set_removed(reason);
    }

    fn is_on_rails(&self) -> bool {
        self.minecart_on_rails()
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn is_pickable(&self) -> bool {
        !self.is_removed()
    }

    fn is_pushable(&self) -> bool {
        true
    }

    fn blocks_building(&self) -> bool {
        true
    }

    fn can_collide_with(&self, other: &dyn Entity) -> bool {
        (other.can_be_collided_with(Some(self)) || other.is_pushable())
            && !self.is_passenger_of_same_vehicle(other)
    }

    fn push_entity(&self, entity: &dyn Entity) {
        self.push_minecart_entity(entity);
    }

    fn passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
        self.minecart_passenger_attachment_point(passenger)
            .unwrap_or_else(|| self.default_passenger_attachment_point(passenger))
    }

    fn dismount_location_for_passenger(&self, passenger: &dyn Entity) -> DVec3 {
        self.minecart_dismount_location(passenger)
    }

    fn motion_direction(&self) -> Direction {
        self.minecart_motion_direction()
    }

    fn known_movement(&self) -> DVec3 {
        let movement = self.velocity();
        if movement.is_nan() {
            return DVec3::ZERO;
        }
        DVec3::new(
            movement.x.clamp(-0.4, 0.4),
            movement.y,
            movement.z.clamp(-0.4, 0.4),
        )
    }

    fn block_speed_factor(&self) -> f32 {
        self.minecart_block_speed_factor()
    }

    fn get_default_gravity(&self) -> f64 {
        self.minecart_gravity()
    }

    fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
        self.vehicle_ignores_explosion(explosion)
    }

    fn dimension_changing_delay(&self) -> i32 {
        10
    }

    fn get_relative_portal_position(&self, axis: Axis, portal_area: FoundRectangle) -> DVec3 {
        reset_forward_direction_of_relative_portal_position(PortalShape::get_relative_position(
            portal_area,
            axis,
            self.position(),
            self.dimensions_for_pose(self.pose()),
        ))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    /// Vanilla `AbstractMinecartContainer.addAdditionalSaveData`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_minecart(nbt);
        if self.loot.save(nbt) {
            return;
        }
        let container = self.container.lock();
        let items: Vec<NbtCompound> = container
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_empty())
            .filter_map(|(slot, item)| match item.clone().to_nbt_tag() {
                NbtTag::Compound(mut item_nbt) => {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "chest minecart slots fit in a byte"
                    )]
                    item_nbt.insert("Slot", slot as i8);
                    Some(item_nbt)
                }
                _ => None,
            })
            .collect();
        nbt.insert("Items", NbtList::Compound(items));
    }

    /// Vanilla `AbstractMinecartContainer.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
        let mut container = self.container.lock();
        for item in container.items_mut() {
            *item = ItemStack::empty();
        }
        if self.loot.load(nbt) {
            return;
        }
        let Some(compounds) = nbt.list("Items").and_then(|list| list.compounds()) else {
            return;
        };
        for compound in compounds {
            let Some(slot) = compound.byte("Slot") else {
                continue;
            };
            let Ok(slot) = usize::try_from(slot) else {
                continue;
            };
            if slot < CHEST_MINECART_SLOTS
                && let Some(item) = ItemStack::from_borrowed_compound(&compound)
            {
                container.items_mut()[slot] = item;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steel_registry::vanilla_entities;

    #[test]
    fn chest_minecart_saves_structure_loot_table_state() {
        let minecart = ChestMinecartEntity::new(
            &vanilla_entities::CHEST_MINECART,
            1,
            DVec3::new(1.5, 2.5, 3.5),
            Weak::new(),
        );
        minecart.set_loot_table(
            Identifier::new_static("minecraft", "chests/abandoned_mineshaft"),
            42,
        );

        let mut nbt = NbtCompound::new();
        minecart.save_additional(&mut nbt);

        assert_eq!(
            nbt.string("LootTable").map(ToString::to_string),
            Some("minecraft:chests/abandoned_mineshaft".to_owned())
        );
        assert_eq!(nbt.long("LootTableSeed"), Some(42));
        assert_eq!(nbt.byte("HasTicked"), Some(1));
        assert_eq!(nbt.byte("FlippedRotation"), Some(0));
    }

    #[test]
    fn chest_minecart_is_pickable_and_pushable_like_vanilla() {
        let minecart = ChestMinecartEntity::new(
            &vanilla_entities::CHEST_MINECART,
            1,
            DVec3::new(1.5, 2.5, 3.5),
            Weak::new(),
        );

        assert!(minecart.is_pickable());
        assert!(minecart.is_pushable());
        assert!(minecart.blocks_building());
    }

    #[test]
    fn chest_minecart_relative_portal_position_resets_forward_offset() {
        let minecart = ChestMinecartEntity::new(
            &vanilla_entities::CHEST_MINECART,
            1,
            DVec3::new(12.0, 66.0, 20.75),
            Weak::new(),
        );
        let portal_area = FoundRectangle {
            min_corner: steel_utils::BlockPos::new(10, 64, 20),
            axis1_size: 4,
            axis2_size: 5,
        };

        assert!(
            minecart
                .get_relative_portal_position(Axis::X, portal_area)
                .z
                .abs()
                < f64::EPSILON
        );
    }
}
