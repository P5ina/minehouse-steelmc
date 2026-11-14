//! The rideable minecart (vanilla `Minecart`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{
    AbstractMinecartEntityData, MinecartEntityData, VehicleEntityData,
};
use steel_registry::vanilla_items;
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Direction, DowncastType, DowncastTypeKey};

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
use super::vehicle_entity::VehicleEntity;
use crate::behavior::InteractionResult;
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, MoveResult,
    reset_forward_direction_of_relative_portal_position,
};
use crate::physics::MoverType;
use crate::player::Player;
use crate::portal::portal_shape::PortalShape;
use crate::world::{Explosion, World};

/// Damage an activator rail deals to an empty cart, enough to wobble it hard.
const ACTIVATOR_RAIL_DAMAGE: f32 = 50.0;

/// A minecart players and mobs can ride.
#[entity_behavior(class = "Minecart")]
pub struct MinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<MinecartEntityData>,
    minecart: MinecartCore,
}

// SAFETY: This key is owned by Steel and uniquely identifies `MinecartEntity`.
unsafe impl DowncastType for MinecartEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/minecart");
}

impl MinecartEntity {
    /// Creates a new minecart.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(MinecartEntityData::new()),
            minecart: MinecartCore::new(),
        }
    }

    /// Creates a minecart from saved data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(MinecartEntityData::new()),
            minecart: MinecartCore::new(),
        }
    }
}

impl VehicleEntity for MinecartEntity {
    fn with_vehicle_data(&self, f: &mut dyn FnMut(&mut VehicleEntityData)) {
        f(&mut self
            .entity_data
            .lock()
            .abstract_minecart_mut()
            .vehicle_entity);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::MINECART
    }
}

impl AbstractMinecart for MinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(self.entity_data.lock().abstract_minecart_mut());
    }

    fn is_rideable(&self) -> bool {
        true
    }

    fn activate_minecart(&self, _world: &Arc<World>, _pos: BlockPos, powered: bool) {
        if !powered {
            return;
        }
        for passenger in self.passengers().into_iter().rev() {
            passenger.stop_riding();
        }
        if self.hurt_time() == 0 {
            self.set_hurt_dir(-self.hurt_dir());
            self.set_hurt_time(10);
            self.set_vehicle_damage(ACTIVATOR_RAIL_DAMAGE);
            self.mark_hurt();
        }
    }
}

impl Entity for MinecartEntity {
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

    fn interact(
        &self,
        player: &Player,
        _hand: InteractionHand,
        _location: DVec3,
    ) -> InteractionResult {
        if player.is_secondary_use_active() || self.is_vehicle() {
            return InteractionResult::Pass;
        }
        let Some(vehicle) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return InteractionResult::Consume;
        };
        if player.start_riding(&vehicle) {
            InteractionResult::SuccessServer
        } else {
            InteractionResult::Consume
        }
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_vehicle(world, source, amount)
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

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_minecart(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
    }
}
