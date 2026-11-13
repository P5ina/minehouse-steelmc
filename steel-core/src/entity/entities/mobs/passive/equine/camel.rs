//! Vanilla `Camel`, `CamelHusk` and `CamelAi`.
//!
//! Camels are horses run by a brain: they carry two riders, sit down now
//! and then, dash on a charged jump and stand up when hurt. Camel husks are
//! the undead camels that never breed and despawn like monsters.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::{CamelEntityData, CamelHuskEntityData};
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_entities,
    vanilla_game_events, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, WorldAabb};

use super::{
    AbstractHorse, AbstractHorseBase, MAX_SPAWN_CLUSTER_SIZE, SOUND_VOLUME, create_offspring, menu,
};
use crate::behavior::blocks::PowderSnowBlock;
use crate::behavior::{BlockCollisionContext, ITEM_BEHAVIORS, InteractionResult};
use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior, trigger_if};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, CountDownCooldownTicks, DoNothing, FollowTemptation,
    LookAtTargetSink, MoveToTargetSink, RandomLookAround, Swim, UniformInt, baby_follow_adult,
    random_stroll, run_one, set_entity_look_target_sometimes, set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    GAZE_COOLDOWN_TICKS, NEAREST_VISIBLE_ADULT, TEMPTATION_COOLDOWN_TICKS, WALK_TARGET,
};
use crate::entity::ai::brain::sensors::{
    AdultSensor, HurtBySensor, NearestLivingEntitySensor, TemptingSensor,
};
use crate::entity::ai::brain::{
    ActivityData, BehaviorControl, Brain, Memories, MemoryStatus, Sensor,
};
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::damage::DamageSource;
use crate::entity::entities::mobs::passive::equine::PlayerRideableJumping;
use crate::entity::leash::Leashable as _;
use crate::entity::monster::{is_dark_enough_to_spawn, is_spawn_allowed_on_difficulty};
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    sync_dirty_mob_effects,
};
use crate::physics::{CollisionWorld as _, MoveResult, WorldCollisionProvider};
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Camel.BABY_SCALE`.
const BABY_SCALE: f32 = 0.6;
/// Vanilla `Camel.DASH_COOLDOWN_TICKS`.
const DASH_COOLDOWN_TICKS: i32 = 55;
/// Vanilla `Camel.DASH_MINIMUM_DURATION_TICKS`: a dash ends on landing once
/// `dashCooldown < 50`.
const DASH_MINIMUM_DURATION_TICKS: i32 = 5;
/// Vanilla `Camel.MAX_HEAD_Y_ROT`.
const MAX_HEAD_Y_ROT: f32 = 30.0;
/// Vanilla `Camel.RUNNING_SPEED_BONUS`.
const RUNNING_SPEED_BONUS: f32 = 0.1;
/// Vanilla `Camel.DASH_VERTICAL_MOMENTUM`.
const DASH_VERTICAL_MOMENTUM: f32 = 1.4285;
/// Vanilla `Camel.DASH_HORIZONTAL_MOMENTUM`.
const DASH_HORIZONTAL_MOMENTUM: f32 = 22.2222;
/// Vanilla `Camel.SITDOWN_DURATION_TICKS`.
const SITDOWN_DURATION_TICKS: i64 = 40;
/// Vanilla `Camel.STANDUP_DURATION_TICKS`.
const STANDUP_DURATION_TICKS: i64 = 52;
/// Vanilla `Camel.SITTING_HEIGHT_DIFFERENCE`.
const SITTING_HEIGHT_DIFFERENCE: f32 = 1.43;
/// Vanilla `Camel.BABY_STANDING_DIMENSIONS`.
const BABY_STANDING_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.95, 1.4, 1.38);
/// Vanilla `Camel.BABY_SITTING_DIMENSIONS`.
const BABY_SITTING_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.95, 0.425, 0.41);
/// Vanilla `Camel.ADULT_SITTING_DIMENSIONS` eye height.
const ADULT_SITTING_EYE_HEIGHT: f32 = 0.845;

/// Vanilla `CamelAi.SPEED_MULTIPLIER_WHEN_PANICKING`.
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 4.0;
/// Vanilla `CamelAi.SPEED_MULTIPLIER_WHEN_IDLING`.
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 2.0;
/// Vanilla `CamelAi.SPEED_MULTIPLIER_WHEN_TEMPTED`.
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 2.5;
/// Vanilla `CamelAi.SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT`.
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 2.5;
/// Vanilla `CamelAi.ADULT_FOLLOW_RANGE`.
const ADULT_FOLLOW_RANGE: UniformInt = UniformInt::of(5, 16);
/// Vanilla `new CamelAi.RandomSitting(20)`: seconds in one pose before
/// switching.
const RANDOM_SITTING_MIN_POSE_SECONDS: i64 = 20;
/// Vanilla `FollowTemptation` close-enough distance of a baby and an adult.
const BABY_TEMPTATION_DISTANCE: f64 = 2.5;
const ADULT_TEMPTATION_DISTANCE: f64 = 3.5;

/// The sounds a camel kind makes.
struct CamelSounds {
    ambient: SoundEventRef,
    hurt: SoundEventRef,
    death: SoundEventRef,
    step: SoundEventRef,
    step_sand: SoundEventRef,
    step_volume: f32,
    eat: SoundEventRef,
    saddle: SoundEventRef,
    dash: SoundEventRef,
    dash_ready: SoundEventRef,
    stand: SoundEventRef,
    sit: SoundEventRef,
}

/// Vanilla `Camel` sounds.
static CAMEL_SOUNDS: CamelSounds = CamelSounds {
    ambient: &sound_events::ENTITY_CAMEL_AMBIENT,
    hurt: &sound_events::ENTITY_CAMEL_HURT,
    death: &sound_events::ENTITY_CAMEL_DEATH,
    step: &sound_events::ENTITY_CAMEL_STEP,
    step_sand: &sound_events::ENTITY_CAMEL_STEP_SAND,
    step_volume: 1.0,
    eat: &sound_events::ENTITY_CAMEL_EAT,
    saddle: &sound_events::ENTITY_CAMEL_SADDLE,
    dash: &sound_events::ENTITY_CAMEL_DASH,
    dash_ready: &sound_events::ENTITY_CAMEL_DASH_READY,
    stand: &sound_events::ENTITY_CAMEL_STAND,
    sit: &sound_events::ENTITY_CAMEL_SIT,
};

/// Vanilla `CamelHusk` sounds.
static CAMEL_HUSK_SOUNDS: CamelSounds = CamelSounds {
    ambient: &sound_events::ENTITY_CAMEL_HUSK_AMBIENT,
    hurt: &sound_events::ENTITY_CAMEL_HUSK_HURT,
    death: &sound_events::ENTITY_CAMEL_HUSK_DEATH,
    step: &sound_events::ENTITY_CAMEL_HUSK_STEP,
    step_sand: &sound_events::ENTITY_CAMEL_HUSK_STEP_SAND,
    step_volume: 0.4,
    eat: &sound_events::ENTITY_CAMEL_HUSK_EAT,
    saddle: &sound_events::ENTITY_CAMEL_HUSK_SADDLE,
    dash: &sound_events::ENTITY_CAMEL_HUSK_DASH,
    dash_ready: &sound_events::ENTITY_CAMEL_HUSK_DASH_READY,
    stand: &sound_events::ENTITY_CAMEL_HUSK_STAND,
    sit: &sound_events::ENTITY_CAMEL_HUSK_SIT,
};

/// State every camel keeps beside its synced data.
pub struct CamelBase {
    /// Vanilla `dashCooldown`.
    dash_cooldown: AtomicI32,
}

impl CamelBase {
    #[must_use]
    const fn new() -> Self {
        Self {
            dash_cooldown: AtomicI32::new(0),
        }
    }
}

/// Vanilla `Camel`: shared by camels and camel husks.
pub trait Camel: AbstractHorse {
    /// The camel's unsynced state.
    fn camel_base(&self) -> &CamelBase;

    /// Vanilla `isDashing`.
    fn is_dashing(&self) -> bool;

    /// Writes vanilla `DASH`; returns whether the value changed.
    fn write_dashing(&self, dashing: bool) -> bool;

    /// Vanilla `LAST_POSE_CHANGE_TICK`: negative while sitting.
    fn last_pose_change_tick(&self) -> i64;

    /// Vanilla `resetLastPoseChangeTick`.
    fn reset_last_pose_change_tick(&self, synced_pose_tick_time: i64);

    /// Vanilla `getDashingSound`.
    fn dashing_sound(&self) -> SoundEventRef;

    /// Vanilla `getDashReadySound`.
    fn dash_ready_sound(&self) -> SoundEventRef;

    /// Vanilla `getStandUpSound`.
    fn stand_up_sound(&self) -> SoundEventRef;

    /// Vanilla `getSitDownSound`.
    fn sit_down_sound(&self) -> SoundEventRef;

    /// Vanilla `dashCooldown`, which is also `getJumpCooldown`.
    fn dash_cooldown(&self) -> i32 {
        self.camel_base().dash_cooldown.load(Ordering::Relaxed)
    }

    /// Sets vanilla `dashCooldown`.
    fn set_dash_cooldown(&self, ticks: i32) {
        self.camel_base()
            .dash_cooldown
            .store(ticks, Ordering::Relaxed);
    }

    /// Vanilla `setDashing`, with `onSyncedDataUpdated` restarting the dash
    /// cooldown when `DASH` changes.
    fn set_dashing(&self, dashing: bool) {
        if self.write_dashing(dashing) && !self.is_first_tick() && self.dash_cooldown() == 0 {
            self.set_dash_cooldown(DASH_COOLDOWN_TICKS);
        }
    }

    /// Vanilla `isCamelSitting`.
    fn is_camel_sitting(&self) -> bool {
        self.last_pose_change_tick() < 0
    }

    /// Vanilla `getPoseTime`.
    fn pose_time(&self) -> i64 {
        let game_time = self.level().map_or(0, |world| world.game_time());
        game_time - self.last_pose_change_tick().abs()
    }

    /// Vanilla `isInPoseTransition`.
    fn is_in_pose_transition(&self) -> bool {
        let duration = if self.is_camel_sitting() {
            SITDOWN_DURATION_TICKS
        } else {
            STANDUP_DURATION_TICKS
        };
        self.pose_time() < duration
    }

    /// Vanilla `refuseToMove`.
    fn refuse_to_move(&self) -> bool {
        self.is_camel_sitting() || self.is_in_pose_transition()
    }

    /// Vanilla `resetLastPoseChangeTickToFullStand`.
    fn reset_last_pose_change_tick_to_full_stand(&self, current_time: i64) {
        self.reset_last_pose_change_tick((current_time - STANDUP_DURATION_TICKS - 1).max(0));
    }

    /// Vanilla `sitDown`.
    fn sit_down(&self) {
        if self.is_camel_sitting() {
            return;
        }
        self.make_sound(Some(self.sit_down_sound()));
        self.set_pose(EntityPose::Sitting);
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        let game_time = self.level().map_or(0, |world| world.game_time());
        self.reset_last_pose_change_tick(-game_time);
    }

    /// Vanilla `standUp`.
    fn stand_up(&self) {
        if !self.is_camel_sitting() {
            return;
        }
        self.make_sound(Some(self.stand_up_sound()));
        self.set_pose(EntityPose::Standing);
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        let game_time = self.level().map_or(0, |world| world.game_time());
        self.reset_last_pose_change_tick(game_time);
    }

    /// Vanilla `standUpInstantly`.
    fn stand_up_instantly(&self) {
        self.set_pose(EntityPose::Standing);
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        let game_time = self.level().map_or(0, |world| world.game_time());
        self.reset_last_pose_change_tick_to_full_stand(game_time);
    }

    /// Vanilla `canCamelChangePose`.
    fn can_camel_change_pose(&self) -> bool
    where
        Self: Sized,
    {
        let pose = if self.is_camel_sitting() {
            EntityPose::Standing
        } else {
            EntityPose::Sitting
        };
        would_not_suffocate_at_target_pose(self, pose)
    }

    /// Vanilla `Camel.tick`, after `AbstractHorse.tick`.
    fn camel_tick(&self) {
        let dash_cooldown = self.dash_cooldown();
        if self.is_dashing()
            && dash_cooldown < DASH_COOLDOWN_TICKS - DASH_MINIMUM_DURATION_TICKS
            && (self.on_ground() || self.is_in_water() || self.is_in_lava() || self.is_passenger())
        {
            self.set_dashing(false);
        }

        let dash_cooldown = self.dash_cooldown();
        if dash_cooldown > 0 {
            self.set_dash_cooldown(dash_cooldown - 1);
            if dash_cooldown - 1 == 0
                && let Some(world) = self.level()
            {
                let (x, y, z) = self.block_position().get_center();
                world.play_sound_at(
                    self.dash_ready_sound(),
                    SoundSource::Neutral,
                    DVec3::new(x, y, z),
                    1.0,
                    1.0,
                    None,
                );
            }
        }

        if self.refuse_to_move() {
            self.clamp_head_rotation_to_body();
        }

        if self.is_camel_sitting() && self.is_in_water() {
            self.stand_up_instantly();
        }
    }

    /// Vanilla `getBodyAnchorAnimationYOffset` at partial tick 0.
    fn body_anchor_animation_y_offset(&self, is_front: bool, height: f32, scale: f32) -> f64 {
        let age_sit_y_offset = if AgeableMob::is_baby(self) {
            0.093_75
        } else {
            0.375
        };
        let mut base_sit_offset = f64::from(height) - age_sit_y_offset;
        let sitting_height_difference = scale * SITTING_HEIGHT_DIFFERENCE;
        let vertical_drop = sitting_height_difference - scale * 0.2;
        let bottom_point = sitting_height_difference - vertical_drop;
        let is_in_transition = self.is_in_pose_transition();
        let is_sitting = self.is_camel_sitting();
        if is_in_transition {
            let animation_duration = if is_sitting {
                SITDOWN_DURATION_TICKS
            } else {
                STANDUP_DURATION_TICKS
            };
            let (half_point, flex_point_offset) = if is_sitting {
                (28_i64, if is_front { 0.5 } else { 0.1 })
            } else if is_front {
                (24, 0.6)
            } else {
                (32, 0.35)
            };
            #[expect(
                clippy::cast_precision_loss,
                reason = "vanilla casts the pose time to float"
            )]
            let pose_time = (self.pose_time() as f32).clamp(0.0, animation_duration as f32);
            #[expect(
                clippy::cast_precision_loss,
                reason = "vanilla's int animation points widen to float"
            )]
            let (half, duration) = (half_point as f32, animation_duration as f32);
            let is_first_part = pose_time < half;
            let part = if is_first_part {
                pose_time / half
            } else {
                (pose_time - half) / (duration - half)
            };
            let flex_point = sitting_height_difference - flex_point_offset * vertical_drop;
            let offset = if is_sitting {
                lerp(
                    part,
                    if is_first_part {
                        sitting_height_difference
                    } else {
                        flex_point
                    },
                    if is_first_part {
                        flex_point
                    } else {
                        bottom_point
                    },
                )
            } else {
                lerp(
                    part,
                    if is_first_part {
                        bottom_point - sitting_height_difference
                    } else {
                        bottom_point - flex_point
                    },
                    if is_first_part {
                        bottom_point - flex_point
                    } else {
                        0.0
                    },
                )
            };
            base_sit_offset += f64::from(offset);
        }

        if is_sitting && !is_in_transition {
            base_sit_offset += f64::from(bottom_point);
        }

        base_sit_offset
    }

    /// Vanilla `Camel.getPassengerAttachmentPoint`.
    fn camel_passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
        let passengers = self.passengers();
        let index = passengers
            .iter()
            .position(|entity| entity.id() == passenger.id())
            .unwrap_or(0);
        let driver = index == 0;
        let scale = self.get_scale() * self.get_age_scale();
        let mut offset = 0.5_f32;
        let height = if self.is_removed() {
            0.01_f32
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "vanilla casts the anchor offset to float"
            )]
            let height =
                self.body_anchor_animation_y_offset(driver, self.base().dimensions().height, scale)
                    as f32;
            height
        };
        if passengers.len() > 1 {
            if !driver {
                offset = -0.7;
            }
            if passenger.as_animal().is_some() {
                offset += 0.2;
            }
        }
        let (sin, cos) = (-self.rotation().0 * (PI / 180.0)).sin_cos();
        let z = offset * scale;
        DVec3::new(f64::from(z * sin), f64::from(height), f64::from(z * cos))
    }
}

/// Vanilla `Mth.lerp` on floats.
fn lerp(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

/// Vanilla `Vec3.normalize`.
fn vanilla_normalize(vector: DVec3) -> DVec3 {
    let length = vector.length();
    if length < 1.0E-5 {
        DVec3::ZERO
    } else {
        vector / length
    }
}

/// Vanilla `LivingEntity.wouldNotSuffocateAtTargetPose`: no block collides
/// with the box `entity` would have in `pose`.
fn would_not_suffocate_at_target_pose(entity: &(impl Entity + Sized), pose: EntityPose) -> bool {
    let Some(world) = entity.level() else {
        return true;
    };
    let dimensions = entity.dimensions_for_pose(pose);
    let position = entity.position();
    let target_box = WorldAabb::entity_box(
        position.x,
        position.y,
        position.z,
        f64::from(dimensions.half_width()),
        f64::from(dimensions.height),
    );
    let collisions = WorldCollisionProvider::for_entity(&world, entity);
    !collisions.has_block_collision_with_context(
        &target_box,
        BlockCollisionContext::entity(position.y, entity.is_descending())
            .with_can_walk_on_powder_snow(PowderSnowBlock::can_entity_walk_on_powder_snow(entity)),
    )
}

/// Vanilla `CamelAi.CamelPanic`: panics unless a mob rides it, and stands
/// up first.
struct CamelPanic<E: ?Sized> {
    panic: AnimalPanic<E>,
}

impl<E: Camel> CamelPanic<E> {
    fn new(speed_multiplier: f32) -> Self {
        Self {
            panic: AnimalPanic::new(speed_multiplier),
        }
    }
}

impl<E: Camel> Behavior<E> for CamelPanic<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        self.panic.entry_condition()
    }

    fn duration(&self) -> (i32, i32) {
        self.panic.duration()
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &E) -> bool {
        self.panic.check_extra_start_conditions(world, body) && !body.is_mob_controlled()
    }

    fn start(&mut self, world: &Arc<World>, body: &E, time: i64) {
        body.stand_up_instantly();
        self.panic.start(world, body, time);
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        self.panic.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.panic.tick(world, body, time);
    }

    fn stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.panic.stop(world, body, time);
    }
}

/// Vanilla `CamelAi.RandomSitting`: sits down or stands up after holding a
/// pose for a while.
struct RandomSitting;

impl<E: Camel> Behavior<E> for RandomSitting {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &[]
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        !body.is_in_water()
            && body.pose_time() >= RANDOM_SITTING_MIN_POSE_SECONDS * 20
            && !body.is_leashed()
            && body.on_ground()
            && !body.has_controlling_passenger()
            && body.can_camel_change_pose()
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if body.is_camel_sitting() {
            body.stand_up();
        } else if !body.is_panicking() {
            body.sit_down();
        }
    }
}

/// Vanilla `Camel.checkCamelSpawnRules`.
fn check_camel_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn rand::Rng,
) -> bool {
    level
        .get_block_state(pos.below())
        .get_block()
        .has_tag(&BlockTag::CAMELS_SPAWNABLE_ON)
        && <CamelEntity as Animal>::is_bright_enough_to_spawn(level, pos)
}

/// A camel: a two-seat mount that sits, dashes and is run by a brain.
#[entity_behavior(class = "Camel", spawn_rule)]
pub struct CamelEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    camel_base: CamelBase,
    entity_data: SyncMutex<CamelEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `CamelEntity`.
unsafe impl DowncastType for CamelEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/camel");
}

/// A camel husk: an undead camel that never breeds.
#[entity_behavior(class = "CamelHusk", spawn_rule)]
pub struct CamelHuskEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    camel_base: CamelBase,
    entity_data: SyncMutex<CamelHuskEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `CamelHuskEntity`.
unsafe impl DowncastType for CamelHuskEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/camel_husk");
}

impl CamelEntity {
    /// Vanilla `SpawnPlacements` predicate: `Camel.checkCamelSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_camel_spawn_rules;
}

impl CamelHuskEntity {
    /// Vanilla `SpawnPlacements` predicate:
    /// `Monster.checkSurfaceMonstersSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        is_spawn_allowed_on_difficulty(entity_type, level)
            && (spawn_reason.ignores_light_requirements()
                || is_dark_enough_to_spawn(level, pos, random))
            && <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
            && (spawn_reason.is_spawner() || level.can_see_sky(pos))
    };
}

/// Whether `partner` is a camel or a camel husk (vanilla `instanceof Camel`).
fn is_camel(partner: &dyn Animal) -> bool {
    let partner_type = partner.entity_type();
    partner_type == &vanilla_entities::CAMEL || partner_type == &vanilla_entities::CAMEL_HUSK
}

/// The item-in-hand part of vanilla `Camel.mobInteract`:
/// `itemStack.interactLivingEntity(player, this, hand)`.
fn interact_with_held_item(
    camel: &(impl LivingEntity + Sized),
    player: &Player,
    hand: InteractionHand,
    item_stack: &ItemStack,
) -> InteractionResult {
    if item_stack.is_empty() {
        return InteractionResult::Pass;
    }
    let result = camel.interact_living_entity_with_equippable(player, hand);
    if result.consumes_action() {
        return result;
    }
    let mut inventory = player.inventory.lock();
    let held = inventory.get_item_in_hand_mut(hand);
    ITEM_BEHAVIORS
        .get_behavior(item_stack.item())
        .interact_living_entity(held, player, camel, hand)
}

macro_rules! camel_entity {
    (
        $entity:ident, $data:ty, $sounds:ident,
        entity { $($entity_items:item)* }
        living { $($living_items:item)* }
        ageable { $($ageable_items:item)* }
        animal { $($animal_items:item)* }
        mob { $($mob_items:item)* }
        horse { $($horse_items:item)* }
    ) => {
        impl $entity {
            /// Creates a new entity at runtime.
            #[must_use]
            pub fn new(
                entity_type: EntityTypeRef,
                id: i32,
                position: DVec3,
                world: Weak<World>,
            ) -> Self {
                Self::new_with_base(
                    EntityBase::new(id, position, entity_type.dimensions, world),
                    entity_type,
                )
            }

            /// Reconstructs the entity from persisted base entity state.
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
                AnimalBase::initialize_pathfinding_malus(&mob_base);
                {
                    let mut navigation = mob_base.navigation().lock();
                    navigation.set_can_float(true);
                    navigation.set_can_walk_over_fences(true);
                }
                let mut entity_data = <$data>::new();
                living_base.initialize_synced_data(&mut entity_data);
                Self {
                    base,
                    entity_type,
                    living_base,
                    mob_base,
                    ageable_base: AgeableMobBase::new(),
                    animal_base: AnimalBase::new(),
                    horse_base: AbstractHorseBase::new(0),
                    camel_base: CamelBase::new(),
                    entity_data: SyncMutex::new(entity_data),
                    brain: Brain::new(&[], Self::sensors(), Self::activities(), None),
                }
            }

            /// Vanilla `Camel.BRAIN_PROVIDER` sensors.
            fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
                vec![
                    Box::new(NearestLivingEntitySensor),
                    Box::new(HurtBySensor),
                    Box::new(TemptingSensor::<Self>::for_animal()),
                    Box::new(AdultSensor),
                ]
            }

            /// Vanilla `CamelAi.getActivities`.
            fn activities() -> Vec<ActivityData<Self>> {
                vec![Self::core_activity(), Self::idle_activity()]
            }

            /// Vanilla `CamelAi.initCoreActivity`.
            fn core_activity() -> ActivityData<Self> {
                ActivityData::with_first_priority(
                    Activity::CORE,
                    0,
                    vec![
                        Box::new(TimedBehavior::new(Swim::new(0.8))),
                        Box::new(TimedBehavior::new(CamelPanic::new(
                            SPEED_MULTIPLIER_WHEN_PANICKING,
                        ))),
                        Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
                        Box::new(TimedBehavior::new(MoveToTargetSink::new())),
                        Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                            &TEMPTATION_COOLDOWN_TICKS,
                        ))),
                        Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                            &GAZE_COOLDOWN_TICKS,
                        ))),
                    ],
                )
            }

            /// Vanilla `CamelAi.initIdleActivity`.
            fn idle_activity() -> ActivityData<Self> {
                let follow: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
                    (
                        Box::new(TimedBehavior::new(FollowTemptation::with_distance(
                            |_| SPEED_MULTIPLIER_WHEN_TEMPTED,
                            |camel: &Self| {
                                if AgeableMob::is_baby(camel) {
                                    BABY_TEMPTATION_DISTANCE
                                } else {
                                    ADULT_TEMPTATION_DISTANCE
                                }
                            },
                            false,
                        ))),
                        1,
                    ),
                    (
                        Box::new(trigger_if(
                            |camel: &Self| !camel.refuse_to_move(),
                            baby_follow_adult(
                                ADULT_FOLLOW_RANGE,
                                |_| SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                                &NEAREST_VISIBLE_ADULT,
                                false,
                            ),
                        )),
                        1,
                    ),
                ];
                let stroll: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
                    (
                        Box::new(trigger_if(
                            |camel: &Self| !camel.refuse_to_move(),
                            random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true),
                        )),
                        1,
                    ),
                    (
                        Box::new(trigger_if(
                            |camel: &Self| !camel.refuse_to_move(),
                            set_walk_target_from_look_target(
                                |_| true,
                                |_| SPEED_MULTIPLIER_WHEN_IDLING,
                                3,
                            ),
                        )),
                        1,
                    ),
                    (Box::new(TimedBehavior::new(RandomSitting)), 1),
                    (Box::new(DoNothing::new(30, 60)), 1),
                ];
                ActivityData::new(
                    Activity::IDLE,
                    vec![
                        (
                            0,
                            Box::new(set_entity_look_target_sometimes(
                                6.0,
                                UniformInt::of(30, 60),
                                |entity| entity.entity_type() == &vanilla_entities::PLAYER,
                            )),
                        ),
                        (
                            1,
                            Box::new(TimedBehavior::new(AnimalMakeLove::new(
                                &vanilla_entities::CAMEL,
                            ))),
                        ),
                        (2, Box::new(run_one(Vec::new(), follow))),
                        (
                            3,
                            Box::new(TimedBehavior::new(RandomLookAround::new(
                                UniformInt::of(150, 250),
                                30.0,
                                0.0,
                                0.0,
                            ))),
                        ),
                        (
                            4,
                            Box::new(run_one(
                                vec![MemoryRequirement::new(
                                    &WALK_TARGET,
                                    MemoryStatus::ValueAbsent,
                                )],
                                stroll,
                            )),
                        ),
                    ],
                )
            }

            fn update_dirty_mob_effect_entity_data(&self) {
                if let Some(display) =
                    sync_dirty_mob_effects(&self.living_base, &self.entity_data)
                {
                    self.entity_data
                        .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
                }
            }

            /// Vanilla `Camel.mobInteract`.
            fn camel_mob_interact(
                &self,
                player: &Player,
                hand: InteractionHand,
            ) -> InteractionResult {
                let item_stack = {
                    let inventory = player.inventory.lock();
                    let item_stack = inventory.get_item_in_hand(hand);
                    item_stack.copy_with_count(item_stack.count())
                };
                let baby = AgeableMob::is_baby(self);
                if player.is_secondary_use_active() && !baby {
                    self.open_custom_inventory_screen(player);
                    return InteractionResult::Success;
                }
                let result = interact_with_held_item(self, player, hand, &item_stack);
                if result.consumes_action() {
                    return result;
                }
                if self.is_food(&item_stack) {
                    return self.fed_food(player, hand, &item_stack);
                }
                if self.passengers().len() < 2 && !baby {
                    self.do_player_ride(player);
                    return InteractionResult::Consume;
                }
                if baby && item_stack.is(&vanilla_items::GOLDEN_DANDELION) {
                    return self.horse_mob_interact(player, hand);
                }
                InteractionResult::Fail
            }
        }

        impl Camel for $entity {
            fn camel_base(&self) -> &CamelBase {
                &self.camel_base
            }

            fn is_dashing(&self) -> bool {
                *self.entity_data.lock().camel().dash.get()
            }

            fn write_dashing(&self, dashing: bool) -> bool {
                let mut entity_data = self.entity_data.lock();
                let changed = *entity_data.camel().dash.get() != dashing;
                entity_data.camel_mut().dash.set(dashing);
                changed
            }

            fn last_pose_change_tick(&self) -> i64 {
                *self.entity_data.lock().camel().last_pose_change_tick.get()
            }

            fn reset_last_pose_change_tick(&self, synced_pose_tick_time: i64) {
                self.entity_data
                    .lock()
                    .camel_mut()
                    .last_pose_change_tick
                    .set(synced_pose_tick_time);
            }

            fn dashing_sound(&self) -> SoundEventRef {
                $sounds.dash
            }

            fn dash_ready_sound(&self) -> SoundEventRef {
                $sounds.dash_ready
            }

            fn stand_up_sound(&self) -> SoundEventRef {
                $sounds.stand
            }

            fn sit_down_sound(&self) -> SoundEventRef {
                $sounds.sit
            }
        }

        impl AbstractHorse for $entity {
            fn horse_base(&self) -> &AbstractHorseBase {
                &self.horse_base
            }

            fn horse_flags(&self) -> i8 {
                *self.entity_data.lock().abstract_horse().id_flags.get()
            }

            fn set_horse_flags(&self, flags: i8) {
                self.entity_data
                    .lock()
                    .abstract_horse_mut()
                    .id_flags
                    .set(flags);
            }

            /// Vanilla `Camel.isTamed`.
            fn is_tamed(&self) -> bool {
                true
            }

            fn can_perform_rearing(&self) -> bool {
                false
            }

            fn eating_sound(&self) -> Option<SoundEventRef> {
                Some($sounds.eat)
            }

            /// Vanilla `Camel.openCustomInventoryScreen`: no rider check.
            fn open_custom_inventory_screen(&self, player: &Player) {
                menu::show_horse_inventory(self, player);
            }

            /// Vanilla `Camel.handleEating`.
            fn handle_eating(&self, player: &Player, item_stack: &ItemStack) -> bool {
                if !self.is_food(item_stack) {
                    return false;
                }
                let could_heal = self.get_health() < self.get_max_health();
                if could_heal {
                    self.heal(2.0);
                }
                let could_set_in_love =
                    self.is_tamed() && self.get_age() == 0 && self.can_fall_in_love();
                if could_set_in_love {
                    self.set_in_love(Some(player));
                }
                let could_age_up = self.can_age_up();
                if could_age_up {
                    // Vanilla's HAPPY_VILLAGER particle here is client-only.
                    self.age_up(10, false);
                }
                if !could_heal && !could_set_in_love && !could_age_up {
                    return false;
                }
                if !self.is_silent()
                    && let Some(eating_sound) = self.eating_sound()
                    && let Some(world) = self.level()
                {
                    world.play_sound_at(
                        eating_sound,
                        self.sound_source(),
                        self.position(),
                        1.0,
                        1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2,
                        None,
                    );
                }
                self.game_event(&vanilla_game_events::EAT);
                true
            }

            /// Vanilla `Camel.getRiddenRotation`: frozen while it refuses to
            /// move.
            fn horse_ridden_rotation(&self, controller: &Player) -> (f32, f32) {
                if self.refuse_to_move() {
                    self.rotation()
                } else {
                    let (yaw, pitch) = controller.rotation();
                    (yaw, pitch * 0.5)
                }
            }

            /// Vanilla `Camel.executeRidersJump`: a dash.
            fn execute_riders_jump(&self, amount: f32, _input: DVec3) {
                let jump_momentum = f64::from(self.get_jump_power());
                let movement_speed = self
                    .attributes()
                    .lock()
                    .required_value(vanilla_attributes::MOVEMENT_SPEED);
                let look = self.look_angle();
                let horizontal = vanilla_normalize(DVec3::new(look.x, 0.0, look.z))
                    * (f64::from(DASH_HORIZONTAL_MOMENTUM * amount)
                        * movement_speed
                        * f64::from(self.block_speed_factor()));
                let impulse = horizontal
                    + DVec3::new(
                        0.0,
                        f64::from(DASH_VERTICAL_MOMENTUM * amount) * jump_momentum,
                        0.0,
                    );
                self.set_velocity(self.velocity() + impulse);
                self.set_dash_cooldown(DASH_COOLDOWN_TICKS);
                self.set_dashing(true);
                self.mark_hurt();
            }

            /// Vanilla `Camel.onPlayerJump`.
            fn on_player_jump(&self, jump_amount: i32) {
                if !(self.is_saddled() && self.dash_cooldown() <= 0 && self.on_ground()) {
                    return;
                }
                // Vanilla `AbstractHorse.onPlayerJump`.
                let jump_amount = if jump_amount < 0 {
                    0
                } else {
                    self.horse_base
                        .allow_stand_sliding
                        .store(true, Ordering::Relaxed);
                    self.stand_if_possible();
                    jump_amount
                };
                *self.horse_base.player_jump_pending_scale.lock() =
                    super::player_jump_pending_scale(jump_amount);
            }

            $($horse_items)*
        }

        // A per-type impl, since `try_as_dyn` cannot see blanket impls.
        impl PlayerRideableJumping for $entity {
            /// Vanilla `Camel.canJump`.
            fn can_jump(&self) -> bool {
                !self.refuse_to_move() && self.is_saddled()
            }

            /// Vanilla `Camel.handleStartJump`.
            fn handle_start_jump(&self, _jump_scale: i32) {
                self.make_sound(Some(self.dashing_sound()));
                self.game_event(&vanilla_game_events::ENTITY_ACTION);
                self.set_dashing(true);
            }

            fn handle_stop_jump(&self) {}
        }

        impl DefaultEntityPush for $entity {}

        impl Entity for $entity {
            fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
                Some(self)
            }

            fn is_hard_collision_relevant(&self) -> bool {
                false
            }

            fn base(&self) -> &EntityBase {
                &self.base
            }

            fn entity_type(&self) -> EntityTypeRef {
                self.entity_type
            }

            fn tick(self: Arc<Self>) {
                let entity = Arc::clone(&self).into_shared_entity();
                self.tick_living_entity(&entity);
                AbstractHorse::horse_tick(&*self);
                self.camel_tick();
            }

            fn base_tick(&self) {
                Mob::base_tick_mob(self);
            }

            fn dismount_location_for_passenger(&self, passenger: &dyn Entity) -> DVec3 {
                AbstractHorse::horse_dismount_location(self, passenger)
            }

            /// Vanilla `Camel.getDefaultDimensions`, scaled by `getScale`.
            fn dimensions_for_pose(&self, pose: EntityPose) -> EntityDimensions {
                let baby = AgeableMob::is_baby(self);
                let dimensions = if pose == EntityPose::Sitting {
                    if baby {
                        BABY_SITTING_DIMENSIONS
                    } else {
                        let standing = vanilla_entities::CAMEL.dimensions;
                        EntityDimensions::new(
                            standing.width,
                            standing.height - SITTING_HEIGHT_DIFFERENCE,
                            ADULT_SITTING_EYE_HEIGHT,
                        )
                    }
                } else if baby {
                    BABY_STANDING_DIMENSIONS
                } else {
                    self.entity_type.dimensions
                };
                if self.entity_type.fixed {
                    dimensions
                } else {
                    dimensions.scale(LivingEntity::get_scale(self))
                }
            }

            fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
                Some(&self.entity_data)
            }

            fn update_data_before_sync(&self) {
                self.update_dirty_mob_effect_entity_data();
            }

            fn max_up_step(&self) -> f32 {
                self.attributes()
                    .lock()
                    .get_value(vanilla_attributes::STEP_HEIGHT)
                    .unwrap_or(vanilla_attributes::STEP_HEIGHT.default_value)
                    as f32
            }

            fn sound_source(&self) -> SoundSource {
                SoundSource::Neutral
            }

            /// Vanilla `Camel.playStepSound`.
            fn play_step_sound(&self, _pos: BlockPos, block_state: BlockStateId) {
                let sound = if block_state
                    .get_block()
                    .has_tag(&BlockTag::CAMEL_SAND_STEP_SOUND_BLOCKS)
                {
                    $sounds.step_sand
                } else {
                    $sounds.step
                };
                self.play_sound(sound, $sounds.step_volume, 1.0);
            }

            fn is_pushable(&self) -> bool {
                self.is_horse_pushable()
            }

            fn on_climbable(&self) -> bool {
                false
            }

            fn controlling_passenger(&self) -> Option<SharedEntity> {
                self.horse_controlling_passenger()
            }

            fn passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
                self.camel_passenger_attachment_point(passenger)
            }

            /// Vanilla `Camel.canAddPassenger`.
            fn can_add_passenger(&self, _passenger: &dyn Entity) -> bool {
                self.passengers().len() <= 2
            }

            fn cause_fall_damage(
                self: Arc<Self>,
                fall_distance: f64,
                damage_modifier: f32,
                source: &DamageSource,
            ) -> bool {
                self.horse_cause_fall_damage(fall_distance, damage_modifier, source)
            }

            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_mob(nbt);
                self.save_ageable_mob(nbt);
                self.save_animal(nbt);
                nbt.insert("Brain", self.brain.save());
                self.save_horse(nbt);
                nbt.insert("LastPoseTick", self.last_pose_change_tick());
            }

            /// Vanilla `Camel.readAdditionalSaveData`.
            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_mob(nbt);
                self.load_ageable_mob(nbt);
                self.load_animal(nbt);
                if let Some(brain) = nbt.compound("Brain") {
                    self.brain.memories().lock().load(brain);
                }
                self.load_horse(nbt);
                let pose_tick = nbt.long("LastPoseTick").unwrap_or(0);
                if pose_tick < 0 {
                    self.set_pose(EntityPose::Sitting);
                }
                self.reset_last_pose_change_tick(pose_tick);
            }

            $($entity_items)*
        }

        impl LivingEntity for $entity {
            fn living_base(&self) -> &LivingEntityBase {
                &self.living_base
            }

            fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
                Some(self.brain.memories())
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

            /// Vanilla `Camel.getAgeScale`.
            fn get_age_scale(&self) -> f32 {
                if AgeableMob::is_baby(self) {
                    BABY_SCALE
                } else {
                    1.0
                }
            }

            fn sound_volume(&self) -> f32 {
                SOUND_VOLUME
            }

            fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
                Some($sounds.hurt)
            }

            fn death_sound(&self) -> Option<SoundEventRef> {
                Some($sounds.death)
            }

            fn can_use_slot(&self, slot: EquipmentSlot) -> bool {
                self.can_use_horse_slot(slot)
            }

            fn can_dispenser_equip_into_slot(&self, slot: EquipmentSlot) -> bool {
                self.can_dispenser_equip_horse_slot(slot)
            }

            /// Vanilla `Camel.getEquipSound`.
            fn equip_sound(
                &self,
                slot: EquipmentSlot,
                stack: &ItemStack,
            ) -> Option<SoundEventRef> {
                if slot == EquipmentSlot::Saddle {
                    Some($sounds.saddle)
                } else {
                    self.horse_equip_sound(slot, stack)
                }
            }

            fn is_immobile(&self) -> bool {
                self.is_horse_immobile()
            }

            fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
                self.horse_hurt_server(world, source, amount)
            }

            /// Vanilla `Camel.actuallyHurt`.
            fn actually_hurt(&self, world: &World, source: &DamageSource, amount: f32) {
                self.stand_up_instantly();
                self.default_actually_hurt(world, source, amount);
            }

            fn drop_equipment(&self) {
                self.drop_horse_inventory();
            }

            fn server_ai_step(&self, entity: &SharedEntity) {
                Mob::mob_server_ai_step(self, entity);
            }

            /// Vanilla `Camel.travel`.
            fn travel(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
                let input = if self.refuse_to_move() && self.on_ground() {
                    let velocity = self.velocity();
                    self.set_velocity(DVec3::new(0.0, velocity.y, 0.0));
                    DVec3::new(0.0, input.y, 0.0)
                } else {
                    input
                };
                self.default_travel(entity, input)
            }

            /// Vanilla `Camel.tickRidden`.
            fn tick_ridden(&self, controller: &Player, ridden_input: DVec3) {
                self.horse_tick_ridden(controller, ridden_input);
                if controller.last_client_input().forward_intent() > 0.0
                    && self.is_camel_sitting()
                    && !self.is_in_pose_transition()
                {
                    self.stand_up();
                }
            }

            /// Vanilla `Camel.getRiddenInput`.
            fn ridden_input(&self, controller: &Player, _self_input: DVec3) -> DVec3 {
                if self.refuse_to_move() {
                    DVec3::ZERO
                } else {
                    self.horse_ridden_input(controller)
                }
            }

            /// Vanilla `Camel.getRiddenSpeed`.
            fn ridden_speed(&self, controller: &Player) -> f32 {
                let movement_bonus = if controller.is_sprinting() && self.dash_cooldown() == 0 {
                    RUNNING_SPEED_BONUS
                } else {
                    0.0
                };
                self.horse_ridden_speed() + movement_bonus
            }

            fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
                self.horse_ai_step_before();
                let result = Mob::mob_ai_step(self, entity);
                AgeableMob::tick_ageable_mob(self);
                Animal::tick_animal_love(self);
                self.horse_ai_step_after();
                result
            }

            $($living_items)*
        }

        impl AgeableMob for $entity {
            fn ageable_base(&self) -> &AgeableMobBase {
                &self.ageable_base
            }

            fn is_age_locked(&self) -> bool {
                *self.entity_data.lock().ageable_mob().age_locked.get()
            }

            fn set_age_locked(&self, age_locked: bool) {
                self.entity_data
                    .lock()
                    .ageable_mob_mut()
                    .age_locked
                    .set(age_locked);
            }

            fn set_synced_baby(&self, baby: bool) {
                self.entity_data.lock().ageable_mob_mut().baby.set(baby);
            }

            fn age_boundary_changed(&self, _baby: bool) {
                self.refresh_dimensions();
            }

            $($ageable_items)*
        }

        impl Animal for $entity {
            fn animal_base(&self) -> &AnimalBase {
                &self.animal_base
            }

            $($animal_items)*
        }

        impl Mob for $entity {
            fn mob_base(&self) -> &MobBase {
                &self.mob_base
            }

            fn tick_goal_selectors(&self, entity: &SharedEntity) {
                PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
            }

            fn tick_path_navigation(&self) {
                PathfinderMob::tick_pathfinder_path_navigation(self);
            }

            /// Vanilla `Camel.customServerAiStep`.
            fn custom_server_ai_step(&self) {
                if let Some(world) = self.level() {
                    self.brain.tick(&world, self);
                    // Vanilla `CamelAi.updateActivity`.
                    self.brain.set_active_activity_to_first_valid(&[Activity::IDLE]);
                }
                Animal::custom_server_ai_step_animal(self);
            }

            fn ambient_sound(&self) -> Option<SoundEventRef> {
                Some($sounds.ambient)
            }

            fn ambient_sound_interval(&self) -> i32 {
                super::AMBIENT_SOUND_INTERVAL
            }

            fn max_spawn_cluster_size(&self) -> i32 {
                MAX_SPAWN_CLUSTER_SIZE
            }

            fn max_head_y_rot(&self) -> f32 {
                MAX_HEAD_Y_ROT
            }

            /// Vanilla `Camel.finalizeSpawn`.
            fn finalize_spawn(
                &self,
                world: &Arc<World>,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                self.reset_last_pose_change_tick_to_full_stand(world.game_time());
                self.finalize_horse_spawn(world, spawn_reason, group_data)
            }

            fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
                self.camel_mob_interact(player, hand)
            }

            fn mob_flags(&self) -> i8 {
                *self.entity_data.lock().mob().mob_flags.get()
            }

            fn set_mob_flags(&self, flags: i8) {
                self.entity_data.lock().mob_mut().mob_flags.set(flags);
            }

            /// Vanilla `CamelMoveControl.tick`: stands up to walk somewhere.
            fn tick_move_control(&self) {
                let operation = self.mob_base.controls().lock().move_control.operation();
                if operation == MoveControlOperation::MoveTo
                    && !self.is_leashed()
                    && self.is_camel_sitting()
                    && !self.is_in_pose_transition()
                    && self.can_camel_change_pose()
                {
                    self.stand_up();
                }
                self.default_tick_move_control();
            }

            /// Vanilla `CamelLookControl.tick`: the rider steers the head.
            fn tick_look_control(&self) {
                if !self.has_controlling_passenger() {
                    self.default_tick_look_control();
                }
            }

            /// Vanilla `CamelBodyRotationControl.clientTick`.
            fn tick_body_rotation_control(&self) {
                if !self.refuse_to_move() {
                    self.default_tick_body_rotation_control();
                }
            }

            /// Vanilla `Camel.onElasticLeashPull`.
            fn mob_on_elastic_leash_pull(&self) {
                if self.is_camel_sitting()
                    && !self.is_in_pose_transition()
                    && self.can_camel_change_pose()
                {
                    self.stand_up();
                }
            }

            $($mob_items)*
        }

        impl PathfinderMob for $entity {}
    };
}

camel_entity!(
    CamelEntity, CamelEntityData, CAMEL_SOUNDS,
    entity {}
    living {}
    ageable {
        /// Vanilla `Camel.getBreedOffspring`.
        fn get_breed_offspring(
            &self,
            world: &Arc<World>,
            _partner: &dyn AgeableMob,
        ) -> Option<SharedEntity> {
            create_offspring(&vanilla_entities::CAMEL, self, world)
        }
    }
    animal {
        fn is_food(&self, item_stack: &ItemStack) -> bool {
            REGISTRY
                .items
                .is_in_tag(item_stack.item(), &ItemTag::CAMEL_FOOD)
        }

        /// Vanilla `Camel.canMate`.
        fn can_mate(&self, partner: &dyn Animal) -> bool {
            partner.id() != self.id()
                && is_camel(partner)
                && self.can_parent()
                && partner
                    .as_abstract_horse()
                    .is_some_and(<dyn AbstractHorse>::can_parent)
        }
    }
    mob {
        fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
            self.remove_when_far_away_animal(dist_sqr)
        }
    }
    horse {}
);

camel_entity!(
    CamelHuskEntity, CamelHuskEntityData, CAMEL_HUSK_SOUNDS,
    entity {
        /// Vanilla `CamelHusk.interact`: any interaction keeps it around.
        fn interact(
            &self,
            player: &Player,
            hand: InteractionHand,
            location: DVec3,
        ) -> InteractionResult {
            self.set_persistence_required();
            self.interact_mob(player, hand, location)
        }
    }
    living {}
    ageable {
        /// Vanilla `CamelHusk.getBreedOffspring`.
        fn get_breed_offspring(
            &self,
            _world: &Arc<World>,
            _partner: &dyn AgeableMob,
        ) -> Option<SharedEntity> {
            None
        }

        /// Vanilla `CamelHusk.canBeABaby`.
        fn can_be_a_baby(&self) -> bool {
            false
        }
    }
    animal {
        fn is_food(&self, item_stack: &ItemStack) -> bool {
            REGISTRY
                .items
                .is_in_tag(item_stack.item(), &ItemTag::CAMEL_HUSK_FOOD)
        }

        /// Vanilla `CamelHusk.canMate`.
        fn can_mate(&self, _partner: &dyn Animal) -> bool {
            false
        }

        /// Vanilla `CamelHusk.canFallInLove`.
        fn can_fall_in_love(&self) -> bool {
            false
        }
    }
    mob {
        /// Vanilla `CamelHusk.removeWhenFarAway`.
        fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
            true
        }

        /// Vanilla `CamelHusk.canBeLeashed`.
        fn mob_can_be_leashed(&self) -> bool {
            !self.is_mob_controlled()
        }
    }
    horse {
        /// Vanilla `CamelHusk.isMobControlled`.
        fn is_mob_controlled(&self) -> bool {
            self.first_passenger()
                .is_some_and(|passenger| passenger.is_mob())
        }
    }
);

#[cfg(test)]
mod tests;
