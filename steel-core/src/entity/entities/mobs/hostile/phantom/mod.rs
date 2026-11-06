//! Vanilla `Phantom`: an undead flyer that circles above players who have not
//! slept, then swoops down to bite them.

mod goals;

#[cfg(test)]
mod tests;

use std::f32::consts::PI;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_math::angle::{DEG_TO_RAD, RAD_TO_DEG, wrap_degrees};
use steel_math::trig;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::PhantomEntityData;
use steel_registry::{sound_events, vanilla_attributes};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use self::goals::{
    PhantomAttackPlayerTargetGoal, PhantomAttackStrategyGoal, PhantomCircleAroundAnchorGoal,
    PhantomSweepAttackGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    DefaultEntityPush, Enemy, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    PathfinderMob, SharedEntity, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 5;
const MAX_SIZE: i32 = 64;
const SIZE_SCALE_PER_STEP: f32 = 0.15;
const BASE_ATTACK_DAMAGE: i32 = 6;
const FLYING_SPEED: f32 = 0.2;
/// Vanilla `Phantom.TICKS_PER_FLAP`: `ceil(24.166098F)`.
const TICKS_PER_FLAP: i32 = 25;
const SPAWN_ANCHOR_HEIGHT: i32 = 5;
const TAG_ANCHOR_POS: &str = "anchor_pos";
const TAG_SIZE: &str = "size";

const MOVE_INITIAL_SPEED: f32 = 0.1;
const MOVE_MAX_SPEED: f32 = 1.8;
const MOVE_TURNING_SPEED: f32 = 0.2;
const MOVE_MAX_TURN: f32 = 4.0;
const MOVE_STRAIGHT_TOLERANCE: f32 = 3.0;

/// Vanilla `Phantom.AttackPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttackPhase {
    Circle,
    Swoop,
}

/// The fields vanilla keeps on the phantom for its goals and move control.
#[derive(Debug)]
pub(crate) struct PhantomState {
    pub(crate) move_target_point: DVec3,
    pub(crate) anchor_point: Option<BlockPos>,
    pub(crate) attack_phase: AttackPhase,
    /// Vanilla `PhantomMoveControl.speed`.
    move_speed: f32,
}

/// A phantom.
#[entity_behavior(class = "Phantom")]
pub struct PhantomEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<PhantomEntityData>,
    state: SyncMutex<PhantomState>,
}

// SAFETY: The owner-scoped type key uniquely identifies PhantomEntity.
unsafe impl DowncastType for PhantomEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/phantom");
}

impl PhantomEntity {
    /// Creates a new phantom.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved phantom.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        mob_base.set_xp_reward(XP_REWARD);
        let mut entity_data = PhantomEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, PhantomAttackStrategyGoal::new());
            goal_selector.add_goal(2, PhantomSweepAttackGoal::new());
            goal_selector.add_goal(3, PhantomCircleAroundAnchorGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, PhantomAttackPlayerTargetGoal::new());
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            state: SyncMutex::new(PhantomState {
                move_target_point: DVec3::ZERO,
                anchor_point: None,
                attack_phase: AttackPhase::Circle,
                move_speed: MOVE_INITIAL_SPEED,
            }),
        }
    }

    /// Vanilla `getPhantomSize`.
    #[must_use]
    pub fn phantom_size(&self) -> i32 {
        *self.entity_data.lock().id_size.get()
    }

    /// Vanilla `setPhantomSize` plus `updatePhantomSizeInfo`: a bigger
    /// phantom is larger and bites harder.
    pub fn set_phantom_size(&self, size: i32) {
        let size = size.clamp(0, MAX_SIZE);
        self.entity_data.lock().id_size.set(size);
        self.refresh_dimensions();
        self.attributes().lock().set_base_value(
            vanilla_attributes::ATTACK_DAMAGE,
            f64::from(BASE_ATTACK_DAMAGE + size),
        );
    }

    /// The point the phantom circles around.
    #[must_use]
    pub fn anchor_point(&self) -> Option<BlockPos> {
        self.state.lock().anchor_point
    }

    /// The point the move control flies to.
    #[must_use]
    pub fn move_target_point(&self) -> DVec3 {
        self.state.lock().move_target_point
    }

    pub(crate) const fn state(&self) -> &SyncMutex<PhantomState> {
        &self.state
    }

    /// Vanilla `getUniqueFlapTickOffset`.
    fn unique_flap_tick_offset(&self) -> i32 {
        self.id().wrapping_mul(3)
    }

    /// Vanilla `PhantomMoveControl.tick`: turn toward the move target at up
    /// to 4 degrees a tick, speeding up while flying straight.
    fn tick_phantom_move_control(&self) {
        let mut state = self.state.lock();
        if self.horizontal_collision() {
            let (y_rot, x_rot) = self.rotation();
            self.set_rotation((y_rot + 180.0, x_rot));
            state.move_speed = MOVE_INITIAL_SPEED;
        }

        let position = self.position();
        let mut tdx = state.move_target_point.x - position.x;
        let tdy = state.move_target_point.y - position.y;
        let mut tdz = state.move_target_point.z - position.z;
        let mut sd = tdx.hypot(tdz);
        if sd.abs() <= f64::from(1.0e-5_f32) {
            return;
        }
        let y_relative_scale = 1.0 - (tdy * f64::from(0.7_f32)).abs() / sd;
        tdx *= y_relative_scale;
        tdz *= y_relative_scale;
        sd = tdx.hypot(tdz);
        let sd2 = (tdx * tdx + tdz * tdz + tdy * tdy).sqrt();
        let prev = self.rotation().0;
        let angle = tdz.atan2(tdx) as f32;
        let a = wrap_degrees(prev + 90.0);
        let b = wrap_degrees(angle * RAD_TO_DEG);
        let y_rot = approach_degrees(a, b, MOVE_MAX_TURN) - 90.0;
        let speed = state.move_speed;
        state.move_speed = if degrees_difference_abs(prev, y_rot) < MOVE_STRAIGHT_TOLERANCE {
            approach(speed, MOVE_MAX_SPEED, 0.005 * (MOVE_MAX_SPEED / speed))
        } else {
            approach(speed, MOVE_TURNING_SPEED, 0.025)
        };
        let speed = state.move_speed;
        drop(state);

        // Vanilla `(float)(-(Mth.atan2(-tdy, sd) * 180.0F / (float)Math.PI))`.
        let x_rot = (-((-tdy).atan2(sd) * f64::from(180.0_f32) / f64::from(PI))) as f32;
        self.set_rotation((y_rot, x_rot));
        self.set_y_body_rot(y_rot);
        let move_angle = (y_rot + 90.0) * DEG_TO_RAD;
        let txd = f64::from(speed * trig::cos(move_angle)) * (tdx / sd2).abs();
        let tzd = f64::from(speed * trig::sin(move_angle)) * (tdz / sd2).abs();
        let tyd = f64::from(speed * trig::sin(x_rot * DEG_TO_RAD)) * (tdy / sd2).abs();
        let movement = self.velocity();
        self.set_velocity(movement + (DVec3::new(txd, tyd, tzd) - movement) * 0.2);
    }
}

/// Vanilla `Mth.approach`.
fn approach(current: f32, target: f32, increment: f32) -> f32 {
    let increment = increment.abs();
    if current < target {
        (current + increment).clamp(current, target)
    } else {
        (current - increment).clamp(target, current)
    }
}

/// Vanilla `Mth.approachDegrees`.
fn approach_degrees(current: f32, target: f32, increment: f32) -> f32 {
    let difference = wrap_degrees(target - current);
    approach(current, current + difference, increment)
}

/// Vanilla `Mth.degreesDifferenceAbs`.
fn degrees_difference_abs(a: f32, b: f32) -> f32 {
    wrap_degrees(b - a).abs()
}

impl DefaultEntityPush for PhantomEntity {}

impl Enemy for PhantomEntity {}

impl Entity for PhantomEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn as_enemy(&self) -> Option<&dyn Enemy> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::All
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(SIZE_SCALE_PER_STEP.mul_add(self.phantom_size() as f32, 1.0))
            .scale(LivingEntity::get_scale(self))
    }

    fn is_flapping(&self) -> bool {
        (self.unique_flap_tick_offset() + self.tick_count()) % TICKS_PER_FLAP == 0
    }

    fn check_fall_damage(
        self: Arc<Self>,
        _vertical_movement: f64,
        _on_ground: bool,
        _on_state: BlockStateId,
        _pos: BlockPos,
        _world: &Arc<World>,
    ) {
    }

    fn on_climbable(&self) -> bool {
        false
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        if let Some(pos) = self.anchor_point() {
            nbt.insert(
                TAG_ANCHOR_POS,
                NbtTag::IntArray(vec![pos.x(), pos.y(), pos.z()]),
            );
        }
        nbt.insert(TAG_SIZE, self.phantom_size());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.state.lock().anchor_point =
            nbt.int_array(TAG_ANCHOR_POS)
                .and_then(|values| match values.as_slice() {
                    [x, y, z] => Some(BlockPos::new(*x, *y, *z)),
                    _ => None,
                });
        self.set_phantom_size(nbt.int(TAG_SIZE).unwrap_or(0));
    }
}

impl LivingEntity for PhantomEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PHANTOM_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PHANTOM_DEATH)
    }

    fn travel(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        self.travel_flying(entity, input, FLYING_SPEED, FLYING_SPEED, FLYING_SPEED)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Mob::mob_ai_step(self, entity)
    }
}

impl Mob for PhantomEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_move_control(&self) {
        self.tick_phantom_move_control();
    }

    /// Vanilla `PhantomLookControl.tick` does nothing.
    fn tick_look_control(&self) {}

    /// Vanilla `PhantomBodyRotationControl.clientTick`: the head follows the
    /// body, the body follows the flight direction.
    fn tick_body_rotation_control(&self) {
        self.set_y_head_rot(self.y_body_rot());
        self.set_y_body_rot(self.rotation().0);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.state.lock().anchor_point = Some(self.block_position().above_n(SPAWN_ANCHOR_HEIGHT));
        self.set_phantom_size(0);
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PHANTOM_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

/// Steel runs goals on pathfinder mobs only; the phantom never walks a path.
impl PathfinderMob for PhantomEntity {}
