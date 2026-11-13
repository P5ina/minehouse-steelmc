//! The hopper minecart (vanilla `MinecartHopper`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{AbstractMinecartEntityData, HopperMinecartEntityData};
use steel_registry::vanilla_items;
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::SyncMutex;
use steel_utils::translations;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Direction, Downcast as _, DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
use super::minecart_container::{ContainerMinecartMenuKind, MinecartContainer};
use crate::behavior::InteractionResult;
use crate::block_entity::entities::{
    HOPPER_SLOTS, hopper_add_item_entity as add_item_entity, hopper_suck_in_items as suck_in_items,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, MoveResult,
    RemovalReason, reset_forward_direction_of_relative_portal_position,
};
use crate::inventory::lock::ContainerRef;
use crate::inventory::menu::kinds::hopper_with_kind;
use crate::physics::MoverType;
use crate::player::Player;
use crate::portal::portal_shape::PortalShape;
use crate::world::{Explosion, World};

/// Vanilla `MinecartHopper.getDefaultDisplayOffset`.
const HOPPER_DISPLAY_OFFSET: i32 = 1;

/// A minecart carrying a hopper that picks up items as it rolls.
#[entity_behavior(class = "MinecartHopper")]
pub struct HopperMinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<HopperMinecartEntityData>,
    minecart: MinecartCore,
    container: MinecartContainer,
    enabled: AtomicBool,
    consumed_item_this_frame: AtomicBool,
}

// SAFETY: This key is owned by Steel and uniquely identifies `HopperMinecartEntity`.
unsafe impl DowncastType for HopperMinecartEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/hopper_minecart");
}

impl HopperMinecartEntity {
    /// Creates a new hopper minecart.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a hopper minecart from saved data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let mut entity_data = HopperMinecartEntityData::new();
        entity_data
            .abstract_minecart
            .id_display_offset
            .set(HOPPER_DISPLAY_OFFSET);
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(entity_data),
            minecart: MinecartCore::new(),
            container: MinecartContainer::new(HOPPER_SLOTS),
            enabled: AtomicBool::new(true),
            consumed_item_this_frame: AtomicBool::new(false),
        }
    }

    /// Vanilla `setLootTable(table, seed)`.
    pub fn set_loot_table(&self, loot_table: Identifier, seed: i64) {
        self.container.set_loot_table(loot_table, seed);
    }

    /// The hopper's inventory, rolling a pending loot table first.
    #[must_use]
    pub fn container_ref(&self) -> ContainerRef {
        self.container.container_ref(self)
    }

    /// Vanilla `isEnabled`: whether the hopper picks items up; a powered
    /// activator rail turns it off.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Vanilla `setEnabled`.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Vanilla `getLevelX/Y/Z`: the hopper sits half a block above the cart's feet.
    fn level_position(&self) -> DVec3 {
        self.position() + DVec3::new(0.0, 0.5, 0.0)
    }

    /// Vanilla `MinecartHopper.tryConsumeItems`.
    fn try_consume_items(&self) {
        if self.is_alive()
            && self.is_enabled()
            && !self.consumed_item_this_frame.load(Ordering::Relaxed)
            && self.suck_in_items()
        {
            self.consumed_item_this_frame.store(true, Ordering::Relaxed);
        }
    }

    /// Vanilla `MinecartHopper.suckInItems`: the container above first, then
    /// item entities touching the cart.
    #[must_use]
    pub fn suck_in_items(&self) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        let hopper = self.container_ref();
        if suck_in_items(&world, &hopper, self.level_position(), false) {
            return true;
        }
        let area = self.bounding_box().inflate_xyz(0.25, 0.0, 0.25);
        world
            .get_entities_in_aabb_matching(&area, |entity| {
                entity.is_alive() && entity.downcast_ref::<ItemEntity>().is_some()
            })
            .iter()
            .filter_map(|entity| entity.downcast_ref::<ItemEntity>())
            .any(|item| add_item_entity(&hopper, item))
    }

    /// Vanilla `ContainerEntity.interactWithContainerVehicle` and
    /// `AbstractMinecartContainer.createMenu`.
    fn open_hopper_menu(&self, player: &Player) {
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
            TextComponent::translated(translations::ENTITY_MINECRAFT_HOPPER_MINECART.msg())
        });
        player.open_menu(title, move |context| {
            hopper_with_kind(
                inventory,
                context.container_id,
                container.clone(),
                ContainerMinecartMenuKind::new(Arc::downgrade(&cart), container.clone(), false),
            )
        });
    }
}

impl AbstractMinecart for HopperMinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(&mut self.entity_data.lock().abstract_minecart);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::HOPPER_MINECART
    }

    fn default_display_offset(&self) -> i32 {
        HOPPER_DISPLAY_OFFSET
    }

    /// Vanilla `MinecartHopper.activateMinecart`: a powered activator rail
    /// switches the hopper off, an unpowered one back on.
    fn activate_minecart(&self, _world: &Arc<World>, _pos: BlockPos, powered: bool) {
        let enabled = !powered;
        if enabled != self.is_enabled() {
            self.set_enabled(enabled);
        }
    }

    fn apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        self.container
            .apply_natural_slowdown(movement, self.is_in_water())
    }

    /// Vanilla `AbstractMinecartContainer.destroy`.
    fn destroy(&self, world: &World, source: &DamageSource) {
        self.default_destroy(world);
        self.container.destroyed(self, world, source);
    }
}

impl Entity for HopperMinecartEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `MinecartHopper.tick`.
    fn tick(self: Arc<Self>) {
        self.consumed_item_this_frame
            .store(false, Ordering::Relaxed);
        tick_minecart(&self);
        self.try_consume_items();
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

    /// Vanilla `AbstractMinecartContainer.interact`.
    fn interact(
        &self,
        player: &Player,
        _hand: InteractionHand,
        _location: DVec3,
    ) -> InteractionResult {
        self.open_hopper_menu(player);
        InteractionResult::Success
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_vehicle(world, source, amount)
    }

    /// Vanilla `AbstractMinecartContainer.remove`: a killed or discarded
    /// cart spills its hopper.
    fn set_removed(&self, reason: RemovalReason) {
        if matches!(reason, RemovalReason::Killed | RemovalReason::Discarded)
            && let Some(world) = self.level()
        {
            self.container.drop_contents(self, &world);
        }
        self.default_set_removed(reason);
    }

    fn container_entity(&self) -> Option<ContainerRef> {
        self.is_alive().then(|| self.container_ref())
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

    /// Vanilla `MinecartHopper.addAdditionalSaveData`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_minecart(nbt);
        self.container.save(nbt);
        nbt.insert("Enabled", i8::from(self.is_enabled()));
    }

    /// Vanilla `MinecartHopper.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
        self.container.load(nbt);
        self.set_enabled(nbt.byte("Enabled").is_none_or(|enabled| enabled != 0));
    }
}
