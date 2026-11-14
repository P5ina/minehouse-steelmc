//! The chest minecart (vanilla `MinecartChest`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{
    AbstractMinecartEntityData, ChestMinecartEntityData, VehicleEntityData,
};
use steel_registry::{vanilla_game_events, vanilla_items};
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::SyncMutex;
use steel_utils::translations;
use steel_utils::types::InteractionHand;
use steel_utils::{Direction, DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
use super::vehicle_container::{ContainerVehicleMenuKind, VehicleContainer};
use super::vehicle_entity::VehicleEntity;
use crate::behavior::InteractionResult;
use crate::behavior::blocks::anger_nearby_piglins;
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, MoveResult,
    RemovalReason, reset_forward_direction_of_relative_portal_position,
};
use crate::inventory::lock::ContainerRef;
use crate::inventory::menu::kinds::chest_with_kind;
use crate::physics::MoverType;
use crate::player::Player;
use crate::portal::portal_shape::PortalShape;
use crate::world::{Explosion, World};

/// Slots in a chest minecart (vanilla `MinecartChest.getContainerSize`).
pub const CHEST_MINECART_SLOTS: usize = 27;
/// Vanilla `MinecartChest.getDefaultDisplayOffset`.
const CHEST_DISPLAY_OFFSET: i32 = 8;

/// A minecart carrying a 27-slot chest.
#[entity_behavior(class = "MinecartChest")]
pub struct ChestMinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ChestMinecartEntityData>,
    minecart: MinecartCore,
    container: VehicleContainer,
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
            container: VehicleContainer::new(CHEST_MINECART_SLOTS),
        }
    }

    /// Vanilla `setLootTable(table, seed)`: the loot rolled when the chest is first reached.
    pub fn set_loot_table(&self, loot_table: Identifier, seed: i64) {
        self.container.set_loot_table(loot_table, seed);
    }

    /// The chest's inventory, rolling a pending loot table first.
    #[must_use]
    pub fn container_ref(&self) -> ContainerRef {
        self.container.container_ref(self)
    }

    /// Vanilla `AbstractContainerMenu.getRedstoneSignalFromContainer` over
    /// this cart's chest, which rolls a pending loot table first.
    #[must_use]
    pub fn redstone_signal(&self) -> i32 {
        self.container.redstone_signal(self)
    }

    /// Vanilla `ContainerEntity.interactWithContainerVehicle` and
    /// `AbstractMinecartContainer.createMenu`.
    fn open_chest_menu(&self, player: &Player) {
        if !self.container.prepare_open(self, player) {
            return;
        }
        let Some(cart) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return;
        };
        let container = ContainerRef::from(self.container.raw());
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
                ContainerVehicleMenuKind::new(Arc::downgrade(&cart), container.clone(), true),
            )
        });
    }
}

impl VehicleEntity for ChestMinecartEntity {
    fn with_vehicle_data(&self, f: &mut dyn FnMut(&mut VehicleEntityData)) {
        f(&mut self.entity_data.lock().abstract_minecart.vehicle_entity);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::CHEST_MINECART
    }

    /// Vanilla `AbstractMinecartContainer.destroy`.
    fn destroy(&self, world: &World, source: &DamageSource) {
        self.default_destroy(world);
        self.container.destroyed(self, world, source);
    }
}

impl AbstractMinecart for ChestMinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(&mut self.entity_data.lock().abstract_minecart);
    }

    fn default_display_offset(&self) -> i32 {
        CHEST_DISPLAY_OFFSET
    }

    fn apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        self.container
            .apply_natural_slowdown(movement, self.is_in_water())
    }
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
            self.container.drop_contents(self, &world);
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
        self.container.save(nbt);
    }

    /// Vanilla `AbstractMinecartContainer.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
        self.container.load(nbt);
    }

    fn container_entity(&self) -> Option<ContainerRef> {
        self.is_alive().then(|| self.container_ref())
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
