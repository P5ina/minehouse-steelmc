//! Vanilla `BreezeAi`, `BreezeUtil`, the breeze fight behaviors (`Shoot`,
//! `LongJump`, `ShootWhenStuck`, `Slide`) and `BreezeAttackEntitySensor`.

use std::sync::Arc;

use glam::DVec3;
use rand::seq::SliceRandom as _;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_data::EntityPose;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_mob_effects,
};
use steel_utils::BlockPos;
use steel_utils::random::{Random as _, legacy_random::LegacyRandom};

use super::BreezeEntity;
use crate::enchantment_helper;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    DoNothing, LookAtTargetSink, MoveToTargetSink, Swim, calculate_jump_vector_for_angle,
    random_stroll, run_one, start_attacking, stop_attacking_if_target_invalid_when,
};
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, BREEZE_JUMP_COOLDOWN, BREEZE_JUMP_INHALING, BREEZE_JUMP_TARGET,
    BREEZE_LEAVING_WATER, BREEZE_SHOOT, BREEZE_SHOOT_CHARGING, BREEZE_SHOOT_COOLDOWN,
    BREEZE_SHOOT_RECOVERING, HURT_BY, NEAREST_ATTACKABLE, NEAREST_LIVING_ENTITIES, WALK_TARGET,
    WalkTarget,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable;
use crate::entity::ai::brain::sensors::{HurtBySensor, NearestLivingEntitySensor, PlayerSensor};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, Memories, MemoryStatus, Sensor};
use crate::entity::ai::goal::random_pos::default_random_pos_away;
use crate::entity::entities::BreezeWindChargeEntity;
use crate::entity::entity_selector::no_creative_or_spectator;
use crate::entity::spawn_placements::is_block_dangerous;
use crate::entity::{Entity, EntityAnchor, LivingEntity, Projectile, SharedEntity, next_entity_id};
use crate::fluid::FluidStateExt as _;
use crate::inventory::prelude::ItemStack;
use crate::world::{ClipBlockShape, ClipFluid, World};

/// Vanilla `BreezeAi.SPEED_MULTIPLIER_WHEN_SLIDING`.
const SPEED_MULTIPLIER_WHEN_SLIDING: f32 = 0.6;
/// Vanilla `BreezeAi.TICKS_TO_REMEMBER_SEEN_TARGET`.
const TICKS_TO_REMEMBER_SEEN_TARGET: i32 = 100;
/// Vanilla `BreezeUtil.MAX_LINE_OF_SIGHT_TEST_RANGE`.
const MAX_LINE_OF_SIGHT_TEST_RANGE: f64 = 50.0;
/// How long a breeze wants to shoot after sliding or getting stuck.
const SHOOT_AFTER_SLIDE_TICKS: i64 = 60;
/// How long a breeze wants to shoot after landing a long jump.
const SHOOT_AFTER_JUMP_TICKS: i64 = 100;

/// Vanilla `Shoot.ATTACK_RANGE_MAX_SQRT`.
const ATTACK_RANGE_MAX_SQR: f64 = 256.0;
/// Vanilla `Shoot.UNCERTAINTY_BASE` and `UNCERTAINTY_MULTIPLIER`.
const UNCERTAINTY_BASE: i32 = 5;
const UNCERTAINTY_MULTIPLIER: i32 = 4;
/// Vanilla `Shoot.PROJECTILE_MOVEMENT_SCALE`.
const PROJECTILE_MOVEMENT_SCALE: f32 = 0.7;
/// Vanilla `Shoot.SHOOT_INITIAL_DELAY_TICKS`, `SHOOT_RECOVER_DELAY_TICKS` and
/// `SHOOT_COOLDOWN_TICKS`.
const SHOOT_INITIAL_DELAY_TICKS: i32 = 15;
const SHOOT_RECOVER_DELAY_TICKS: i32 = 4;
const SHOOT_COOLDOWN_TICKS: i64 = 10;

/// Vanilla `LongJump.REQUIRED_AIR_BLOCKS_ABOVE`.
const REQUIRED_AIR_BLOCKS_ABOVE: i32 = 4;
/// Vanilla `LongJump.JUMP_COOLDOWN_TICKS` and `JUMP_COOLDOWN_WHEN_HURT_TICKS`.
const JUMP_COOLDOWN_TICKS: i64 = 10;
const JUMP_COOLDOWN_WHEN_HURT_TICKS: i64 = 2;
/// Vanilla `LongJump.INHALING_DURATION_TICKS`.
const INHALING_DURATION_TICKS: i64 = 10;
/// Vanilla `LongJump.MAX_JUMP_VELOCITY_MULTIPLIER`.
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 0.058_333_334;
/// Vanilla `LongJump.ALLOWED_ANGLES`.
const ALLOWED_ANGLES: [i32; 5] = [40, 55, 60, 75, 80];
/// Vanilla `LongJump` run time.
const LONG_JUMP_DURATION: i32 = 200;
/// How far `snapToSurface` looks up and down for a landing block.
const SNAP_TO_SURFACE_DISTANCE: f64 = 10.0;

/// Vanilla `BreezeAi.getActivities`.
pub(super) fn activities() -> Vec<ActivityData<BreezeEntity>> {
    vec![core_activity(), idle_activity(), fight_activity()]
}

/// Vanilla `Breeze.BRAIN_PROVIDER` sensors.
pub(super) fn sensors() -> Vec<Box<dyn Sensor<BreezeEntity>>> {
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(HurtBySensor),
        Box::new(PlayerSensor),
        Box::new(BreezeAttackEntitySensor),
    ]
}

fn core_activity() -> ActivityData<BreezeEntity> {
    ActivityData::with_first_priority(
        Activity::CORE,
        0,
        vec![
            Box::new(TimedBehavior::new(Swim::new(0.8))),
            Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
        ],
    )
}

fn idle_activity() -> ActivityData<BreezeEntity> {
    let wander: Vec<(Box<dyn BehaviorControl<BreezeEntity>>, i32)> = vec![
        (Box::new(DoNothing::new(20, 100)), 1),
        (
            Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_SLIDING, true)),
            2,
        ),
    ];
    ActivityData::with_first_priority(
        Activity::IDLE,
        0,
        vec![
            Box::new(start_attacking(
                |_world: &Arc<World>, breeze: &BreezeEntity| {
                    breeze.memories().lock().get(&NEAREST_ATTACKABLE)
                },
            )),
            Box::new(start_attacking(
                |_world: &Arc<World>, breeze: &BreezeEntity| breeze.hurt_by(),
            )),
            Box::new(TimedBehavior::new(SlideToTargetSink {
                sink: MoveToTargetSink::with_timeout(20, 40),
            })),
            Box::new(run_one(Vec::new(), wander)),
        ],
    )
}

fn fight_activity() -> ActivityData<BreezeEntity> {
    // Vanilla `Sensor.wasEntityAttackableLastNTicks(body, 100).negate()`: the
    // `rememberPositives` counter belongs to this one behavior.
    let mut positives_left = 0;
    ActivityData::with_first_priority(
        Activity::FIGHT,
        0,
        vec![
            Box::new(stop_attacking_if_target_invalid_when(
                move |world: &Arc<World>, breeze: &BreezeEntity, target: &dyn LivingEntity| {
                    if is_entity_attackable(world, breeze, target) {
                        positives_left = TICKS_TO_REMEMBER_SEEN_TARGET;
                        false
                    } else {
                        positives_left -= 1;
                        positives_left < 0
                    }
                },
                |_, _, _| {},
            )),
            Box::new(TimedBehavior::new(Shoot)),
            Box::new(TimedBehavior::new(LongJump)),
            Box::new(TimedBehavior::new(ShootWhenStuck)),
            Box::new(TimedBehavior::new(Slide)),
        ],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
        MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    ])
}

/// Vanilla `BreezeAi.updateActivity`.
pub(super) fn update_activity(breeze: &BreezeEntity) {
    breeze
        .brain
        .set_active_activity_to_first_valid(&[Activity::FIGHT, Activity::IDLE]);
}

fn attack_target(breeze: &BreezeEntity) -> Option<SharedEntity> {
    breeze.memories().lock().get(&ATTACK_TARGET)
}

fn set_unit_with_expiry(breeze: &BreezeEntity, memory: &'static MemoryModuleType<()>, ticks: i64) {
    breeze.memories().lock().set_with_expiry(memory, (), ticks);
}

/// Vanilla `Entity.getY(scale)`.
fn y_at(entity: &dyn Entity, scale: f64) -> f64 {
    entity.position().y + f64::from(entity.base().dimensions().height) * scale
}

/// Vanilla `Vec3.atCenterOf`.
fn center_of(pos: BlockPos) -> DVec3 {
    DVec3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y) + 0.5,
        f64::from(pos.0.z) + 0.5,
    )
}

/// Vanilla `BreezeUtil.randomPointBehindTarget`.
pub(super) fn random_point_behind_target(enemy: &dyn LivingEntity) -> DVec3 {
    let mut random = LegacyRandom::from_seed(rand::random());
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla casts the gaussian to float"
    )]
    let spread = random.next_gaussian() as f32 * 90.0 / 2.0;
    let view_angle = enemy.y_head_rot() + 180.0 + spread;
    let radius = lerp(rand::random::<f32>(), 4.0, 8.0);
    // Vanilla `Vec3.directionFromRotation(0, viewAngle)`.
    let yaw = -view_angle.to_radians();
    let direction = DVec3::new(f64::from(yaw.sin()), 0.0, f64::from(yaw.cos()));
    enemy.position() + direction * f64::from(radius)
}

fn lerp(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

/// Vanilla `BreezeUtil.hasLineOfSight`.
pub(super) fn has_line_of_sight(world: &World, breeze: &BreezeEntity, target: DVec3) -> bool {
    let from = breeze.position();
    let max_range = MAX_LINE_OF_SIGHT_TEST_RANGE.max(
        breeze
            .attributes()
            .lock()
            .required_value(vanilla_attributes::FOLLOW_RANGE),
    );
    if target.distance(from) > max_range {
        return false;
    }
    // TODO: vanilla clips with the breeze's collision context; Steel's clip
    // uses the empty context, which only differs for entity-dependent shapes.
    world
        .clip(from, target, ClipBlockShape::Collider, ClipFluid::None)
        .miss
}

fn follow_range(breeze: &BreezeEntity) -> f64 {
    breeze
        .attributes()
        .lock()
        .required_value(vanilla_attributes::FOLLOW_RANGE)
}

/// Vanilla `BreezeAi.SlideToTargetSink`: a `MoveToTargetSink` that slides,
/// then wants to shoot.
pub(super) struct SlideToTargetSink {
    sink: MoveToTargetSink,
}

impl Behavior<BreezeEntity> for SlideToTargetSink {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        <MoveToTargetSink as Behavior<BreezeEntity>>::entry_condition(&self.sink)
    }

    fn duration(&self) -> (i32, i32) {
        <MoveToTargetSink as Behavior<BreezeEntity>>::duration(&self.sink)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &BreezeEntity) -> bool {
        self.sink.check_extra_start_conditions(world, body)
    }

    fn start(&mut self, world: &Arc<World>, body: &BreezeEntity, time: i64) {
        self.sink.start(world, body, time);
        body.play_sound(&sound_events::ENTITY_BREEZE_SLIDE, 1.0, 1.0);
        body.set_pose(EntityPose::Sliding);
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &BreezeEntity, time: i64) -> bool {
        self.sink.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &BreezeEntity, time: i64) {
        self.sink.tick(world, body, time);
    }

    fn stop(&mut self, world: &Arc<World>, body: &BreezeEntity, time: i64) {
        self.sink.stop(world, body, time);
        body.set_pose(EntityPose::Standing);
        if body.memories().lock().has_value(&ATTACK_TARGET) {
            set_unit_with_expiry(body, &BREEZE_SHOOT, SHOOT_AFTER_SLIDE_TICKS);
        }
    }

    fn timed_out(&self, time: i64, end_time: i64) -> bool {
        <MoveToTargetSink as Behavior<BreezeEntity>>::timed_out(&self.sink, time, end_time)
    }
}

const SHOOT_ENTRY: [MemoryRequirement; 7] = [
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&BREEZE_SHOOT_COOLDOWN, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_SHOOT_CHARGING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_SHOOT_RECOVERING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_SHOOT, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_JUMP_TARGET, MemoryStatus::ValueAbsent),
];

/// Vanilla `Shoot`: inhales, then fires a breeze wind charge at the target.
pub(super) struct Shoot;

impl Shoot {
    fn fire(world: &Arc<World>, breeze: &BreezeEntity, target: &SharedEntity) {
        let Some(shooter) = world.get_entity_by_id(breeze.id()) else {
            return;
        };
        let breeze_pos = breeze.position();
        let target_pos = target.position();
        let direction = DVec3::new(
            target_pos.x - breeze_pos.x,
            y_at(
                target.as_ref(),
                if target.is_passenger() { 0.8 } else { 0.3 },
            ) - breeze.firing_y_position(),
            target_pos.z - breeze_pos.z,
        );
        let charge = BreezeWindChargeEntity::new(
            &vanilla_entities::BREEZE_WIND_CHARGE,
            next_entity_id(),
            DVec3::new(breeze_pos.x, breeze.firing_y_position(), breeze_pos.z),
            Arc::downgrade(world),
        );
        charge.set_owner_entity(Some(&shooter));
        let uncertainty = UNCERTAINTY_BASE - (world.difficulty() as i32) * UNCERTAINTY_MULTIPLIER;
        #[expect(
            clippy::cast_precision_loss,
            reason = "the uncertainty is a small whole number"
        )]
        charge.shoot(direction, PROJECTILE_MOVEMENT_SCALE, uncertainty as f32);
        let charge: SharedEntity = Arc::new(charge);
        if let Err(error) = world.try_add_entity(Arc::clone(&charge)) {
            log::debug!("breeze wind charge was not added to the world: {error}");
            return;
        }
        enchantment_helper::on_projectile_spawned(
            world,
            &mut ItemStack::empty(),
            charge.as_ref(),
            Some(&shooter),
        );
    }
}

impl Behavior<BreezeEntity> for Shoot {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SHOOT_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        let duration = SHOOT_INITIAL_DELAY_TICKS + 1 + SHOOT_RECOVER_DELAY_TICKS;
        (duration, duration)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, breeze: &BreezeEntity) -> bool {
        if breeze.pose() != EntityPose::Standing {
            return false;
        }
        let Some(target) = attack_target(breeze) else {
            return false;
        };
        let within_range =
            breeze.position().distance_squared(target.position()) < ATTACK_RANGE_MAX_SQR;
        if !within_range {
            breeze.memories().lock().erase(&BREEZE_SHOOT);
        }
        within_range
    }

    fn can_still_use(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) -> bool {
        let memories = breeze.memories().lock();
        memories.has_value(&ATTACK_TARGET) && memories.has_value(&BREEZE_SHOOT)
    }

    fn start(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        if attack_target(breeze).is_some() {
            breeze.set_pose(EntityPose::Shooting);
        }
        set_unit_with_expiry(
            breeze,
            &BREEZE_SHOOT_CHARGING,
            i64::from(SHOOT_INITIAL_DELAY_TICKS),
        );
        breeze.play_sound(&sound_events::ENTITY_BREEZE_INHALE, 1.0, 1.0);
    }

    fn tick(&mut self, world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        let Some(target) = attack_target(breeze) else {
            return;
        };
        breeze.look_at(EntityAnchor::Eyes, target.position());
        let ready = {
            let memories = breeze.memories().lock();
            !memories.has_value(&BREEZE_SHOOT_CHARGING)
                && !memories.has_value(&BREEZE_SHOOT_RECOVERING)
        };
        if ready {
            set_unit_with_expiry(
                breeze,
                &BREEZE_SHOOT_RECOVERING,
                i64::from(SHOOT_RECOVER_DELAY_TICKS),
            );
            Self::fire(world, breeze, &target);
            breeze.play_sound(&sound_events::ENTITY_BREEZE_SHOOT, 1.5, 1.0);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        if breeze.pose() == EntityPose::Shooting {
            breeze.set_pose(EntityPose::Standing);
        }
        set_unit_with_expiry(breeze, &BREEZE_SHOOT_COOLDOWN, SHOOT_COOLDOWN_TICKS);
        breeze.memories().lock().erase(&BREEZE_SHOOT);
    }
}

const LONG_JUMP_ENTRY: [MemoryRequirement; 7] = [
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&BREEZE_JUMP_COOLDOWN, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_JUMP_INHALING, MemoryStatus::Registered),
    MemoryRequirement::new(&BREEZE_JUMP_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&BREEZE_SHOOT, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_LEAVING_WATER, MemoryStatus::Registered),
];

/// Vanilla `LongJump`: inhales, then leaps to a point behind the target.
pub(super) struct LongJump;

impl LongJump {
    /// Vanilla `LongJump.canRun`.
    fn can_run(world: &Arc<World>, breeze: &BreezeEntity) -> bool {
        if !breeze.on_ground() && !breeze.is_in_water() {
            return false;
        }
        // Vanilla `isInFluidDeeperThan(getFluidJumpThreshold(), ENTITY_FLOATABLE)`;
        // the floatable fluid tag is water, as in `Swim`.
        if breeze.is_in_water()
            && breeze.fluid_contact().water_height() > breeze.get_fluid_jump_threshold()
            || breeze.is_in_lava()
        {
            return false;
        }
        if breeze.memories().lock().has_value(&BREEZE_JUMP_TARGET) {
            return true;
        }
        let Some(target) = attack_target(breeze) else {
            return false;
        };
        let distance_sqr = target.position().distance_squared(breeze.position());
        let range = follow_range(breeze);
        if distance_sqr >= range * range {
            breeze.memories().lock().erase(&ATTACK_TARGET);
            return false;
        }
        // Vanilla `tooCloseForJump`: `distanceTo` is a float.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vanilla measures the distance as a float"
        )]
        if distance_sqr.sqrt() as f32 - 4.0 <= 0.0 {
            return false;
        }
        if !Self::can_jump_from_current_position(world, breeze) {
            return false;
        }
        let Some(living) = target.as_living_entity() else {
            return false;
        };
        let Some(target_pos) = Self::snap_to_surface(world, random_point_behind_target(living))
        else {
            return false;
        };
        if is_block_dangerous(
            breeze.entity_type(),
            world.get_block_state(target_pos.below()),
        ) {
            return false;
        }
        if !has_line_of_sight(world, breeze, center_of(target_pos))
            && !has_line_of_sight(world, breeze, center_of(target_pos.above_n(4)))
        {
            return false;
        }
        breeze
            .memories()
            .lock()
            .set(&BREEZE_JUMP_TARGET, Some(target_pos));
        true
    }

    /// Vanilla `LongJump.canJumpFromCurrentPosition`.
    pub(super) fn can_jump_from_current_position(world: &World, breeze: &BreezeEntity) -> bool {
        let current_pos = breeze.block_position();
        if world.get_block_state(current_pos).get_block() == &vanilla_blocks::HONEY_BLOCK {
            return false;
        }
        (1..=REQUIRED_AIR_BLOCKS_ABOVE).all(|i| {
            let state = world.get_block_state(current_pos.above_n(i));
            state.is_air() || state.get_fluid_state().is_water()
        })
    }

    /// Vanilla `LongJump.snapToSurface`.
    fn snap_to_surface(world: &World, target: DVec3) -> Option<BlockPos> {
        let below = world.clip(
            target,
            target - DVec3::new(0.0, SNAP_TO_SURFACE_DISTANCE, 0.0),
            ClipBlockShape::Collider,
            ClipFluid::None,
        );
        if !below.miss {
            return Some(Self::containing(below.location).above());
        }
        let above = world.clip(
            target,
            target + DVec3::new(0.0, SNAP_TO_SURFACE_DISTANCE, 0.0),
            ClipBlockShape::Collider,
            ClipFluid::None,
        );
        (!above.miss).then(|| Self::containing(above.location).above())
    }

    const fn containing(position: DVec3) -> BlockPos {
        BlockPos::containing(position.x, position.y, position.z)
    }

    fn is_finished_inhaling(breeze: &BreezeEntity) -> bool {
        !breeze.memories().lock().has_value(&BREEZE_JUMP_INHALING)
            && breeze.pose() == EntityPose::Inhaling
    }

    fn is_finished_jumping(breeze: &BreezeEntity) -> bool {
        let is_jumping = breeze.pose() == EntityPose::LongJumping;
        let landed_in_water =
            breeze.is_in_water() && !breeze.memories().lock().has_value(&BREEZE_LEAVING_WATER);
        is_jumping && (breeze.on_ground() || landed_in_water)
    }

    /// Vanilla `LongJump.calculateOptimalJumpVector`.
    fn calculate_optimal_jump_vector(
        world: &Arc<World>,
        breeze: &BreezeEntity,
        target_pos: DVec3,
    ) -> Option<DVec3> {
        let mut angles = ALLOWED_ANGLES;
        angles.shuffle(&mut rand::rng());
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vanilla casts the follow range to float"
        )]
        let max_jump_velocity = MAX_JUMP_VELOCITY_MULTIPLIER * follow_range(breeze) as f32;
        angles.into_iter().find_map(|angle| {
            let velocity = calculate_jump_vector_for_angle(
                world,
                breeze,
                target_pos,
                max_jump_velocity,
                angle,
                false,
            )?;
            if breeze.has_mob_effect(vanilla_mob_effects::JUMP_BOOST) {
                let boost =
                    velocity.normalize_or_zero().y * f64::from(breeze.get_jump_boost_power());
                return Some(velocity + DVec3::new(0.0, boost, 0.0));
            }
            Some(velocity)
        })
    }
}

impl Behavior<BreezeEntity> for LongJump {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &LONG_JUMP_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (LONG_JUMP_DURATION, LONG_JUMP_DURATION)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, breeze: &BreezeEntity) -> bool {
        Self::can_run(world, breeze)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) -> bool {
        breeze.pose() != EntityPose::Standing
            && !breeze.memories().lock().has_value(&BREEZE_JUMP_COOLDOWN)
    }

    fn start(&mut self, world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        if !breeze.memories().lock().has_value(&BREEZE_JUMP_INHALING) {
            set_unit_with_expiry(breeze, &BREEZE_JUMP_INHALING, INHALING_DURATION_TICKS);
        }
        breeze.set_pose(EntityPose::Inhaling);
        world.play_sound_at(
            &sound_events::ENTITY_BREEZE_CHARGE,
            SoundSource::Hostile,
            breeze.position(),
            1.0,
            1.0,
            None,
        );
        let jump_target = breeze.memories().lock().get(&BREEZE_JUMP_TARGET);
        if let Some(target_pos) = jump_target {
            breeze.look_at(EntityAnchor::Eyes, center_of(target_pos));
        }
    }

    fn tick(&mut self, world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        let in_water = breeze.is_in_water();
        if !in_water {
            breeze.memories().lock().erase(&BREEZE_LEAVING_WATER);
        }

        if Self::is_finished_inhaling(breeze) {
            let jump_target = breeze.memories().lock().get(&BREEZE_JUMP_TARGET);
            let velocity = jump_target.and_then(|target_pos| {
                let bottom_center = DVec3::new(
                    f64::from(target_pos.0.x) + 0.5,
                    f64::from(target_pos.0.y),
                    f64::from(target_pos.0.z) + 0.5,
                );
                Self::calculate_optimal_jump_vector(world, breeze, bottom_center)
            });
            let Some(velocity) = velocity else {
                breeze.set_pose(EntityPose::Standing);
                return;
            };
            if in_water {
                breeze
                    .memories()
                    .lock()
                    .set(&BREEZE_LEAVING_WATER, Some(()));
            }
            breeze.play_sound(&sound_events::ENTITY_BREEZE_JUMP, 1.0, 1.0);
            breeze.set_pose(EntityPose::LongJumping);
            let (_, pitch) = breeze.rotation();
            breeze.set_rotation((breeze.y_body_rot(), pitch));
            breeze.set_discard_friction(true);
            breeze.set_velocity(velocity);
        } else if Self::is_finished_jumping(breeze) {
            breeze.play_sound(&sound_events::ENTITY_BREEZE_LAND, 1.0, 1.0);
            breeze.set_pose(EntityPose::Standing);
            breeze.set_discard_friction(false);
            let was_hurt = breeze.memories().lock().has_value(&HURT_BY);
            set_unit_with_expiry(
                breeze,
                &BREEZE_JUMP_COOLDOWN,
                if was_hurt {
                    JUMP_COOLDOWN_WHEN_HURT_TICKS
                } else {
                    JUMP_COOLDOWN_TICKS
                },
            );
            set_unit_with_expiry(breeze, &BREEZE_SHOOT, SHOOT_AFTER_JUMP_TICKS);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        let pose = breeze.pose();
        if pose == EntityPose::LongJumping || pose == EntityPose::Inhaling {
            breeze.set_pose(EntityPose::Standing);
        }
        let mut memories = breeze.memories().lock();
        memories.erase(&BREEZE_JUMP_TARGET);
        memories.erase(&BREEZE_JUMP_INHALING);
        memories.erase(&BREEZE_LEAVING_WATER);
    }
}

const SHOOT_WHEN_STUCK_ENTRY: [MemoryRequirement; 5] = [
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&BREEZE_JUMP_INHALING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_JUMP_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_SHOOT, MemoryStatus::ValueAbsent),
];

/// Vanilla `ShootWhenStuck`: shoots when it can neither jump nor slide.
pub(super) struct ShootWhenStuck;

impl Behavior<BreezeEntity> for ShootWhenStuck {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SHOOT_WHEN_STUCK_ENTRY
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, breeze: &BreezeEntity) -> bool {
        breeze.is_passenger()
            || breeze.is_in_water()
            || breeze.has_mob_effect(vanilla_mob_effects::LEVITATION)
    }

    fn start(&mut self, _world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        set_unit_with_expiry(breeze, &BREEZE_SHOOT, SHOOT_AFTER_SLIDE_TICKS);
    }
}

const SLIDE_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_JUMP_COOLDOWN, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREEZE_SHOOT, MemoryStatus::ValueAbsent),
];

/// Vanilla `Slide`: picks a spot around the target to slide to.
pub(super) struct Slide;

impl Slide {
    /// Vanilla `Slide.randomPointInMiddleCircle`.
    fn random_point_in_middle_circle(breeze: &BreezeEntity, enemy: &dyn Entity) -> DVec3 {
        let direction = enemy.position() - breeze.position();
        let delta = rand::random::<f64>();
        let distance = direction.length() - (8.0 + delta * (4.0 - 8.0));
        breeze.position() + direction.normalize_or_zero() * distance
    }
}

impl Behavior<BreezeEntity> for Slide {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SLIDE_ENTRY
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, breeze: &BreezeEntity) -> bool {
        breeze.on_ground() && !breeze.is_in_water() && breeze.pose() == EntityPose::Standing
    }

    fn start(&mut self, world: &Arc<World>, breeze: &BreezeEntity, _time: i64) {
        let Some(enemy) = attack_target(breeze) else {
            return;
        };
        let Some(living_enemy) = enemy.as_living_entity() else {
            return;
        };
        let enemy_pos = enemy.position();
        let mut position = None;
        if breeze.within_inner_circle_range(enemy_pos) {
            position = default_random_pos_away(breeze, 5, 5, enemy_pos).filter(|candidate| {
                has_line_of_sight(world, breeze, *candidate)
                    && enemy_pos.distance_squared(*candidate)
                        > enemy_pos.distance_squared(breeze.position())
            });
        }
        let position = position.unwrap_or_else(|| {
            if rand::random::<bool>() {
                random_point_behind_target(living_enemy)
            } else {
                Self::random_point_in_middle_circle(breeze, enemy.as_ref())
            }
        });
        breeze.memories().lock().set(
            &WALK_TARGET,
            Some(WalkTarget::to_point(
                position,
                SPEED_MULTIPLIER_WHEN_SLIDING,
                1,
            )),
        );
    }
}

/// Vanilla `BreezeAttackEntitySensor`: nearest living entities, plus the
/// first attackable one that is not a creative or spectator player.
pub(super) struct BreezeAttackEntitySensor;

impl Sensor<BreezeEntity> for BreezeAttackEntitySensor {
    fn register_memories(&self, memories: &mut Memories) {
        <NearestLivingEntitySensor as Sensor<BreezeEntity>>::register_memories(
            &NearestLivingEntitySensor,
            memories,
        );
        memories.register(&NEAREST_ATTACKABLE);
    }

    fn do_tick(&mut self, world: &Arc<World>, breeze: &BreezeEntity) {
        NearestLivingEntitySensor.do_tick(world, breeze);
        let nearest = breeze.memories().lock().get(&NEAREST_LIVING_ENTITIES);
        let attackable = nearest.into_iter().flatten().find(|entity| {
            no_creative_or_spectator(entity.as_ref())
                && entity
                    .as_living_entity()
                    .is_some_and(|living| is_entity_attackable(world, breeze, living))
        });
        breeze
            .memories()
            .lock()
            .set(&NEAREST_ATTACKABLE, attackable);
    }
}
