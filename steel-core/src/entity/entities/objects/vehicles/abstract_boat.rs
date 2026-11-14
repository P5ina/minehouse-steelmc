//! Behavior shared by boats and rafts (vanilla `AbstractBoat`).

use std::f32::consts::{PI, SQRT_2};
use std::sync::Arc;

use glam::DVec3;
use rand::RngExt as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_math::{DEG_TO_RAD, vector::y_rot, wrap_degrees};
use steel_registry::REGISTRY;
use steel_registry::TaggedRegistryExt as _;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityDimensions;
use steel_registry::fluid::FluidStateExt as _;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::sound_events;
use steel_registry::vanilla_blocks;
use steel_registry::vanilla_entity_data::AbstractBoatEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_game_events;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Direction, WorldAabb};

use super::vehicle_entity::VehicleEntity;
use crate::behavior::InteractionResult;
use crate::entity::dismount_helper::{
    block_floor_height_at, can_dismount_to_pose, collision_horizontal_escape_vector,
    collision_shape, dismount_poses, is_block_floor_valid,
};
use crate::entity::leash::{LeashData, Leashable};
use crate::entity::physics_state_for_move;
use crate::entity::selector::EntitySelector;
use crate::entity::{Entity, EntityStatus, RemovalReason, SharedEntity, position_rider_default};
use crate::fluid::{get_fluid_state, get_height};
use crate::physics::{CollisionWorld as _, MoverType, WorldCollisionProvider};
use crate::player::Player;
use crate::world::World;

/// Vanilla `AbstractBoat.PADDLE_LEFT`.
pub const PADDLE_LEFT: usize = 0;
/// Vanilla `AbstractBoat.PADDLE_RIGHT`.
pub const PADDLE_RIGHT: usize = 1;
/// Ticks spent under water before passengers are thrown out.
const TIME_TO_EJECT: f32 = 60.0;
/// How far a paddle turns each tick (vanilla `PADDLE_SPEED`, π/8).
const PADDLE_SPEED: f32 = PI / 8.0;
/// The paddle angle at which the stroke sound plays (vanilla `PADDLE_SOUND_TIME`, π/4).
const PADDLE_SOUND_TIME: f32 = PI / 4.0;
/// Ticks a bubble column keeps acting on the boat (vanilla `BUBBLE_TIME`).
pub const BUBBLE_TIME: i32 = 60;
/// Vanilla `AbstractBoat.getDefaultGravity`.
const BOAT_GRAVITY: f64 = 0.04;

/// Where the boat is relative to water (vanilla `AbstractBoat.Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoatStatus {
    /// Floating on water.
    InWater,
    /// Fully under still water.
    UnderWater,
    /// Fully under flowing water.
    UnderFlowingWater,
    /// Resting on blocks.
    OnLand,
    /// Neither in water nor on land.
    InAir,
}

/// Server-side state vanilla keeps in `AbstractBoat` fields.
#[derive(Debug, Clone)]
struct BoatState {
    paddle_positions: [f32; 2],
    out_of_control_ticks: f32,
    delta_rotation: f32,
    water_level: f64,
    land_friction: f32,
    status: Option<BoatStatus>,
    old_status: Option<BoatStatus>,
    last_yd: f64,
    is_above_bubble_column: bool,
    bubble_column_direction_is_down: bool,
}

/// State shared by every boat and raft.
pub struct BoatCore {
    state: SyncMutex<BoatState>,
    leash_data: SyncMutex<Option<LeashData>>,
}

impl Default for BoatCore {
    fn default() -> Self {
        Self::new()
    }
}

impl BoatCore {
    /// Creates the state for a freshly built boat.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: SyncMutex::new(BoatState {
                paddle_positions: [0.0; 2],
                out_of_control_ticks: 0.0,
                delta_rotation: 0.0,
                water_level: 0.0,
                land_friction: 0.0,
                status: None,
                old_status: None,
                last_yd: 0.0,
                is_above_bubble_column: false,
                bubble_column_direction_is_down: false,
            }),
            leash_data: SyncMutex::new(None),
        }
    }

    /// The boat's leash data.
    #[must_use]
    pub const fn leash_data(&self) -> &SyncMutex<Option<LeashData>> {
        &self.leash_data
    }
}

/// Vanilla `AbstractBoat`: the behavior every boat and raft shares.
pub trait AbstractBoat: VehicleEntity + Leashable {
    /// The boat's shared state.
    fn boat_core(&self) -> &BoatCore;

    /// Runs `f` on the boat's synchronized data.
    fn with_boat_data(&self, f: &mut dyn FnMut(&mut AbstractBoatEntityData));

    /// Vanilla `rideHeight`: how high passengers sit.
    fn ride_height(&self, dimensions: &EntityDimensions) -> f64;

    /// Vanilla `getSinglePassengerXOffset`.
    fn single_passenger_x_offset(&self) -> f32 {
        0.0
    }

    /// Vanilla `getMaxPassengers`.
    fn max_passengers(&self) -> usize {
        2
    }

    /// Vanilla `HasCustomInventoryScreen.openCustomInventoryScreen`; only
    /// chest boats have one.
    fn open_custom_inventory_screen(&self, _player: &Player) {}

    /// Vanilla `setPaddleState`.
    fn set_paddle_state(&self, left: bool, right: bool) {
        self.with_boat_data(&mut |data| {
            data.id_paddle_left.set(left);
            data.id_paddle_right.set(right);
        });
    }

    /// Vanilla `getPaddleState`: a paddle only moves while someone steers.
    fn paddle_state(&self, side: usize) -> bool {
        let mut value = false;
        self.with_boat_data(&mut |data| {
            value = if side == PADDLE_LEFT {
                *data.id_paddle_left.get()
            } else {
                *data.id_paddle_right.get()
            };
        });
        value && self.controlling_passenger().is_some()
    }

    /// Vanilla `getBubbleTime`.
    fn bubble_time(&self) -> i32 {
        let mut value = 0;
        self.with_boat_data(&mut |data| value = *data.id_bubble_time.get());
        value
    }

    /// Vanilla `setBubbleTime`.
    fn set_bubble_time(&self, bubble_time: i32) {
        self.with_boat_data(&mut |data| data.id_bubble_time.set(bubble_time));
    }

    /// The status computed at the start of the last tick.
    fn boat_status(&self) -> Option<BoatStatus> {
        self.boat_core().state.lock().status
    }

    /// Vanilla `AbstractBoat.isUnderWater`.
    fn boat_is_under_water(&self) -> bool {
        matches!(
            self.boat_status(),
            Some(BoatStatus::UnderWater | BoatStatus::UnderFlowingWater)
        )
    }

    /// Vanilla `hasEnoughSpaceFor`.
    fn has_enough_space_for(&self, entity: &dyn Entity) -> bool {
        entity.base().dimensions().width < self.base().dimensions().width
    }

    /// Vanilla `AbstractBoat.getPassengerAttachmentPoint`.
    fn boat_passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
        let passengers = self.passengers();
        let mut offset = self.single_passenger_x_offset();
        if passengers.len() > 1 {
            let index = passengers
                .iter()
                .position(|entity| entity.id() == passenger.id());
            offset = if index == Some(0) { 0.2 } else { -0.6 };
            if passenger.as_animal().is_some() {
                offset += 0.2;
            }
        }
        let dimensions = self.base().dimensions();
        y_rot(
            DVec3::new(0.0, self.ride_height(&dimensions), f64::from(offset)),
            -self.rotation().0 * DEG_TO_RAD,
        )
    }

    /// Vanilla `AbstractBoat.push`: boats only shove what sits low enough.
    fn boat_push_entity(&self, entity: &dyn Entity) {
        let own_box = self.bounding_box();
        let other_box = entity.bounding_box();
        if entity.as_abstract_boat().is_some() {
            if other_box.min_y() < own_box.max_y() {
                self.default_push_entity(entity);
            }
        } else if other_box.min_y() <= own_box.min_y() {
            self.default_push_entity(entity);
        }
    }

    /// Vanilla `AbstractBoat.onAboveBubbleColumn`.
    fn boat_on_above_bubble_column(&self, drag_down: bool) {
        {
            let mut state = self.boat_core().state.lock();
            state.is_above_bubble_column = true;
            state.bubble_column_direction_is_down = drag_down;
        }
        if self.bubble_time() == 0 {
            self.set_bubble_time(BUBBLE_TIME);
        }

        // VANILLA CLIENT-LOCAL: the splash sound and particle; the server
        // keeps the random roll and the game event.
        if !self.boat_is_under_water() && rand::rng().random_range(0..100) == 0 {
            let controller = self.controlling_passenger();
            self.game_event_with_source_entity(&vanilla_game_events::SPLASH, controller.as_deref());
        }
    }

    /// Vanilla `handleBubbleColumnEffect`.
    fn handle_bubble_column_effect(&self, drag_down: bool) {
        if !self.can_simulate_movement() {
            return;
        }
        let movement = self.velocity();
        if drag_down {
            self.set_velocity(movement + DVec3::new(0.0, -0.7, 0.0));
        } else {
            self.set_velocity(DVec3::new(movement.x, 0.6, movement.z));
        }
    }

    /// Vanilla `getPaddleSound`, which recomputes the status like vanilla does.
    fn paddle_sound(&self) -> Option<SoundEventRef> {
        match compute_status(self) {
            BoatStatus::InWater | BoatStatus::UnderWater | BoatStatus::UnderFlowingWater => {
                Some(&sound_events::ENTITY_BOAT_PADDLE_WATER)
            }
            BoatStatus::OnLand => Some(&sound_events::ENTITY_BOAT_PADDLE_LAND),
            BoatStatus::InAir => None,
        }
    }

    /// Vanilla `AbstractBoat.positionRider`.
    fn boat_position_rider(&self, passenger: &dyn Entity) {
        position_rider_default(self, passenger);
        let turns_in_boats = REGISTRY
            .entity_types
            .is_in_tag(passenger.entity_type(), &EntityTypeTag::CAN_TURN_IN_BOATS);
        if turns_in_boats {
            return;
        }
        if passenger.is_server_driven_movement() {
            let delta_rotation = self.boat_core().state.lock().delta_rotation;
            let (y_rot, x_rot) = passenger.rotation();
            passenger.set_rotation((y_rot + delta_rotation, x_rot));
            if let Some(living) = passenger.as_living_entity() {
                living.set_y_head_rot(living.y_head_rot() + delta_rotation);
            }
        }
        self.clamp_rotation(passenger);
    }

    /// Vanilla `clampRotation`.
    fn clamp_rotation(&self, passenger: &dyn Entity) -> f32 {
        let passenger_body_y_rot = self.calculate_passenger_body_y_rot(passenger);
        if let Some(living) = passenger.as_living_entity() {
            living.set_y_body_rot(passenger_body_y_rot);
        }
        let (y_rot, x_rot) = passenger.rotation();
        if let Some(mob) = passenger.as_mob() {
            passenger.set_rotation((passenger_body_y_rot, x_rot));
            mob.clamp_head_rotation_to_body();
            return wrap_degrees(passenger_body_y_rot - passenger.rotation().0);
        }
        let delta = wrap_degrees(y_rot - passenger_body_y_rot);
        let target_delta = delta.clamp(-105.0, 105.0);
        passenger.set_rotation((y_rot + target_delta - delta, x_rot));
        target_delta - delta
    }

    /// Vanilla `calculatePassengerBodyYRot`: animals sit sideways in a full boat.
    fn calculate_passenger_body_y_rot(&self, passenger: &dyn Entity) -> f32 {
        let y_rot = self.rotation().0;
        if passenger.as_animal().is_some() && self.passengers().len() == self.max_passengers() {
            y_rot + if passenger.id() % 2 == 0 { 90.0 } else { 270.0 }
        } else {
            y_rot
        }
    }

    /// Vanilla `AbstractBoat.getDismountLocationForPassenger`.
    fn boat_dismount_location(&self, passenger: &dyn Entity) -> DVec3 {
        if passenger.as_living_entity().is_some()
            && let Some(world) = self.level()
            && let Some(location) = self.boat_dismount_target(&world, passenger)
        {
            return location;
        }
        self.default_dismount_location_for_passenger(passenger)
    }

    /// The dry dismount spot beside the boat, if there is one.
    fn boat_dismount_target(&self, world: &Arc<World>, passenger: &dyn Entity) -> Option<DVec3> {
        let width = self.base().dimensions().width * SQRT_2;
        let direction = collision_horizontal_escape_vector(
            f64::from(width),
            f64::from(passenger.base().dimensions().width),
            passenger.rotation().0,
        );
        let position = self.position();
        let target_x = position.x + direction.x;
        let target_z = position.z + direction.z;
        let target_block_pos =
            BlockPos::containing(target_x, self.bounding_box().max_y(), target_z);
        let below_block_pos = target_block_pos.below();
        if get_fluid_state(world, below_block_pos).is_water() {
            return None;
        }

        let mut targets = Vec::with_capacity(2);
        let target_floor = block_floor_height_at(world, target_block_pos);
        if is_block_floor_valid(target_floor) {
            targets.push(DVec3::new(
                target_x,
                f64::from(target_block_pos.y()) + target_floor,
                target_z,
            ));
        }
        let below_floor = block_floor_height_at(world, below_block_pos);
        if is_block_floor_valid(below_floor) {
            targets.push(DVec3::new(
                target_x,
                f64::from(below_block_pos.y()) + below_floor,
                target_z,
            ));
        }

        for &dismount_pose in dismount_poses(passenger) {
            for &target in &targets {
                if can_dismount_to_pose(world, target, passenger, dismount_pose) {
                    passenger.set_pose(dismount_pose);
                    return Some(target);
                }
            }
        }
        None
    }

    /// Vanilla `AbstractBoat.canAddPassenger`.
    fn boat_can_add_passenger(&self) -> bool {
        self.passengers().len() < self.max_passengers() && !self.is_eye_in_water()
    }

    /// Vanilla `AbstractBoat.getControllingPassenger`: only a player steers.
    fn boat_controlling_passenger(&self) -> Option<SharedEntity> {
        self.first_passenger()
            .filter(|passenger| passenger.as_player().is_some())
    }

    /// Vanilla `AbstractBoat.checkFallDamage`: boats never take fall damage.
    fn boat_check_fall_damage(&self, vertical_movement: f64, on_ground: bool, world: &Arc<World>) {
        self.boat_core().state.lock().last_yd = self.velocity().y;
        if self.is_passenger() {
            return;
        }
        if on_ground {
            self.reset_fall_distance();
        } else if !get_fluid_state(world, self.block_position().below()).is_water()
            && vertical_movement < 0.0
        {
            self.set_fall_distance(self.fall_distance() - f64::from(vertical_movement as f32));
        }
    }

    /// Vanilla `AbstractBoat.modifyPassengerFluidInteractionBox`.
    fn boat_modify_passenger_fluid_interaction_box(
        &self,
        passenger_box: WorldAabb,
    ) -> Option<WorldAabb> {
        if self.boat_is_under_water() {
            return Some(passenger_box);
        }
        let boat_box = self.bounding_box();
        if boat_box.max_y() >= passenger_box.max_y() {
            return None;
        }
        let min_y = passenger_box.min_y().max(boat_box.max_y());
        Some(WorldAabb::new(
            passenger_box.min_x(),
            min_y,
            passenger_box.min_z(),
            passenger_box.max_x(),
            passenger_box.max_y(),
            passenger_box.max_z(),
        ))
    }

    /// Vanilla `AbstractBoat.interact`.
    fn boat_interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        location: DVec3,
    ) -> InteractionResult {
        let super_interaction = self.interact_entity(player, hand, location);
        if super_interaction != InteractionResult::Pass {
            return super_interaction;
        }
        if player.is_secondary_use_active()
            || self.boat_core().state.lock().out_of_control_ticks >= TIME_TO_EJECT
        {
            return InteractionResult::Pass;
        }
        let Some(boat) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return InteractionResult::Pass;
        };
        if player.start_riding(&boat) {
            InteractionResult::Success
        } else {
            InteractionResult::Pass
        }
    }

    /// Vanilla `AbstractBoat.remove`: a destroyed boat drops its lead.
    fn boat_removed(&self, reason: RemovalReason) {
        if matches!(reason, RemovalReason::Killed | RemovalReason::Discarded) && self.is_leashed() {
            self.drop_leash();
        }
    }

    /// Vanilla `AbstractBoat.addAdditionalSaveData`.
    fn save_boat(&self, nbt: &mut NbtCompound) {
        if let Some(leash_data) = self.boat_core().leash_data.lock().as_ref() {
            leash_data.save(nbt);
        }
    }

    /// Vanilla `AbstractBoat.readAdditionalSaveData`.
    fn load_boat(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        *self.boat_core().leash_data.lock() = LeashData::load(nbt);
    }

    /// Vanilla `AbstractBoat.getMotionDirection`.
    fn boat_motion_direction(&self) -> Direction {
        self.direction_yaw().rotate_y_clockwise()
    }
}

/// Vanilla `Entity.ejectPassengers`.
fn eject_passengers(boat: &dyn Entity) {
    for passenger in boat.passengers().iter().rev() {
        passenger.stop_riding();
    }
}

/// Vanilla `AbstractBoat.tick`.
pub(super) fn tick_boat<B: AbstractBoat>(boat: &Arc<B>) {
    let status = compute_status(boat.as_ref());
    let out_of_control_ticks = {
        let mut state = boat.boat_core().state.lock();
        state.old_status = state.status;
        state.status = Some(status);
        if matches!(
            status,
            BoatStatus::UnderWater | BoatStatus::UnderFlowingWater
        ) {
            state.out_of_control_ticks += 1.0;
        } else {
            state.out_of_control_ticks = 0.0;
        }
        state.out_of_control_ticks
    };
    if out_of_control_ticks >= TIME_TO_EJECT {
        eject_passengers(boat.as_ref());
    }

    if boat.hurt_time() > 0 {
        boat.set_hurt_time(boat.hurt_time() - 1);
    }
    if boat.vehicle_damage() > 0.0 {
        boat.set_vehicle_damage(boat.vehicle_damage() - 1.0);
    }

    boat.base_tick();
    if boat.is_server_driven_movement() {
        if boat
            .first_passenger()
            .is_none_or(|passenger| passenger.as_player().is_none())
        {
            boat.set_paddle_state(false, false);
        }
        float_boat(boat.as_ref());
        let _ = Arc::clone(boat).move_entity(MoverType::SelfMovement, boat.velocity());
    } else {
        boat.set_velocity(DVec3::ZERO);
        boat.boat_core().state.lock().delta_rotation = 0.0;
    }

    boat.apply_effects_from_blocks();
    boat.apply_effects_from_blocks();
    tick_bubble_column(boat.as_ref());
    tick_paddles(boat.as_ref());
    push_and_board_entities(boat);
}

/// The server half of vanilla `tickBubbleColumn`.
fn tick_bubble_column<B: AbstractBoat + ?Sized>(boat: &B) {
    let (above, drag_down) = {
        let state = boat.boat_core().state.lock();
        (
            state.is_above_bubble_column,
            state.bubble_column_direction_is_down,
        )
    };
    if !above {
        boat.set_bubble_time(0);
    }

    let mut bubble_time = boat.bubble_time();
    if bubble_time <= 0 {
        return;
    }
    bubble_time -= 1;
    boat.set_bubble_time(bubble_time);
    let diff = BUBBLE_TIME - bubble_time - 1;
    if diff > 0 && bubble_time == 0 {
        boat.set_bubble_time(0);
        if drag_down {
            eject_passengers(boat.as_entity_event_source());
            boat.broadcast_entity_event(EntityStatus::BoatSink);
        } else {
            boat.broadcast_entity_event(EntityStatus::BoatLaunch);
        }
        boat.handle_bubble_column_effect(drag_down);
    }
    boat.boat_core().state.lock().is_above_bubble_column = false;
}

/// The paddle stroke sounds from vanilla `AbstractBoat.tick`.
fn tick_paddles<B: AbstractBoat + ?Sized>(boat: &B) {
    for side in [PADDLE_LEFT, PADDLE_RIGHT] {
        if !boat.paddle_state(side) {
            boat.boat_core().state.lock().paddle_positions[side] = 0.0;
            continue;
        }
        let paddle_position = boat.boat_core().state.lock().paddle_positions[side];
        if !boat.is_silent()
            && paddle_position % (PI * 2.0) <= PADDLE_SOUND_TIME
            && (paddle_position + PADDLE_SPEED) % (PI * 2.0) >= PADDLE_SOUND_TIME
            && let Some(sound) = boat.paddle_sound()
            && let Some(world) = boat.level()
        {
            let view_vector = boat.look_angle();
            let dx = if side == PADDLE_RIGHT {
                -view_vector.z
            } else {
                view_vector.z
            };
            let dz = if side == PADDLE_RIGHT {
                view_vector.x
            } else {
                -view_vector.x
            };
            let position = boat.position();
            let pitch = 0.8 + 0.4 * rand::rng().random::<f32>();
            world.play_sound_at(
                sound,
                boat.sound_source(),
                DVec3::new(position.x + dx, position.y, position.z + dz),
                1.0,
                pitch,
                None,
            );
        }
        boat.boat_core().state.lock().paddle_positions[side] = paddle_position + PADDLE_SPEED;
    }
}

/// Vanilla's end of `AbstractBoat.tick`: mobs bumping a free boat climb in,
/// everything else is pushed.
fn push_and_board_entities<B: AbstractBoat>(boat: &Arc<B>) {
    let Some(world) = boat.level() else {
        return;
    };
    let hitbox = boat.bounding_box().inflate_xyz(
        f64::from(0.2_f32),
        f64::from(-0.01_f32),
        f64::from(0.2_f32),
    );
    let pushable = EntitySelector::pushable_by(boat.as_ref());
    let entities = world.get_entities_in_aabb_matching(&hitbox, |entity| pushable(entity));
    if entities.is_empty() {
        return;
    }
    let add_new_passengers = boat
        .controlling_passenger()
        .is_none_or(|controller| controller.as_player().is_none());
    let Some(vehicle) = world.get_entity_by_id(boat.id()) else {
        return;
    };
    for entity in entities {
        if entity.has_passenger(boat.as_ref()) {
            continue;
        }
        if add_new_passengers
            && boat.passengers().len() < boat.max_passengers()
            && !entity.is_passenger()
            && boat.has_enough_space_for(entity.as_ref())
            && entity.as_living_entity().is_some()
            && !REGISTRY.entity_types.is_in_tag(
                entity.entity_type(),
                &EntityTypeTag::CANNOT_BE_PUSHED_ONTO_BOATS,
            )
        {
            entity.start_riding(&vehicle);
        } else {
            boat.push_entity(entity.as_ref());
        }
    }
}

/// Vanilla `getStatus`, which also records the water level and land friction.
fn compute_status<B: AbstractBoat + ?Sized>(boat: &B) -> BoatStatus {
    let Some(world) = boat.level() else {
        return BoatStatus::InAir;
    };
    if let Some(water_status) = underwater_status(&world, boat.bounding_box()) {
        boat.boat_core().state.lock().water_level = boat.bounding_box().max_y();
        return water_status;
    }
    let (in_water, water_level) = check_in_water(&world, boat.bounding_box());
    boat.boat_core().state.lock().water_level = water_level;
    if in_water {
        return BoatStatus::InWater;
    }
    let friction = ground_friction(&world, boat.bounding_box());
    if friction > 0.0 {
        boat.boat_core().state.lock().land_friction = friction;
        BoatStatus::OnLand
    } else {
        BoatStatus::InAir
    }
}

/// Vanilla `Mth.floor(double)`.
const fn floor(value: f64) -> i32 {
    value.floor() as i32
}

/// Vanilla `Mth.ceil(double)`.
const fn ceil(value: f64) -> i32 {
    value.ceil() as i32
}

/// Vanilla `getWaterLevelAbove`.
fn water_level_above(world: &Arc<World>, aabb: WorldAabb, last_yd: f64) -> f32 {
    let min_x = floor(aabb.min_x());
    let max_x = ceil(aabb.max_x());
    let min_y = floor(aabb.max_y());
    let max_y = ceil(aabb.max_y() - last_yd);
    let min_z = floor(aabb.min_z());
    let max_z = ceil(aabb.max_z());

    'rows: for y in min_y..max_y {
        let mut block_height = 0.0_f32;
        for x in min_x..max_x {
            for z in min_z..max_z {
                let pos = BlockPos::new(x, y, z);
                let fluid_state = get_fluid_state(world, pos);
                if fluid_state.is_water() {
                    block_height = block_height.max(get_height(world, pos, fluid_state));
                }
                if block_height >= 1.0 {
                    continue 'rows;
                }
            }
        }
        if block_height < 1.0 {
            return y as f32 + block_height;
        }
    }
    (max_y + 1) as f32
}

/// Vanilla `getGroundFriction`: the average friction of the blocks under the boat.
fn ground_friction(world: &Arc<World>, bb: WorldAabb) -> f32 {
    let boat_box = WorldAabb::new(
        bb.min_x(),
        bb.min_y() - 0.001,
        bb.min_z(),
        bb.max_x(),
        bb.min_y(),
        bb.max_z(),
    );
    let x0 = floor(boat_box.min_x()) - 1;
    let x1 = ceil(boat_box.max_x()) + 1;
    let y0 = floor(boat_box.min_y()) - 1;
    let y1 = ceil(boat_box.max_y()) + 1;
    let z0 = floor(boat_box.min_z()) - 1;
    let z1 = ceil(boat_box.max_z()) + 1;
    let mut friction = 0.0_f32;
    let mut count = 0_i32;

    for x in x0..x1 {
        for z in z0..z1 {
            let edges = i32::from(x == x0 || x == x1 - 1) + i32::from(z == z0 || z == z1 - 1);
            if edges == 2 {
                continue;
            }
            for y in y0..y1 {
                if edges > 0 && (y == y0 || y == y1 - 1) {
                    continue;
                }
                let pos = BlockPos::new(x, y, z);
                let state = world.get_block_state(pos);
                let block = state.get_block();
                if block == &vanilla_blocks::LILY_PAD {
                    continue;
                }
                let touches = collision_shape(world, pos)
                    .iter()
                    .any(|shape| shape.at_block(pos).intersects(boat_box));
                if touches {
                    friction += block.config.friction;
                    count += 1;
                }
            }
        }
    }

    friction / count as f32
}

/// Vanilla `checkInWater`: whether the boat's bottom is in water, and the water level.
fn check_in_water(world: &Arc<World>, bb: WorldAabb) -> (bool, f64) {
    let min_x = floor(bb.min_x());
    let max_x = ceil(bb.max_x());
    let min_y = floor(bb.min_y());
    let max_y = ceil(bb.min_y() + 0.001);
    let min_z = floor(bb.min_z());
    let max_z = ceil(bb.max_z());
    let mut in_water = false;
    let mut water_level = -f64::MAX;

    for x in min_x..max_x {
        for y in min_y..max_y {
            for z in min_z..max_z {
                let pos = BlockPos::new(x, y, z);
                let fluid_state = get_fluid_state(world, pos);
                if fluid_state.is_water() {
                    let height = y as f32 + get_height(world, pos, fluid_state);
                    water_level = water_level.max(f64::from(height));
                    in_water |= bb.min_y() < f64::from(height);
                }
            }
        }
    }

    (in_water, water_level)
}

/// Vanilla `isUnderwater`: whether water covers the top of the boat.
fn underwater_status(world: &Arc<World>, aabb: WorldAabb) -> Option<BoatStatus> {
    let max_y = aabb.max_y() + 0.001;
    let x0 = floor(aabb.min_x());
    let x1 = ceil(aabb.max_x());
    let y0 = floor(aabb.max_y());
    let y1 = ceil(max_y);
    let z0 = floor(aabb.min_z());
    let z1 = ceil(aabb.max_z());
    let mut under_water = false;

    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                let pos = BlockPos::new(x, y, z);
                let fluid_state = get_fluid_state(world, pos);
                if fluid_state.is_water()
                    && max_y < f64::from(y as f32 + get_height(world, pos, fluid_state))
                {
                    if !fluid_state.is_source() {
                        return Some(BoatStatus::UnderFlowingWater);
                    }
                    under_water = true;
                }
            }
        }
    }

    under_water.then_some(BoatStatus::UnderWater)
}

/// Vanilla `floatBoat`: buoyancy, drag and gravity.
fn float_boat<B: AbstractBoat + ?Sized>(boat: &B) {
    let mut vspeed = -boat.get_gravity();
    let mut buoyancy = 0.0;
    let mut inv_friction = 0.05_f32;
    let (old_status, status) = {
        let state = boat.boat_core().state.lock();
        (state.old_status, state.status)
    };
    let height = boat.base().dimensions().height;

    if old_status == Some(BoatStatus::InAir)
        && status != Some(BoatStatus::InAir)
        && status != Some(BoatStatus::OnLand)
    {
        let position = boat.position();
        let last_yd = {
            let mut state = boat.boat_core().state.lock();
            state.water_level = position.y + f64::from(height);
            state.last_yd
        };
        if let Some(world) = boat.level() {
            let target_y =
                f64::from(water_level_above(&world, boat.bounding_box(), last_yd) - height) + 0.101;
            let collision_world =
                WorldCollisionProvider::for_entity(&world, boat.as_entity_event_source());
            let moved_box =
                boat.bounding_box()
                    .translate(DVec3::new(0.0, target_y - position.y, 0.0));
            let no_collision = !collision_world.has_collision_with_context(
                &moved_box,
                physics_state_for_move(boat.as_entity_event_source()).block_collision_context(),
            );
            if no_collision {
                let _ = boat.try_set_position(DVec3::new(position.x, target_y, position.z));
                boat.set_velocity(boat.velocity() * DVec3::new(1.0, 0.0, 1.0));
                boat.boat_core().state.lock().last_yd = 0.0;
            }
        }
        boat.boat_core().state.lock().status = Some(BoatStatus::InWater);
        return;
    }

    match status {
        Some(BoatStatus::InWater) => {
            let water_level = boat.boat_core().state.lock().water_level;
            buoyancy = (water_level - boat.position().y) / f64::from(height);
            inv_friction = 0.9;
        }
        Some(BoatStatus::UnderFlowingWater) => {
            vspeed = -7.0e-4;
            inv_friction = 0.9;
        }
        Some(BoatStatus::UnderWater) => {
            buoyancy = f64::from(0.01_f32);
            inv_friction = 0.45;
        }
        Some(BoatStatus::InAir) => inv_friction = 0.9,
        Some(BoatStatus::OnLand) => {
            let mut state = boat.boat_core().state.lock();
            inv_friction = state.land_friction;
            if boat
                .controlling_passenger()
                .is_some_and(|controller| controller.as_player().is_some())
            {
                state.land_friction /= 2.0;
            }
        }
        None => {}
    }

    let movement = boat.velocity();
    let friction = f64::from(inv_friction);
    boat.set_velocity(DVec3::new(
        movement.x * friction,
        movement.y + vspeed,
        movement.z * friction,
    ));
    boat.boat_core().state.lock().delta_rotation *= inv_friction;
    if buoyancy > 0.0 {
        let movement = boat.velocity();
        boat.set_velocity(DVec3::new(
            movement.x,
            (movement.y + buoyancy * (BOAT_GRAVITY / 0.65)) * 0.75,
            movement.z,
        ));
    }
}

/// Vanilla `AbstractBoat.getDefaultGravity`.
#[must_use]
pub(super) const fn boat_gravity() -> f64 {
    BOAT_GRAVITY
}
