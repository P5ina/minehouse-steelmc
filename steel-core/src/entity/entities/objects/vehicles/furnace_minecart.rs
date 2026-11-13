//! The furnace minecart (vanilla `MinecartFurnace`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{AbstractMinecartEntityData, FurnaceMinecartEntityData};
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::vanilla_items;
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{Direction, DowncastType, DowncastTypeKey};

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
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

/// Vanilla `MinecartFurnace.FUEL_TICKS_PER_ITEM`.
const FUEL_TICKS_PER_ITEM: i32 = 3600;
/// Vanilla `MinecartFurnace.MAX_FUEL_TICKS`.
const MAX_FUEL_TICKS: i32 = 32000;

#[derive(Debug, Default, Clone, Copy)]
struct FurnaceState {
    fuel: i32,
    push: DVec3,
}

/// A minecart that pushes itself, and the carts in front of it, while it burns fuel.
#[entity_behavior(class = "MinecartFurnace")]
pub struct FurnaceMinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<FurnaceMinecartEntityData>,
    minecart: MinecartCore,
    furnace: SyncMutex<FurnaceState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `FurnaceMinecartEntity`.
unsafe impl DowncastType for FurnaceMinecartEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/furnace_minecart");
}

impl FurnaceMinecartEntity {
    /// Creates a new furnace minecart.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a furnace minecart from saved data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(FurnaceMinecartEntityData::new()),
            minecart: MinecartCore::new(),
            furnace: SyncMutex::new(FurnaceState::default()),
        }
    }

    /// Ticks of fuel left.
    #[must_use]
    pub fn fuel(&self) -> i32 {
        self.furnace.lock().fuel
    }

    /// The horizontal push the burning furnace applies.
    #[must_use]
    pub fn push(&self) -> DVec3 {
        self.furnace.lock().push
    }

    /// Vanilla `MinecartFurnace.addFuel`: coal or charcoal adds 3600 ticks
    /// and pushes the cart away from whoever fed it.
    pub fn add_fuel(&self, interacting_pos: DVec3, item: &ItemStack) -> bool {
        let mut furnace = self.furnace.lock();
        if !item.item.has_tag(&ItemTag::FURNACE_MINECART_FUEL)
            || furnace.fuel + FUEL_TICKS_PER_ITEM > MAX_FUEL_TICKS
        {
            return false;
        }
        furnace.fuel += FUEL_TICKS_PER_ITEM;
        if furnace.fuel > 0 {
            let away = self.position() - interacting_pos;
            furnace.push = DVec3::new(away.x, 0.0, away.z);
        }
        true
    }

    /// Whether the furnace burns, synced so clients light it and puff smoke.
    #[must_use]
    pub fn has_fuel(&self) -> bool {
        *self.entity_data.lock().minecart_furnace.id_fuel.get()
    }

    fn set_has_fuel(&self, has_fuel: bool) {
        self.entity_data
            .lock()
            .minecart_furnace
            .id_fuel
            .set(has_fuel);
    }

    /// Vanilla `MinecartFurnace.calculateNewPushAlong`: keeps the push's
    /// strength but turns it along the track the cart moves on.
    fn calculate_new_push_along(push: DVec3, movement: DVec3) -> DVec3 {
        let horizontal = |v: DVec3| v.x * v.x + v.z * v.z;
        if horizontal(push) > 1.0e-4 && horizontal(movement) > 0.001 {
            let projected = movement * (push.dot(movement) / movement.dot(movement));
            projected.normalize_or_zero() * push.length()
        } else {
            push
        }
    }
}

impl AbstractMinecart for FurnaceMinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(&mut self.entity_data.lock().minecart_furnace.abstract_minecart);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::FURNACE_MINECART
    }

    fn is_furnace(&self) -> bool {
        true
    }

    /// Vanilla `MinecartFurnace.getMaxSpeed`: half speed, three quarters in water.
    fn max_speed(&self) -> f64 {
        if self.is_in_water() {
            self.default_max_speed() * 0.75
        } else {
            self.default_max_speed() * 0.5
        }
    }

    /// Vanilla `MinecartFurnace.applyNaturalSlowdown`.
    fn apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        let new_movement = {
            let mut furnace = self.furnace.lock();
            if furnace.push.length_squared() > 1.0e-7 {
                furnace.push = Self::calculate_new_push_along(furnace.push, movement);
                let pushed = DVec3::new(movement.x * 0.8, 0.0, movement.z * 0.8) + furnace.push;
                if self.is_in_water() {
                    pushed * 0.1
                } else {
                    pushed
                }
            } else {
                DVec3::new(movement.x * 0.98, 0.0, movement.z * 0.98)
            }
        };
        self.default_apply_natural_slowdown(new_movement)
    }
}

impl Entity for FurnaceMinecartEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `MinecartFurnace.tick`: burns a tick of fuel after moving.
    fn tick(self: Arc<Self>) {
        tick_minecart(&self);
        let has_fuel = {
            let mut furnace = self.furnace.lock();
            if furnace.fuel > 0 {
                furnace.fuel -= 1;
            }
            if furnace.fuel <= 0 {
                furnace.push = DVec3::ZERO;
            }
            furnace.fuel > 0
        };
        self.set_has_fuel(has_fuel);
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

    /// Vanilla `MinecartFurnace.interact`.
    fn interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        _location: DVec3,
    ) -> InteractionResult {
        let item = {
            let inventory = player.inventory.lock();
            let item = inventory.get_item_in_hand(hand);
            item.copy_with_count(item.count())
        };
        if self.add_fuel(player.position(), &item) && !player.has_infinite_materials() {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
        }
        InteractionResult::Success
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

    /// Vanilla `MinecartFurnace.addAdditionalSaveData`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_minecart(nbt);
        let furnace = *self.furnace.lock();
        nbt.insert("PushX", furnace.push.x);
        nbt.insert("PushZ", furnace.push.z);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vanilla saves the fuel as a short; it never exceeds 32000"
        )]
        nbt.insert("Fuel", furnace.fuel as i16);
    }

    /// Vanilla `MinecartFurnace.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
        let mut furnace = self.furnace.lock();
        furnace.push = DVec3::new(
            nbt.double("PushX").unwrap_or(0.0),
            0.0,
            nbt.double("PushZ").unwrap_or(0.0),
        );
        furnace.fuel = i32::from(nbt.short("Fuel").unwrap_or(0));
    }
}
