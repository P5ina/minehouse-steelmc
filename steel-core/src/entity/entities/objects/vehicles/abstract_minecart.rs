//! Shared minecart state and behavior (vanilla `VehicleEntity` and `AbstractMinecart`).
//!
//! Steel has no experimental feature flags, so every minecart moves with vanilla's
//! default `OldMinecartBehavior`; `NewMinecartBehavior` belongs to the
//! `minecart_improvements` experiment and is not ported.

use std::sync::Arc;

use glam::{DVec3, IVec3};
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_math::trig;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, RailShape};
use steel_registry::data_components::vanilla_components::CUSTOM_NAME;
use steel_registry::entity_data::EntityPose;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::AbstractMinecartEntityData;
use steel_registry::vanilla_game_rules::{ENTITY_DROPS, MOB_GRIEFING};
use steel_registry::{vanilla_blocks, vanilla_entities, vanilla_game_events};
use steel_utils::axis::Axis;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, Direction};

use crate::behavior::blocks::is_rail_state;
use crate::block_entity::block_state_nbt;
use crate::entity::damage::DamageSource;
use crate::entity::{Entity, RemovalReason, dismount_helper};
use crate::physics::MoverType;
use crate::world::is_redstone_conductor;
use crate::world::{Explosion, World};

/// Vanilla `AbstractMinecart.WATER_SLOWDOWN_FACTOR`.
pub(super) const WATER_SLOWDOWN_FACTOR: f64 = 0.95_f32 as f64;
/// Vanilla `AbstractMinecart.getAirDrag`.
const AIR_DRAG: f64 = 0.95_f32 as f64;
/// Vanilla `VehicleEntity` hurt-wobble duration in ticks.
const HURT_TIME: i32 = 10;
/// Accumulated damage above which a vehicle breaks.
const BREAK_DAMAGE: f32 = 40.0;

/// Per-cart movement state that is not synchronized to clients.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct MinecartState {
    on_rails: bool,
    flipped: bool,
}

/// State every minecart carries next to its synchronized entity data.
#[derive(Debug, Default)]
pub struct MinecartCore {
    state: SyncMutex<MinecartState>,
}

impl MinecartCore {
    /// Creates a fresh minecart core.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Behavior shared by every minecart (vanilla `AbstractMinecart`).
///
/// Concrete minecarts implement the accessors and per-type hooks; the provided methods
/// carry vanilla's shared logic.
pub trait AbstractMinecart: Entity {
    /// The cart's unsynchronized movement state.
    fn minecart_core(&self) -> &MinecartCore;

    /// Runs `f` on the cart's synchronized `AbstractMinecart` data layer.
    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData));

    /// The item dropped when this cart breaks (vanilla `getDropItem`).
    fn drop_item(&self) -> ItemRef;

    /// Whether mobs walking into this cart get in (vanilla `isRideable`).
    fn is_rideable(&self) -> bool {
        false
    }

    /// Whether this is a furnace minecart, which pushes other carts.
    fn is_furnace(&self) -> bool {
        false
    }

    /// Reacts to an activator rail under the cart (vanilla `activateMinecart`).
    fn activate_minecart(&self, _world: &Arc<World>, _pos: BlockPos, _powered: bool) {}

    /// Whether the cart is on a rail this tick.
    fn minecart_on_rails(&self) -> bool {
        self.minecart_core().state.lock().on_rails
    }

    /// Records whether the cart is on a rail this tick.
    fn set_on_rails(&self, on_rails: bool) {
        self.minecart_core().state.lock().on_rails = on_rails;
    }

    /// Whether the cart faces the opposite way from its travel direction.
    fn is_flipped(&self) -> bool {
        self.minecart_core().state.lock().flipped
    }

    /// Sets whether the cart faces the opposite way from its travel direction.
    fn set_flipped(&self, flipped: bool) {
        self.minecart_core().state.lock().flipped = flipped;
    }

    /// Ticks left on the hurt wobble.
    fn hurt_time(&self) -> i32 {
        let mut value = 0;
        self.with_minecart_data(&mut |data| value = *data.vehicle_entity.id_hurt.get());
        value
    }

    /// Sets the hurt wobble.
    fn set_hurt_time(&self, hurt_time: i32) {
        self.with_minecart_data(&mut |data| data.vehicle_entity.id_hurt.set(hurt_time));
    }

    /// Direction of the hurt wobble, `1` or `-1`.
    fn hurt_dir(&self) -> i32 {
        let mut value = 0;
        self.with_minecart_data(&mut |data| value = *data.vehicle_entity.id_hurtdir.get());
        value
    }

    /// Sets the direction of the hurt wobble.
    fn set_hurt_dir(&self, hurt_dir: i32) {
        self.with_minecart_data(&mut |data| data.vehicle_entity.id_hurtdir.set(hurt_dir));
    }

    /// Accumulated damage; it decays by one each tick.
    fn vehicle_damage(&self) -> f32 {
        let mut value = 0.0;
        self.with_minecart_data(&mut |data| value = *data.vehicle_entity.id_damage.get());
        value
    }

    /// Sets the accumulated damage.
    fn set_vehicle_damage(&self, damage: f32) {
        self.with_minecart_data(&mut |data| data.vehicle_entity.id_damage.set(damage));
    }

    /// The display block chosen with `DisplayState`, if any.
    fn custom_display_block_state(&self) -> Option<BlockStateId> {
        let mut value = None;
        self.with_minecart_data(&mut |data| value = *data.id_custom_display_block.get());
        value
    }

    /// Sets the custom display block.
    fn set_custom_display_block_state(&self, state: Option<BlockStateId>) {
        self.with_minecart_data(&mut |data| data.id_custom_display_block.set(state));
    }

    /// The default display-block offset in pixels.
    fn default_display_offset(&self) -> i32 {
        6
    }

    /// The display-block offset in pixels.
    fn display_offset(&self) -> i32 {
        let mut value = 0;
        self.with_minecart_data(&mut |data| value = *data.id_display_offset.get());
        value
    }

    /// Sets the display-block offset.
    fn set_display_offset(&self, offset: i32) {
        self.with_minecart_data(&mut |data| data.id_display_offset.set(offset));
    }

    /// The rail block the cart sits on: its own block, or the one below it.
    fn current_block_pos_or_rail_below(&self) -> BlockPos {
        let position = self.position();
        let pos = BlockPos::containing(position.x, position.y, position.z);
        let Some(world) = self.level() else {
            return pos;
        };
        if world
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::RAILS)
        {
            return pos.below();
        }
        pos
    }

    /// Top speed per tick (vanilla `AbstractMinecart.getMaxSpeed`).
    fn max_speed(&self) -> f64 {
        self.default_max_speed()
    }

    /// Vanilla `OldMinecartBehavior.getMaxSpeed`, for overrides that call `super`.
    fn default_max_speed(&self) -> f64 {
        if self.is_in_water() { 0.2 } else { 0.4 }
    }

    /// Per-tick speed retention while on rails.
    fn slowdown_factor(&self) -> f64 {
        if self.is_vehicle() { 0.997 } else { 0.96 }
    }

    /// Vanilla `applyNaturalSlowdown`.
    fn apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        self.default_apply_natural_slowdown(movement)
    }

    /// The base `applyNaturalSlowdown` body, for overrides that call `super`.
    fn default_apply_natural_slowdown(&self, movement: DVec3) -> DVec3 {
        let factor = self.slowdown_factor();
        let slowed = DVec3::new(movement.x * factor, 0.0, movement.z * factor);
        if self.is_in_water() {
            slowed * WATER_SLOWDOWN_FACTOR
        } else {
            slowed
        }
    }

    /// Vanilla `isRedstoneConductor` for the block at `pos`.
    fn is_redstone_conductor_at(&self, pos: BlockPos) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        is_redstone_conductor(world.as_ref(), world.get_block_state(pos), pos)
    }

    /// Vanilla `VehicleEntity.hurtServer`.
    fn hurt_vehicle(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_removed() {
            return true;
        }
        if self.is_invulnerable_to_base(source) {
            return false;
        }

        self.set_hurt_dir(-self.hurt_dir());
        self.set_hurt_time(HURT_TIME);
        self.mark_hurt();
        self.set_vehicle_damage(self.vehicle_damage() + amount * 10.0);
        self.game_event_with_source_entity(
            &vanilla_game_events::ENTITY_DAMAGE,
            source.causing_entity().map(AsRef::as_ref),
        );

        let creative_player = source.is_creative_player();
        if creative_player || self.vehicle_damage() > BREAK_DAMAGE {
            if creative_player {
                self.set_removed(RemovalReason::Discarded);
            } else {
                self.destroy(world, source);
            }
        }
        true
    }

    /// Vanilla `VehicleEntity.destroy`: kills the cart and drops its item.
    fn destroy(&self, world: &World, _source: &DamageSource) {
        self.default_destroy(world);
    }

    /// The base `destroy` body, for overrides that call `super.destroy`.
    fn default_destroy(&self, world: &World) {
        self.kill(world);
        if !world.get_game_rule(&ENTITY_DROPS) {
            return;
        }
        let mut item = ItemStack::new(self.drop_item());
        if let Some(name) = self.custom_name() {
            item.set(CUSTOM_NAME, name);
        }
        let _ = self.spawn_at_location(item, 0.0);
    }

    /// Vanilla `VehicleEntity.ignoreExplosion`.
    fn vehicle_ignores_explosion(&self, explosion: &dyn Explosion) -> bool {
        !explosion.world().get_game_rule(&MOB_GRIEFING)
            && explosion
                .indirect_source_entity()
                .is_some_and(|entity| entity.as_mob().is_some())
    }

    /// Vanilla `AbstractMinecart.getRedstoneDirection`: a powered straight rail next to
    /// a solid block pushes the cart away from it.
    fn redstone_direction(&self, pos: BlockPos) -> DVec3 {
        let Some(world) = self.level() else {
            return DVec3::ZERO;
        };
        let state = world.get_block_state(pos);
        if state.get_block() != &vanilla_blocks::POWERED_RAIL
            || !state.get_value(&BlockStateProperties::POWERED)
        {
            return DVec3::ZERO;
        }
        match state.get_value(&BlockStateProperties::RAIL_SHAPE) {
            RailShape::EastWest if self.is_redstone_conductor_at(pos.west()) => DVec3::X,
            RailShape::EastWest if self.is_redstone_conductor_at(pos.east()) => DVec3::NEG_X,
            RailShape::NorthSouth if self.is_redstone_conductor_at(pos.north()) => DVec3::Z,
            RailShape::NorthSouth if self.is_redstone_conductor_at(pos.south()) => DVec3::NEG_Z,
            _ => DVec3::ZERO,
        }
    }

    /// Vanilla `AbstractMinecart.getMotionDirection` under `OldMinecartBehavior`.
    fn minecart_motion_direction(&self) -> Direction {
        let facing = self.direction_yaw();
        if self.is_flipped() {
            facing.opposite().rotate_y_clockwise()
        } else {
            facing.rotate_y_clockwise()
        }
    }

    /// Vanilla `AbstractMinecart.getDismountLocationForPassenger`.
    fn minecart_dismount_location(&self, passenger: &dyn Entity) -> DVec3 {
        let forward = self.minecart_motion_direction();
        if forward.get_axis() == Axis::Y {
            return self.default_dismount_location_for_passenger(passenger);
        }
        let Some(world) = self.level() else {
            return self.default_dismount_location_for_passenger(passenger);
        };
        let vehicle_pos = self.block_position();
        let poses = dismount_helper::dismount_poses(passenger);
        for &pose in poses {
            let dimensions = passenger.dimensions_for_pose(pose);
            let reach = f64::from(dimensions.width.min(1.0) / 2.0);
            for &offset_y in pose_dismount_heights(pose) {
                for (offset_x, offset_z) in dismount_helper::offsets_for_direction(forward) {
                    let target = BlockPos::new(
                        vehicle_pos.x() + offset_x,
                        vehicle_pos.y() + offset_y,
                        vehicle_pos.z() + offset_z,
                    );
                    let floor_height = dismount_helper::block_floor_height(
                        dismount_helper::non_climbable_shape(&world, target),
                        dismount_helper::non_climbable_shape(&world, target.below()),
                    );
                    if !dismount_helper::is_block_floor_valid(floor_height) {
                        continue;
                    }
                    let location = dismount_helper::up_from_bottom_center_of(target, floor_height);
                    let dismount_box = steel_utils::WorldAabb::new(
                        -reach,
                        0.0,
                        -reach,
                        reach,
                        f64::from(dimensions.height),
                        reach,
                    )
                    .translate(location);
                    if dismount_helper::can_passenger_dismount_to(&world, passenger, &dismount_box)
                    {
                        passenger.set_pose(pose);
                        return location;
                    }
                }
            }
        }

        // No free spot beside the track: pick the first pose that fits under the ceiling
        // above the cart, then fall back to the cart's top.
        let vehicle_top = self.bounding_box().max_y();
        let column = BlockPos::containing(
            f64::from(vehicle_pos.x()),
            vehicle_top,
            f64::from(vehicle_pos.z()),
        );
        for &pose in poses {
            let pose_height = f64::from(passenger.dimensions_for_pose(pose).height);
            let block_coverage = (vehicle_top - f64::from(column.y()) + pose_height).ceil() as i32;
            let ceiling = dismount_helper::find_ceiling_from(column, block_coverage, |pos| {
                dismount_helper::collision_shape(&world, pos)
            });
            if vehicle_top + pose_height <= ceiling {
                passenger.set_pose(pose);
                break;
            }
        }
        self.default_dismount_location_for_passenger(passenger)
    }

    /// Vanilla `AbstractMinecart.push(Entity)`: carts push each other along the track;
    /// other entities get a small shove and push the cart back.
    fn push_minecart_entity(&self, entity: &dyn Entity) {
        if entity.no_physics() || self.no_physics() || self.has_passenger(entity) {
            return;
        }
        let mut xa = entity.position().x - self.position().x;
        let mut za = entity.position().z - self.position().z;
        let mut distance = xa * xa + za * za;
        if distance < f64::from(1.0e-4_f32) {
            return;
        }
        distance = distance.sqrt();
        xa /= distance;
        za /= distance;
        let pow = (1.0 / distance).min(1.0);
        xa *= pow;
        za *= pow;
        xa *= f64::from(0.1_f32);
        za *= f64::from(0.1_f32);
        xa *= 0.5;
        za *= 0.5;

        if let Some(other) = entity.as_abstract_minecart() {
            self.push_other_minecart(other, xa, za);
        } else {
            self.push_impulse(DVec3::new(-xa, 0.0, -za));
            entity.push_impulse(DVec3::new(xa / 4.0, 0.0, za / 4.0));
        }
    }

    /// Vanilla `AbstractMinecart.pushOtherMinecart` under `OldMinecartBehavior`.
    fn push_other_minecart(&self, other: &dyn AbstractMinecart, xa: f64, za: f64) {
        let offset = other.position() - self.position();
        let direction = DVec3::new(offset.x, 0.0, offset.z).normalize_or_zero();
        let yaw_radians = self.rotation().0 * steel_math::DEG_TO_RAD;
        let facing = DVec3::new(
            f64::from(trig::cos(yaw_radians)),
            0.0,
            f64::from(trig::sin(yaw_radians)),
        )
        .normalize_or_zero();
        if direction.dot(facing).abs() < f64::from(0.8_f32) {
            return;
        }

        let movement = self.velocity();
        let other_movement = other.velocity();
        let keep = DVec3::new(0.2, 1.0, 0.2);
        if other.is_furnace() && !self.is_furnace() {
            self.set_velocity(movement * keep);
            self.push_impulse(DVec3::new(
                other_movement.x - xa,
                0.0,
                other_movement.z - za,
            ));
            other.set_velocity(other_movement * DVec3::new(0.95, 1.0, 0.95));
        } else if !other.is_furnace() && self.is_furnace() {
            other.set_velocity(other_movement * keep);
            other.push_impulse(DVec3::new(movement.x + xa, 0.0, movement.z + za));
            self.set_velocity(movement * DVec3::new(0.95, 1.0, 0.95));
        } else {
            let x_average = f64::midpoint(other_movement.x, movement.x);
            let z_average = f64::midpoint(other_movement.z, movement.z);
            self.set_velocity(movement * keep);
            self.push_impulse(DVec3::new(x_average - xa, 0.0, z_average - za));
            other.set_velocity(other_movement * keep);
            other.push_impulse(DVec3::new(x_average + xa, 0.0, z_average + za));
        }
    }

    /// Vanilla `AbstractMinecart.getPassengerAttachmentPoint`: villagers and wandering
    /// traders sit lower.
    fn minecart_passenger_attachment_point(&self, passenger: &dyn Entity) -> Option<DVec3> {
        let lowered = passenger.entity_type() == &vanilla_entities::VILLAGER
            || passenger.entity_type() == &vanilla_entities::WANDERING_TRADER;
        lowered.then_some(DVec3::ZERO)
    }

    /// Vanilla `AbstractMinecart.getBlockSpeedFactor`: rails never slow the cart.
    fn minecart_block_speed_factor(&self) -> f32 {
        let on_rail = self.level().is_some_and(|world| {
            world
                .get_block_state(self.block_position())
                .get_block()
                .has_tag(&BlockTag::RAILS)
        });
        if on_rail {
            1.0
        } else {
            self.default_block_speed_factor()
        }
    }

    /// Vanilla `AbstractMinecart.getDefaultGravity`.
    fn minecart_gravity(&self) -> f64 {
        if self.is_in_water() { 0.005 } else { 0.04 }
    }

    /// Vanilla `AbstractMinecart.addAdditionalSaveData`.
    fn save_minecart(&self, nbt: &mut NbtCompound) {
        if let Some(state) = self.custom_display_block_state() {
            nbt.insert("DisplayState", block_state_nbt::save(state));
        }
        let display_offset = self.display_offset();
        if display_offset != self.default_display_offset() {
            nbt.insert("DisplayOffset", display_offset);
        }
        nbt.insert("FlippedRotation", i8::from(self.is_flipped()));
        nbt.insert("HasTicked", i8::from(self.is_first_tick()));
    }

    /// Vanilla `AbstractMinecart.readAdditionalSaveData`.
    fn load_minecart(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let display_state = nbt.compound("DisplayState").and_then(block_state_nbt::load);
        self.set_custom_display_block_state(display_state);
        self.set_display_offset(
            nbt.int("DisplayOffset")
                .unwrap_or_else(|| self.default_display_offset()),
        );
        self.set_flipped(nbt.byte("FlippedRotation").is_some_and(|value| value != 0));
        self.set_first_tick(nbt.byte("HasTicked").is_some_and(|value| value != 0));
    }
}

/// Vanilla `AbstractMinecart.POSE_DISMOUNT_HEIGHTS`.
const fn pose_dismount_heights(pose: EntityPose) -> &'static [i32] {
    match pose {
        EntityPose::Swimming => &[0, 1],
        _ => &[0, 1, -1],
    }
}

/// Vanilla `AbstractMinecart.EXITS`: the two block offsets a rail shape connects.
#[must_use]
pub(super) const fn exits(shape: RailShape) -> (IVec3, IVec3) {
    const WEST: IVec3 = IVec3::new(-1, 0, 0);
    const EAST: IVec3 = IVec3::new(1, 0, 0);
    const NORTH: IVec3 = IVec3::new(0, 0, -1);
    const SOUTH: IVec3 = IVec3::new(0, 0, 1);
    const WEST_BELOW: IVec3 = IVec3::new(-1, -1, 0);
    const EAST_BELOW: IVec3 = IVec3::new(1, -1, 0);
    const NORTH_BELOW: IVec3 = IVec3::new(0, -1, -1);
    const SOUTH_BELOW: IVec3 = IVec3::new(0, -1, 1);
    match shape {
        RailShape::NorthSouth => (NORTH, SOUTH),
        RailShape::EastWest => (WEST, EAST),
        RailShape::AscendingEast => (WEST_BELOW, EAST),
        RailShape::AscendingWest => (WEST, EAST_BELOW),
        RailShape::AscendingNorth => (NORTH, SOUTH_BELOW),
        RailShape::AscendingSouth => (NORTH_BELOW, SOUTH),
        RailShape::SouthEast => (SOUTH, EAST),
        RailShape::SouthWest => (SOUTH, WEST),
        RailShape::NorthWest => (NORTH, WEST),
        RailShape::NorthEast => (NORTH, EAST),
    }
}

/// Vanilla `BaseRailBlock.isRail(BlockState)`.
#[must_use]
pub(super) fn is_rail(state: BlockStateId) -> bool {
    is_rail_state(state)
}

/// Vanilla `AbstractMinecart.comeOffTrack`.
pub(super) fn come_off_track<M: AbstractMinecart>(minecart: &Arc<M>) {
    let max_speed = minecart.max_speed();
    let movement = minecart.velocity();
    minecart.set_velocity(DVec3::new(
        movement.x.clamp(-max_speed, max_speed),
        movement.y,
        movement.z.clamp(-max_speed, max_speed),
    ));
    if minecart.on_ground() {
        minecart.set_velocity(minecart.velocity() * 0.5);
    }

    Arc::clone(minecart).move_entity(MoverType::SelfMovement, minecart.velocity());
    if !minecart.on_ground() {
        minecart.set_velocity(minecart.velocity() * AIR_DRAG);
    }
}

/// Vanilla `AbstractMinecart.tick`.
pub(super) fn tick_minecart<M: AbstractMinecart>(minecart: &Arc<M>) {
    if minecart.hurt_time() > 0 {
        minecart.set_hurt_time(minecart.hurt_time() - 1);
    }
    if minecart.vehicle_damage() > 0.0 {
        minecart.set_vehicle_damage(minecart.vehicle_damage() - 1.0);
    }

    minecart.check_below_world();
    minecart.base().compute_known_speed();
    minecart.handle_portal();
    super::old_minecart_behavior::tick(minecart);
    minecart.update_fluid_interaction();
    if minecart.is_in_lava() {
        minecart.lava_ignite();
        minecart.lava_hurt();
        minecart.set_fall_distance(minecart.fall_distance() * 0.5);
    }
    minecart.set_first_tick(false);
}
