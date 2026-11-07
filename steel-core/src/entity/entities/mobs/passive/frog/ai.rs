//! Vanilla `FrogAi`, `Croak`, `ShootTongue` and `FrogAttackablesSensor`.

use std::sync::Arc;

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_blocks, vanilla_entities,
};
use steel_utils::{BlockPos, Identifier};

use super::FrogEntity;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::gate::{GateBehavior, OrderPolicy, RunningPolicy};
use crate::entity::ai::brain::behaviors::utils::look_at_entity;
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, CountDownCooldownTicks, FollowTemptation, LongJumpMidJump,
    LongJumpToRandomPos, LookAtTargetSink, MoveToTargetSink, UniformInt,
    default_acceptable_landing_spot, is_breeding, random_stroll, random_swim, run_one,
    set_entity_look_target_sometimes, set_walk_target_from_look_target, start_attacking,
    stop_attacking_if_target_invalid, try_find_land, try_find_land_near_liquid,
    try_lay_spawn_on_fluid_near_land,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, BREED_TARGET, IS_IN_WATER, IS_PANICKING, IS_PREGNANT, LONG_JUMP_COOLDOWN_TICKS,
    LONG_JUMP_MID_JUMP, LOOK_TARGET, NEAREST_ATTACKABLE, NEAREST_VISIBLE_LIVING_ENTITIES,
    TEMPTATION_COOLDOWN_TICKS, TEMPTING_PLAYER, UNREACHABLE_TONGUE_TARGETS, WALK_TARGET,
    WalkTarget,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable;
use crate::entity::ai::brain::sensors::{
    HurtBySensor, IsInWaterSensor, NearestLivingEntitySensor, TemptingSensor,
};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, Memories, MemoryStatus, Sensor};
use crate::entity::ai::path::{PathType, PathfindingContext};
use crate::entity::ai::walk::WalkPathEvaluator;
use crate::entity::{Entity, EntityPose, LivingEntity, Mob, PathfinderMob, RemovalReason};
use crate::world::World;

const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_ON_LAND: f32 = 1.0;
const SPEED_MULTIPLIER_IN_WATER: f32 = 0.75;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
/// Vanilla `FrogAi.TIME_BETWEEN_LONG_JUMPS`.
pub(super) const TIME_BETWEEN_LONG_JUMPS: UniformInt = UniformInt::of(100, 140);
/// Vanilla `FrogAi.MAX_LONG_JUMP_HEIGHT`.
const MAX_LONG_JUMP_HEIGHT: i32 = 2;
/// Vanilla `FrogAi.MAX_LONG_JUMP_WIDTH`.
const MAX_LONG_JUMP_WIDTH: i32 = 4;
/// Vanilla `FrogAi.MAX_JUMP_VELOCITY_MULTIPLIER`.
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 3.571_428_8;
/// Vanilla `LongJumpToPreferredBlock`'s chance to look for a lily pad.
const PREFERRED_BLOCKS_CHANCE: f32 = 0.5;
const LOOK_AT_PLAYER_DISTANCE: f32 = 6.0;
const LOOK_AT_PLAYER_INTERVAL: UniformInt = UniformInt::of(30, 60);
/// Vanilla `FrogAttackablesSensor.TARGET_DETECTION_DISTANCE`.
const TARGET_DETECTION_DISTANCE: f64 = 10.0;

static FROG_PREFER_JUMP_TO: Identifier = BlockTag::FROG_PREFER_JUMP_TO;
static FROG_TRIES_TO_FIND_LAND_NEAR: Identifier = FluidTag::FROG_TRIES_TO_FIND_LAND_NEAR;

/// The sensors vanilla's frog `Brain.provider` lists.
pub(super) fn sensors() -> Vec<Box<dyn Sensor<FrogEntity>>> {
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(HurtBySensor),
        Box::new(FrogAttackablesSensor),
        Box::new(TemptingSensor::<FrogEntity>::for_animal()),
        Box::new(IsInWaterSensor),
    ]
}

/// Vanilla `FrogAi.getActivities`.
pub(super) fn activities() -> Vec<ActivityData<FrogEntity>> {
    vec![
        core_activity(),
        idle_activity(),
        swim_activity(),
        lay_spawn_activity(),
        tongue_activity(),
        jump_activity(),
    ]
}

/// Vanilla `FrogAi.updateActivity`.
pub(super) fn update_activity(body: &FrogEntity) {
    body.brain().set_active_activity_to_first_valid(&[
        Activity::TONGUE,
        Activity::LAY_SPAWN,
        Activity::LONG_JUMP,
        Activity::SWIM,
        Activity::IDLE,
    ]);
}

/// Vanilla `Frog.canEat`: small enough frog food (a size 1 cube at most).
#[must_use]
pub fn can_eat(entity: &dyn LivingEntity) -> bool {
    if entity
        .as_cube_mob()
        .is_some_and(|cube_mob| cube_mob.get_size() != 1)
    {
        return false;
    }
    REGISTRY
        .entity_types
        .is_in_tag(entity.entity_type(), &EntityTypeTag::FROG_FOOD)
}

fn look_at_players() -> impl BehaviorControl<FrogEntity> {
    set_entity_look_target_sometimes(
        LOOK_AT_PLAYER_DISTANCE,
        LOOK_AT_PLAYER_INTERVAL,
        |entity: &dyn LivingEntity| entity.entity_type() == &vanilla_entities::PLAYER,
    )
}

/// Vanilla `StartAttacking.create(canAttack, NEAREST_ATTACKABLE)`.
fn start_attacking_nearest() -> impl BehaviorControl<FrogEntity> {
    start_attacking(|_world: &Arc<World>, body: &FrogEntity| {
        if is_breeding(body) {
            return None;
        }
        body.brain().memories().lock().get(&NEAREST_ATTACKABLE)
    })
}

/// Vanilla `BehaviorBuilder.triggerIf`.
fn trigger_if(
    predicate: impl Fn(&FrogEntity) -> bool + Send + 'static,
) -> impl BehaviorControl<FrogEntity> {
    OneShot::new(
        Vec::new(),
        move |_world: &Arc<World>, body: &FrogEntity, _time: i64| predicate(body),
    )
}

fn core_activity() -> ActivityData<FrogEntity> {
    ActivityData::with_first_priority(
        Activity::CORE,
        0,
        vec![
            Box::new(TimedBehavior::new(AnimalPanic::new(
                SPEED_MULTIPLIER_WHEN_PANICKING,
            ))),
            Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
            Box::new(TimedBehavior::new(MoveToTargetSink::new())),
            Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                &TEMPTATION_COOLDOWN_TICKS,
            ))),
            Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                &LONG_JUMP_COOLDOWN_TICKS,
            ))),
        ],
    )
}

fn idle_activity() -> ActivityData<FrogEntity> {
    ActivityData::new(
        Activity::IDLE,
        vec![
            (0, Box::new(look_at_players())),
            (
                0,
                Box::new(TimedBehavior::new(AnimalMakeLove::new(
                    &vanilla_entities::FROG,
                ))),
            ),
            (
                1,
                Box::new(TimedBehavior::new(FollowTemptation::new(|_| {
                    SPEED_MULTIPLIER_WHEN_TEMPTED
                }))),
            ),
            (2, Box::new(start_attacking_nearest())),
            (3, Box::new(try_find_land(6, SPEED_MULTIPLIER_WHEN_IDLING))),
            (
                4,
                Box::new(run_one(
                    vec![MemoryRequirement::new(
                        &WALK_TARGET,
                        MemoryStatus::ValueAbsent,
                    )],
                    vec![
                        (
                            Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true)),
                            1,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target(
                                |_| true,
                                |_| SPEED_MULTIPLIER_ON_LAND,
                                3,
                            )),
                            1,
                        ),
                        (Box::new(TimedBehavior::new(Croak::new())), 3),
                        (Box::new(trigger_if(Entity::on_ground)), 2),
                    ],
                )),
            ),
        ],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&LONG_JUMP_MID_JUMP, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&IS_IN_WATER, MemoryStatus::ValueAbsent),
    ])
}

fn swim_activity() -> ActivityData<FrogEntity> {
    ActivityData::new(
        Activity::SWIM,
        vec![
            (0, Box::new(look_at_players())),
            (
                1,
                Box::new(TimedBehavior::new(FollowTemptation::new(|_| {
                    SPEED_MULTIPLIER_WHEN_TEMPTED
                }))),
            ),
            (2, Box::new(start_attacking_nearest())),
            (3, Box::new(try_find_land(8, 1.5))),
            (
                5,
                Box::new(GateBehavior::new(
                    vec![MemoryRequirement::new(
                        &WALK_TARGET,
                        MemoryStatus::ValueAbsent,
                    )],
                    Vec::new(),
                    OrderPolicy::Ordered,
                    RunningPolicy::TryAll,
                    vec![
                        (Box::new(random_swim(SPEED_MULTIPLIER_IN_WATER)), 1),
                        (Box::new(random_stroll(SPEED_MULTIPLIER_ON_LAND, true)), 1),
                        (
                            Box::new(set_walk_target_from_look_target(
                                |_| true,
                                |_| SPEED_MULTIPLIER_ON_LAND,
                                3,
                            )),
                            1,
                        ),
                        (Box::new(trigger_if(Entity::is_in_water)), 5),
                    ],
                )),
            ),
        ],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&LONG_JUMP_MID_JUMP, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&IS_IN_WATER, MemoryStatus::ValuePresent),
    ])
}

fn lay_spawn_activity() -> ActivityData<FrogEntity> {
    ActivityData::new(
        Activity::LAY_SPAWN,
        vec![
            (0, Box::new(look_at_players())),
            (1, Box::new(start_attacking_nearest())),
            (
                2,
                Box::new(try_find_land_near_liquid(
                    8,
                    SPEED_MULTIPLIER_ON_LAND,
                    &FROG_TRIES_TO_FIND_LAND_NEAR,
                )),
            ),
            (
                3,
                Box::new(try_lay_spawn_on_fluid_near_land(&vanilla_blocks::FROGSPAWN)),
            ),
            (
                4,
                Box::new(run_one(
                    Vec::new(),
                    vec![
                        (
                            Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true)),
                            2,
                        ),
                        (
                            Box::new(set_walk_target_from_look_target(
                                |_| true,
                                |_| SPEED_MULTIPLIER_ON_LAND,
                                3,
                            )),
                            1,
                        ),
                        (Box::new(TimedBehavior::new(Croak::new())), 2),
                        (Box::new(trigger_if(Entity::on_ground)), 1),
                    ],
                )),
            ),
        ],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&LONG_JUMP_MID_JUMP, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&IS_PREGNANT, MemoryStatus::ValuePresent),
    ])
}

fn jump_activity() -> ActivityData<FrogEntity> {
    ActivityData::new(
        Activity::LONG_JUMP,
        vec![
            (
                0,
                Box::new(TimedBehavior::new(LongJumpMidJump::new(
                    TIME_BETWEEN_LONG_JUMPS,
                    &sound_events::ENTITY_FROG_STEP,
                ))),
            ),
            (
                1,
                Box::new(TimedBehavior::new(LongJumpToRandomPos::to_preferred_block(
                    TIME_BETWEEN_LONG_JUMPS,
                    MAX_LONG_JUMP_HEIGHT,
                    MAX_LONG_JUMP_WIDTH,
                    MAX_JUMP_VELOCITY_MULTIPLIER,
                    |_| &sound_events::ENTITY_FROG_LONG_JUMP,
                    &FROG_PREFER_JUMP_TO,
                    PREFERRED_BLOCKS_CHANCE,
                    is_acceptable_landing_spot,
                ))),
            ),
        ],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&TEMPTING_PLAYER, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&LONG_JUMP_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&IS_IN_WATER, MemoryStatus::ValueAbsent),
    ])
}

fn tongue_activity() -> ActivityData<FrogEntity> {
    ActivityData::with_first_priority(
        Activity::TONGUE,
        0,
        vec![
            Box::new(stop_attacking_if_target_invalid(
                |_world: &Arc<World>, _body: &FrogEntity, _target| {},
            )),
            Box::new(TimedBehavior::new(ShootTongue::new(
                &sound_events::ENTITY_FROG_TONGUE,
                &sound_events::ENTITY_FROG_EAT,
            ))),
        ],
    )
    .while_memory_present(&ATTACK_TARGET)
}

/// Vanilla `FrogAi.isAcceptableLandingSpot`: never into fluid, always onto or
/// next to a lily pad, and otherwise like any long jump except that trapdoors
/// are fine.
fn is_acceptable_landing_spot(body: &FrogEntity, target_pos: BlockPos) -> bool {
    let Some(world) = body.level() else {
        return false;
    };
    let below = target_pos.below();
    let has_fluid = |pos: BlockPos| {
        !world
            .get_block_state(pos)
            .get_fluid_state()
            .fluid_id
            .is_empty
    };
    if has_fluid(target_pos) || has_fluid(below) || has_fluid(target_pos.above()) {
        return false;
    }
    let state = world.get_block_state(target_pos);
    let state_below = world.get_block_state(below);
    if state.get_block().has_tag(&FROG_PREFER_JUMP_TO)
        || state_below.get_block().has_tag(&FROG_PREFER_JUMP_TO)
    {
        return true;
    }
    let mut context = PathfindingContext::new(world.as_ref(), body.block_position());
    let path_type = WalkPathEvaluator::path_type_static(&mut context, target_pos);
    let path_type_below = WalkPathEvaluator::path_type_static(&mut context, below);
    if path_type == PathType::Trapdoor || state.is_air() && path_type_below == PathType::Trapdoor {
        return true;
    }
    default_acceptable_landing_spot(body, target_pos)
}

/// Vanilla `Croak.CROAK_TICKS`.
const CROAK_TICKS: i32 = 60;
/// Vanilla `Croak.TIME_OUT_DURATION`.
const CROAK_TIME_OUT_DURATION: i32 = 100;
const CROAK_ENTRY: [MemoryRequirement; 1] = [MemoryRequirement::new(
    &WALK_TARGET,
    MemoryStatus::ValueAbsent,
)];

/// Vanilla `Croak`: puffs up its throat for three seconds while standing
/// still on land.
pub(super) struct Croak {
    croak_counter: i32,
}

impl Croak {
    pub(super) const fn new() -> Self {
        Self { croak_counter: 0 }
    }
}

impl Behavior<FrogEntity> for Croak {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &CROAK_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (CROAK_TIME_OUT_DURATION, CROAK_TIME_OUT_DURATION)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &FrogEntity) -> bool {
        body.pose() == EntityPose::Standing
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &FrogEntity, _time: i64) -> bool {
        self.croak_counter < CROAK_TICKS
    }

    fn start(&mut self, _world: &Arc<World>, body: &FrogEntity, _time: i64) {
        if !body.is_in_water() && !body.is_in_lava() {
            body.set_pose(EntityPose::Croaking);
            self.croak_counter = 0;
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &FrogEntity, _time: i64) {
        body.set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, _world: &Arc<World>, _body: &FrogEntity, _time: i64) {
        self.croak_counter += 1;
    }
}

/// Vanilla `ShootTongue.TIME_OUT_DURATION`.
const TONGUE_TIME_OUT_DURATION: i32 = 100;
/// Vanilla `ShootTongue.CATCH_ANIMATION_DURATION`.
const CATCH_ANIMATION_DURATION: i32 = 6;
/// Vanilla `ShootTongue.TONGUE_ANIMATION_DURATION`.
const TONGUE_ANIMATION_DURATION: i32 = 10;
/// Vanilla `ShootTongue.EATING_DISTANCE`.
const EATING_DISTANCE: f32 = 1.75;
/// Vanilla `ShootTongue.EATING_MOVEMENT_FACTOR`.
const EATING_MOVEMENT_FACTOR: f64 = 0.75;
/// Vanilla `ShootTongue.UNREACHABLE_TONGUE_TARGETS_COOLDOWN_DURATION`.
const UNREACHABLE_TONGUE_TARGETS_COOLDOWN_DURATION: i64 = 100;
/// Vanilla `ShootTongue.MAX_UNREACHBLE_TONGUE_TARGETS_IN_MEMORY`.
const MAX_UNREACHABLE_TONGUE_TARGETS_IN_MEMORY: usize = 5;
/// Vanilla `ShootTongue`'s walk speed towards its prey.
const TONGUE_WALK_SPEED: f32 = 2.0;
/// Ticks between two walk target refreshes while closing in.
const RECALCULATE_PATH_TICKS: i32 = 10;

const SHOOT_TONGUE_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
];

/// Vanilla `ShootTongue.State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TongueState {
    MoveToTarget,
    CatchAnimation,
    EatAnimation,
    Done,
}

/// Vanilla `ShootTongue`: walks up to its prey, pulls it in with its tongue
/// and eats it.
pub(super) struct ShootTongue {
    eat_animation_timer: i32,
    calculate_path_counter: i32,
    tongue_sound: SoundEventRef,
    eat_sound: SoundEventRef,
    state: TongueState,
}

impl ShootTongue {
    pub(super) const fn new(tongue_sound: SoundEventRef, eat_sound: SoundEventRef) -> Self {
        Self {
            eat_animation_timer: 0,
            calculate_path_counter: 0,
            tongue_sound,
            eat_sound,
            state: TongueState::Done,
        }
    }

    /// Vanilla `canPathfindToTarget`.
    fn can_pathfind_to_target(body: &FrogEntity, target: &dyn Entity) -> bool {
        body.create_path_to(target.block_position(), 0)
            .is_some_and(|path| path.dist_to_target() < EATING_DISTANCE)
    }

    /// Vanilla `addUnreachableTargetToMemory`.
    fn add_unreachable_target_to_memory(body: &FrogEntity, target: &dyn Entity) {
        let mut memories = body.brain().memories().lock();
        let mut unreachable_targets = memories
            .get(&UNREACHABLE_TONGUE_TARGETS)
            .unwrap_or_default();
        let uuid = target.uuid();
        let should_add = !unreachable_targets.contains(&uuid);
        if unreachable_targets.len() == MAX_UNREACHABLE_TONGUE_TARGETS_IN_MEMORY && should_add {
            unreachable_targets.remove(0);
        }
        if should_add {
            unreachable_targets.push(uuid);
        }
        memories.set_with_expiry(
            &UNREACHABLE_TONGUE_TARGETS,
            unreachable_targets,
            UNREACHABLE_TONGUE_TARGETS_COOLDOWN_DURATION,
        );
    }

    fn walk_to(body: &FrogEntity, target: &dyn Entity) {
        body.brain().memories().lock().set(
            &WALK_TARGET,
            Some(WalkTarget::to_point(
                target.position(),
                TONGUE_WALK_SPEED,
                0,
            )),
        );
    }

    /// Vanilla `eatEntity`.
    fn eat_entity(&self, world: &Arc<World>, body: &FrogEntity) {
        world.play_sound_at(
            self.eat_sound,
            SoundSource::Neutral,
            body.position(),
            2.0,
            1.0,
            None,
        );
        let Some(target) = body.tongue_target() else {
            return;
        };
        if !target.is_alive() {
            return;
        }
        let Some(frog) = world.get_entity_by_id(body.id()) else {
            return;
        };
        let _hurt = body.do_hurt_target(&frog, world, &target);
        if !target.is_alive() {
            target.set_removed(RemovalReason::Killed);
        }
    }

    #[cfg(test)]
    pub(super) const fn state(&self) -> TongueState {
        self.state
    }
}

/// Vanilla `Vec3.normalize`: zero below a length of `1.0E-5`.
fn vanilla_normalize(vector: DVec3) -> DVec3 {
    let length = vector.length();
    if length < 1.0e-5 {
        DVec3::ZERO
    } else {
        vector / length
    }
}

impl Behavior<FrogEntity> for ShootTongue {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SHOOT_TONGUE_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (TONGUE_TIME_OUT_DURATION, TONGUE_TIME_OUT_DURATION)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &FrogEntity) -> bool {
        let Some(target) = body.brain().memories().lock().get(&ATTACK_TARGET) else {
            return false;
        };
        let can_pathfind_to_target = Self::can_pathfind_to_target(body, target.as_ref());
        if !can_pathfind_to_target {
            body.brain().memories().lock().erase(&ATTACK_TARGET);
            Self::add_unreachable_target_to_memory(body, target.as_ref());
        }
        can_pathfind_to_target
            && body.pose() != EntityPose::Croaking
            && target.as_living_entity().is_some_and(can_eat)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &FrogEntity, _time: i64) -> bool {
        let memories = body.brain().memories().lock();
        memories.has_value(&ATTACK_TARGET)
            && self.state != TongueState::Done
            && !memories.has_value(&IS_PANICKING)
    }

    fn start(&mut self, _world: &Arc<World>, body: &FrogEntity, _time: i64) {
        let Some(target) = body.brain().memories().lock().get(&ATTACK_TARGET) else {
            return;
        };
        look_at_entity(body, &target);
        body.set_tongue_target(target.as_ref());
        Self::walk_to(body, target.as_ref());
        self.calculate_path_counter = RECALCULATE_PATH_TICKS;
        self.state = TongueState::MoveToTarget;
    }

    fn stop(&mut self, _world: &Arc<World>, body: &FrogEntity, _time: i64) {
        body.brain().memories().lock().erase(&ATTACK_TARGET);
        body.erase_tongue_target();
        body.set_pose(EntityPose::Standing);
    }

    fn tick(&mut self, world: &Arc<World>, body: &FrogEntity, _time: i64) {
        let Some(target) = body.brain().memories().lock().get(&ATTACK_TARGET) else {
            return;
        };
        body.set_tongue_target(target.as_ref());
        match self.state {
            TongueState::MoveToTarget => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "vanilla compares the float distance"
                )]
                let distance = target.position().distance(body.position()) as f32;
                if distance < EATING_DISTANCE {
                    world.play_sound_at(
                        self.tongue_sound,
                        SoundSource::Neutral,
                        body.position(),
                        2.0,
                        1.0,
                        None,
                    );
                    body.set_pose(EntityPose::UsingTongue);
                    target.set_velocity(
                        vanilla_normalize(body.position() - target.position())
                            * EATING_MOVEMENT_FACTOR,
                    );
                    self.eat_animation_timer = 0;
                    self.state = TongueState::CatchAnimation;
                } else if self.calculate_path_counter <= 0 {
                    Self::walk_to(body, target.as_ref());
                    self.calculate_path_counter = RECALCULATE_PATH_TICKS;
                } else {
                    self.calculate_path_counter -= 1;
                }
            }
            TongueState::CatchAnimation => {
                let timer = self.eat_animation_timer;
                self.eat_animation_timer += 1;
                if timer >= CATCH_ANIMATION_DURATION {
                    self.state = TongueState::EatAnimation;
                    self.eat_entity(world, body);
                }
            }
            TongueState::EatAnimation => {
                if self.eat_animation_timer >= TONGUE_ANIMATION_DURATION {
                    self.state = TongueState::Done;
                } else {
                    self.eat_animation_timer += 1;
                }
            }
            TongueState::Done => {}
        }
    }
}

/// Vanilla `FrogAttackablesSensor`: the closest visible frog food within 10
/// blocks that has not recently proven unreachable.
struct FrogAttackablesSensor;

impl Sensor<FrogEntity> for FrogAttackablesSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_ATTACKABLE);
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
        memories.register(&UNREACHABLE_TONGUE_TARGETS);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &FrogEntity) {
        let memories = body.brain().memories();
        let (visible, unreachable) = {
            let memories = memories.lock();
            (
                memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES),
                memories
                    .get(&UNREACHABLE_TONGUE_TARGETS)
                    .unwrap_or_default(),
            )
        };
        let nearest = visible.and_then(|visible| {
            visible.find_closest(|entity| {
                entity.as_living_entity().is_some_and(|mob| {
                    is_entity_attackable(world, body, mob)
                        && can_eat(mob)
                        && !unreachable.contains(&mob.uuid())
                        && mob.position().distance_squared(body.position())
                            < TARGET_DETECTION_DISTANCE * TARGET_DETECTION_DISTANCE
                })
            })
        });
        memories.lock().set(&NEAREST_ATTACKABLE, nearest);
    }
}
