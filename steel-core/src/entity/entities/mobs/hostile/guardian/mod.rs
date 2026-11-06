//! Vanilla `Guardian` and `ElderGuardian`: water-bound monsters that charge a
//! beam at players, squids and axolotls.

mod elder_guardian;
mod goals;

use std::f32::consts::PI as PI_F32;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::{Rng, RngExt as _};
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::GuardianEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_damage_types, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

pub use elder_guardian::ElderGuardianEntity;
use goals::{GuardianAttackGoal, GuardianRandomStrollGoal, MoveAndLookGoal};

use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{
    LookAtPlayerGoal, MoveTowardsRestrictionGoal, NearestAttackableTargetGoal,
    RandomLookAroundGoal, RandomStrollGoal,
};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::mob::rotlerp;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster,
    PathfinderMob, SharedEntity, SpawnRule, sync_dirty_mob_effects,
};
use crate::fluid::FluidStateExt as _;
use crate::physics::{MoveResult, MoverType};
use crate::world::{LevelReader, ServerLevelAccessor, World};

/// Vanilla `Guardian.ATTACK_TIME`: ticks the beam charges before it hits.
pub const GUARDIAN_ATTACK_DURATION: i32 = 80;
const GUARDIAN_XP_REWARD: i32 = 10;
const GUARDIAN_AMBIENT_SOUND_INTERVAL: i32 = 160;
const GUARDIAN_MAX_HEAD_X_ROT: f32 = 180.0;
const GUARDIAN_STROLL_INTERVAL: i32 = 80;
const GUARDIAN_THORNS_DAMAGE: f32 = 2.0;
const GUARDIAN_MAX_AIR_SUPPLY: i32 = 300;
const GUARDIAN_LAND_FLOP_SPREAD: f32 = 0.4;
const GUARDIAN_LAND_FLOP_JUMP: f64 = 0.5;
const GUARDIAN_WATER_WALK_TARGET_BONUS: f32 = 10.0;
const GUARDIAN_WATER_ACCELERATION: f32 = 0.1;
const GUARDIAN_WATER_DRAG: f64 = 0.9;
const GUARDIAN_IDLE_SINK: f64 = -0.005;
const GUARDIAN_MOVE_CONTROL_MAX_TURN: f32 = 90.0;
const GUARDIAN_MOVE_CONTROL_LERP: f32 = 0.125;
/// Vanilla `GuardianAttackGoal` and `GuardianAttackSelector`: targets closer
/// than 3 blocks are ignored, and a normal guardian stops attacking them.
pub(super) const GUARDIAN_MIN_ATTACK_DISTANCE_SQR: f64 = 9.0;
/// Vanilla guardians look at their beam target with 90° turn limits.
pub(super) const GUARDIAN_ATTACK_LOOK_ROT: f32 = 90.0;
const GUARDIAN_TARGET_RANDOM_INTERVAL: i32 = 10;
const GUARDIAN_LOOK_AT_GUARDIAN_DISTANCE: f64 = 12.0;
const GUARDIAN_LOOK_AT_GUARDIAN_PROBABILITY: f32 = 0.01;
const GUARDIAN_LOOK_AT_PLAYER_DISTANCE: f64 = 8.0;
const GUARDIAN_SPAWN_SKY_ROLL_BOUND: i32 = 20;

/// Vanilla `Guardian` behavior shared with `ElderGuardian`.
pub trait Guardian: Monster {
    /// Vanilla `Guardian.isMoving` (synced `DATA_ID_MOVING`).
    fn is_guardian_moving(&self) -> bool;

    /// Vanilla `Guardian.setMoving`.
    fn set_guardian_moving(&self, moving: bool);

    /// The synced id of the entity the beam is locked on, or 0 for none.
    fn active_attack_target_id(&self) -> i32;

    /// Vanilla `Guardian.setActiveAttackTarget`.
    fn set_active_attack_target(&self, entity_id: i32);

    /// Set when the random stroll goal should run on its next check.
    fn random_stroll_trigger(&self) -> &AtomicBool;

    /// Vanilla `Guardian.getAttackDuration`.
    fn attack_duration(&self) -> i32 {
        GUARDIAN_ATTACK_DURATION
    }

    /// Vanilla `Guardian.hasActiveAttackTarget`.
    fn has_active_attack_target(&self) -> bool {
        self.active_attack_target_id() != 0
    }

    /// Vanilla `randomStrollGoal.trigger()`.
    fn trigger_random_stroll(&self) {
        self.random_stroll_trigger().store(true, Ordering::Relaxed);
    }

    /// Vanilla `Guardian.getWalkTargetValue`: water is worth more the brighter
    /// it is, dry land is scored like any monster's.
    fn guardian_walk_target_value(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        if level.get_block_state(pos).get_fluid_state().is_water() {
            GUARDIAN_WATER_WALK_TARGET_BONUS + level.pathfinding_cost_from_light_levels(pos)
        } else {
            self.get_walk_target_value_monster(pos, level)
        }
    }

    /// Vanilla `Guardian.hurtServer`: while its spikes are out, a guardian
    /// hurts the living attacker that hit it directly.
    fn guardian_hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if !self.is_guardian_moving()
            && !source.is(&DamageTypeTag::AVOIDS_GUARDIAN_THORNS)
            && source.damage_type != &vanilla_damage_types::THORNS
            && let Some(cause) = source.direct_entity()
            && cause.as_living_entity().is_some()
            && let Some(this) = world.get_entity_by_id(self.id())
        {
            let thorns = DamageSource::direct(&vanilla_damage_types::THORNS, this);
            cause.hurt(world, &thorns, GUARDIAN_THORNS_DAMAGE);
        }

        self.trigger_random_stroll();
        LivingEntity::hurt_server(self, world, source, amount)
    }

    /// Vanilla `Guardian.aiStep` on the server: refill air in water, flop
    /// around on land, and face the beam target.
    fn guardian_ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        if LivingEntity::is_alive(self) {
            if self.is_in_water() {
                self.set_air_supply(GUARDIAN_MAX_AIR_SUPPLY);
            } else if self.on_ground() {
                let x = (rand::random::<f32>() * 2.0 - 1.0) * GUARDIAN_LAND_FLOP_SPREAD;
                let z = (rand::random::<f32>() * 2.0 - 1.0) * GUARDIAN_LAND_FLOP_SPREAD;
                self.set_velocity(
                    self.velocity()
                        + DVec3::new(f64::from(x), GUARDIAN_LAND_FLOP_JUMP, f64::from(z)),
                );
                let (_, x_rot) = self.rotation();
                self.set_rotation((rand::random::<f32>() * 360.0, x_rot));
                self.set_on_ground(false);
                self.mark_velocity_sync();
            }

            if self.has_active_attack_target() {
                let (_, x_rot) = self.rotation();
                self.set_rotation((self.y_head_rot(), x_rot));
            }
        }

        Monster::monster_ai_step(self, entity)
    }

    /// Vanilla `Guardian.travelInWater`: idle guardians slowly sink.
    fn guardian_travel_in_water(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        self.move_relative(GUARDIAN_WATER_ACCELERATION, input);
        let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
        self.set_velocity(self.velocity() * GUARDIAN_WATER_DRAG);
        if !self.is_guardian_moving() && self.target().is_none() {
            self.set_velocity(self.velocity() + DVec3::new(0.0, GUARDIAN_IDLE_SINK, 0.0));
        }
        result
    }

    /// Vanilla `Guardian.GuardianMoveControl.tick`: swims straight at the
    /// wanted position with a gentle wobble, looking where it goes. It keeps
    /// `MOVE_TO` until navigation finishes.
    fn tick_guardian_move_control(&self) {
        let move_control = self.mob_base().controls().lock().move_control;
        let navigating = !self.mob_base().navigation().lock().is_done();
        if !matches!(move_control.operation(), MoveControlOperation::MoveTo) || !navigating {
            self.set_mob_speed(0.0);
            self.set_guardian_moving(false);
            return;
        }

        let position = self.position();
        let delta = move_control.wanted_position() - position;
        let length = delta.length();
        let xd = delta.x / length;
        let yd = delta.y / length;
        let zd = delta.z / length;
        let y_rot_d = (delta.z.atan2(delta.x) * 180.0 / f64::from(PI_F32)) as f32 - 90.0;
        let (y_rot, x_rot) = self.rotation();
        let y_rot = rotlerp(y_rot, y_rot_d, GUARDIAN_MOVE_CONTROL_MAX_TURN);
        self.set_rotation((y_rot, x_rot));
        self.set_y_body_rot(y_rot);

        let movement_speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED);
        let target_speed = (move_control.speed_modifier() * movement_speed) as f32;
        let speed = self.get_speed();
        let new_speed = speed + GUARDIAN_MOVE_CONTROL_LERP * (target_speed - speed);
        self.set_mob_speed(new_speed);

        let phase = f64::from(self.tick_count().wrapping_add(self.id()));
        let push = (phase * 0.5).sin() * 0.05;
        let y_rot_rad = y_rot * (PI / 180.0) as f32;
        let cos = f64::from(y_rot_rad).cos();
        let sin = f64::from(y_rot_rad).sin();
        let y_push = (phase * 0.75).sin() * 0.05;
        self.set_velocity(
            self.velocity()
                + DVec3::new(
                    push * cos,
                    y_push * (sin + cos) * 0.25 + f64::from(new_speed) * yd * 0.1,
                    push * sin,
                ),
        );

        let new_look = DVec3::new(
            position.x + xd * 2.0,
            self.get_eye_y() + yd / length,
            position.z + zd * 2.0,
        );
        {
            let mut controls = self.mob_base().controls().lock();
            let look_control = &mut controls.look_control;
            let old_look = if look_control.is_looking_at_target() {
                look_control.wanted_position()
            } else {
                new_look
            };
            let lerp = f64::from(GUARDIAN_MOVE_CONTROL_LERP);
            look_control.set_look_at(old_look + (new_look - old_look) * lerp, 10.0, 40.0);
        }
        self.set_guardian_moving(true);
    }
}

/// Vanilla `Guardian.checkGuardianSpawnRules`, shared by elder guardians:
/// deep or dark water on top of more water, never on peaceful.
pub(crate) fn check_guardian_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    random: &mut dyn Rng,
) -> bool {
    (random.random_range(0..GUARDIAN_SPAWN_SKY_ROLL_BOUND) == 0
        || !level.can_see_sky_from_below_water(pos))
        && level.difficulty() != Difficulty::Peaceful
        && (spawn_reason.is_spawner() || level.get_block_state(pos).get_fluid_state().is_water())
        && level
            .get_block_state(pos.below())
            .get_fluid_state()
            .is_water()
}

/// Vanilla `Guardian.GuardianAttackSelector`.
fn guardian_attack_selector(mob: &dyn PathfinderMob, target: &dyn LivingEntity, _: &World) -> bool {
    (target.as_player().is_some()
        || target.as_squid().is_some()
        || target.entity_type() == &vanilla_entities::AXOLOTL)
        && target.position().distance_squared(mob.position()) > GUARDIAN_MIN_ATTACK_DISTANCE_SQR
}

/// Vanilla `Guardian` constructor and `registerGoals`: water-bound
/// navigation, free water pathing and the guardian goals. Returns the flag
/// that forces the random stroll goal.
pub(crate) fn init_guardian(
    mob_base: &MobBase,
    elder: bool,
    stroll_interval: i32,
) -> Arc<AtomicBool> {
    mob_base.set_xp_reward(GUARDIAN_XP_REWARD);
    mob_base
        .pathfinding_malus()
        .lock()
        .set(PathType::Water, 0.0);
    *mob_base.navigation().lock() = PathNavigation::water_bound(false);

    let stroll_trigger = Arc::new(AtomicBool::new(false));
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        goal_selector.add_goal(4, GuardianAttackGoal::new(elder));
        goal_selector.add_goal(
            5,
            MoveAndLookGoal::new(MoveTowardsRestrictionGoal::new(1.0)),
        );
        goal_selector.add_goal(
            7,
            GuardianRandomStrollGoal::new(
                RandomStrollGoal::with_interval(1.0, stroll_interval),
                Arc::clone(&stroll_trigger),
            ),
        );
        goal_selector.add_goal(8, LookAtPlayerGoal::new(GUARDIAN_LOOK_AT_PLAYER_DISTANCE));
        goal_selector.add_goal(
            8,
            LookAtPlayerGoal::new_for_living_entities(
                GUARDIAN_LOOK_AT_GUARDIAN_DISTANCE,
                GUARDIAN_LOOK_AT_GUARDIAN_PROBABILITY,
                |target, _| target.as_guardian().is_some(),
            ),
        );
        goal_selector.add_goal(9, RandomLookAroundGoal::new());

        mob_base.target_selector().lock().add_goal(
            1,
            NearestAttackableTargetGoal::new_with_interval(
                GUARDIAN_TARGET_RANDOM_INTERVAL,
                true,
                false,
                |_, _| true,
            )
            .with_mob_selector(guardian_attack_selector),
        );
    }
    stroll_trigger
}

/// Vanilla guardian.
#[entity_behavior(class = "Guardian", spawn_rule)]
pub struct GuardianEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<GuardianEntityData>,
    stroll_trigger: Arc<AtomicBool>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `GuardianEntity`.
unsafe impl DowncastType for GuardianEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/guardian");
}

impl GuardianEntity {
    /// Vanilla `SpawnPlacements` predicate: `Guardian.checkGuardianSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_guardian_spawn_rules;

    /// Creates a new guardian.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved guardian.
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
        let stroll_trigger = init_guardian(&mob_base, false, GUARDIAN_STROLL_INTERVAL);

        let mut entity_data = GuardianEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            stroll_trigger,
        }
    }
}

impl Guardian for GuardianEntity {
    fn is_guardian_moving(&self) -> bool {
        *self.entity_data.lock().id_moving.get()
    }

    fn set_guardian_moving(&self, moving: bool) {
        self.entity_data.lock().id_moving.set(moving);
    }

    fn active_attack_target_id(&self) -> i32 {
        *self.entity_data.lock().id_attack_target.get()
    }

    fn set_active_attack_target(&self, entity_id: i32) {
        self.entity_data.lock().id_attack_target.set(entity_id);
    }

    fn random_stroll_trigger(&self) -> &AtomicBool {
        &self.stroll_trigger
    }
}

impl DefaultEntityPush for GuardianEntity {}

impl Entity for GuardianEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.guardian_hurt(world, source, amount)
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(LivingEntity::get_scale(self))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn sound_source(&self) -> SoundSource {
        Monster::sound_source_monster(self)
    }

    fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        Monster::fall_sounds_monster(self)
    }

    fn swim_sound(&self) -> SoundEventRef {
        Monster::swim_sound_monster(self)
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        Monster::swim_splash_sound_monster(self)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
    }
}

impl LivingEntity for GuardianEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let clamped = health.clamp(0.0, self.get_max_health());
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_GUARDIAN_HURT
        } else {
            &sound_events::ENTITY_GUARDIAN_HURT_LAND
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_GUARDIAN_DEATH
        } else {
            &sound_events::ENTITY_GUARDIAN_DEATH_LAND
        })
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.guardian_travel_in_water(entity, input)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        self.guardian_ai_step(entity)
    }
}

impl Mob for GuardianEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        self.tick_guardian_move_control();
    }

    fn max_head_x_rot(&self) -> f32 {
        GUARDIAN_MAX_HEAD_X_ROT
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_GUARDIAN_AMBIENT
        } else {
            &sound_events::ENTITY_GUARDIAN_AMBIENT_LAND
        })
    }

    fn ambient_sound_interval(&self) -> i32 {
        GUARDIAN_AMBIENT_SOUND_INTERVAL
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        level.is_unobstructed_by_entities(self)
    }
}

impl Monster for GuardianEntity {}

impl PathfinderMob for GuardianEntity {
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        self.guardian_walk_target_value(pos, level)
    }
}

#[cfg(test)]
mod tests;
