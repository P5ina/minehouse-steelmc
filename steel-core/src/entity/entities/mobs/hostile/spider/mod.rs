//! Vanilla `Spider` and `CaveSpider`.
//!
//! Both are wall climbers: their navigation steers straight at a target it
//! cannot path to, and they climb any wall they are pushed against.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::mob_effect::MobEffectRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::{CaveSpiderEntityData, SpiderEntityData};
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Downcast as _, DowncastType, DowncastTypeKey};

use crate::entity::ai::goal::{
    FloatGoal, Goal, GoalControls, HurtByTargetGoal, LeapAtTargetGoal, LookAtPlayerGoal,
    MeleeAttackGoal, NearestAttackableTargetGoal, RandomLookAroundGoal,
    WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::SkeletonEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::spawn::create_entity_instance;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    MobEffectInstance, Monster, PathfinderMob, SharedEntity, SpawnGroupData,
    sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::{LevelReader as _, World};

const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const XP_REWARD: i32 = 5;
const CLIMBING_FLAG: i8 = 1;
/// Vanilla `Spider.SPIDER_SPECIAL_EFFECT_CHANCE`.
const SPIDER_SPECIAL_EFFECT_CHANCE: f32 = 0.1;
/// Vanilla `random.nextInt(100) == 0`: one spider in a hundred is a jockey.
const SKELETON_JOCKEY_BOUND: i32 = 100;
/// Brightness at which spiders stop picking and keeping targets.
const BRIGHT_LIGHT: f32 = 0.5;
const STEP_SOUND_VOLUME: f32 = 0.15;
/// Vanilla spider vehicle attachment height, before scale.
const SPIDER_VEHICLE_ATTACHMENT: f64 = 0.3125;
/// Vanilla cave spider vehicle attachment height, before scale.
const CAVE_SPIDER_VEHICLE_ATTACHMENT: f64 = 0.21875;
/// Vanilla `CaveSpider.doHurtTarget` poison seconds on Normal and Hard.
const CAVE_SPIDER_POISON_SECONDS_NORMAL: i32 = 7;
const CAVE_SPIDER_POISON_SECONDS_HARD: i32 = 15;

/// Vanilla `Entity.getLightLevelDependentMagicValue`, taken at eye height.
fn light_level_dependent_magic_value(mob: &dyn PathfinderMob) -> f32 {
    let Some(world) = mob.level() else {
        return 0.0;
    };
    let position = mob.position();
    let eye_pos = BlockPos::containing(position.x, mob.get_eye_y(), position.z);
    if !world.has_full_chunk(ChunkPos::from_block_pos(eye_pos)) {
        return 0.0;
    }
    world.light_level_dependent_magic_value(eye_pos)
}

/// Vanilla `Spider.SpiderEffectsGroupData.setRandomEffect`.
fn random_spider_effect() -> MobEffectRef {
    match rand::random_range(0..5) {
        0 | 1 => vanilla_mob_effects::SPEED,
        2 => vanilla_mob_effects::STRENGTH,
        3 => vanilla_mob_effects::REGENERATION,
        _ => vanilla_mob_effects::INVISIBILITY,
    }
}

/// Vanilla `Spider.SpiderAttackGoal`: melee that gives up in bright light.
struct SpiderAttackGoal {
    melee: MeleeAttackGoal,
}

impl SpiderAttackGoal {
    const fn new() -> Self {
        Self {
            melee: MeleeAttackGoal::new(1.0, true),
        }
    }
}

impl Goal for SpiderAttackGoal {
    fn controls(&self) -> GoalControls {
        self.melee.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.melee.can_use(mob) && !mob.is_vehicle()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let brightness = light_level_dependent_magic_value(mob);
        if brightness >= BRIGHT_LIGHT && rand::random_range(0..100) == 0 {
            let _ = mob.set_target(None);
            return false;
        }
        self.melee.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.melee.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.melee.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.melee.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.melee.tick(mob, entity);
    }
}

/// Vanilla `Spider.SpiderTargetGoal`: only picks targets in the dark.
struct SpiderTargetGoal {
    nearest: NearestAttackableTargetGoal,
}

impl Goal for SpiderTargetGoal {
    fn controls(&self) -> GoalControls {
        self.nearest.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if light_level_dependent_magic_value(mob) >= BRIGHT_LIGHT {
            return false;
        }
        self.nearest.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.nearest.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.nearest.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.nearest.tick(mob, entity);
    }
}

/// Builds the shared vanilla `Spider` mob base: wall-climber navigation and goals.
fn spider_mob_base() -> MobBase {
    let mob_base = MobBase::new();
    mob_base.set_xp_reward(XP_REWARD);
    mob_base.navigation().lock().set_wall_climber(true);
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        goal_selector.add_goal(1, FloatGoal::new(&mob_base));
        // Vanilla priority 2 avoids armadillos that are not scared; armadillos
        // are not implemented in Steel yet.
        goal_selector.add_goal(3, LeapAtTargetGoal::new(0.4));
        goal_selector.add_goal(4, SpiderAttackGoal::new());
        goal_selector.add_goal(5, WaterAvoidingRandomStrollGoal::new(0.8));
        goal_selector.add_goal(6, LookAtPlayerGoal::new(8.0));
        goal_selector.add_goal(6, RandomLookAroundGoal::new());

        let mut target_selector = mob_base.target_selector().lock();
        target_selector.add_goal(1, HurtByTargetGoal::new());
        target_selector.add_goal(
            2,
            SpiderTargetGoal {
                nearest: NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            },
        );
        target_selector.add_goal(
            3,
            SpiderTargetGoal {
                nearest: NearestAttackableTargetGoal::new(true, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            },
        );
    }
    mob_base
}

/// Implements the entity traits both spiders share, mirroring vanilla `Spider`.
///
/// `$attachment` is the vehicle attachment height before scale.
macro_rules! impl_spider_traits {
    ($entity:ty, $attachment:expr) => {
        impl $entity {
            /// Vanilla `Spider.isClimbing`.
            #[must_use]
            pub fn is_climbing(&self) -> bool {
                *self.entity_data.lock().spider().flags.get() & CLIMBING_FLAG != 0
            }

            /// Vanilla `Spider.setClimbing`.
            pub fn set_climbing(&self, climbing: bool) {
                let mut entity_data = self.entity_data.lock();
                let flags = *entity_data.spider().flags.get();
                let flags = if climbing {
                    flags | CLIMBING_FLAG
                } else {
                    flags & !CLIMBING_FLAG
                };
                entity_data.spider_mut().flags.set(flags);
            }

            fn update_dirty_mob_effect_entity_data(&self) {
                sync_dirty_mob_effects(&self.living_base, &self.entity_data);
            }
        }

        impl DefaultEntityPush for $entity {}

        impl Entity for $entity {
            fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
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

            fn tick(self: Arc<Self>) {
                let entity = Arc::clone(&self).into_shared_entity();
                if let Some(living) = entity.as_living_entity() {
                    living.tick_living_entity(&entity);
                }
                if !self.is_removed() {
                    self.set_climbing(self.horizontal_collision());
                }
            }

            fn base_tick(&self) {
                Mob::base_tick_mob(self);
            }

            fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
                let scale = LivingEntity::get_scale(self);
                if self.entity_type.fixed {
                    self.entity_type.dimensions
                } else {
                    self.entity_type.dimensions.scale(scale)
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
                    .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
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

            fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
                self.play_sound(&sound_events::ENTITY_SPIDER_STEP, STEP_SOUND_VOLUME, 1.0);
            }

            fn on_climbable(&self) -> bool {
                self.is_climbing()
            }

            fn make_stuck_in_block(&self, state: BlockStateId, speed_multiplier: DVec3) {
                // Spiders walk through cobwebs at full speed.
                if state.get_block() != &vanilla_blocks::COBWEB {
                    self.default_make_stuck_in_block(state, speed_multiplier);
                }
            }

            fn vehicle_attachment_point(&self, vehicle: &dyn Entity) -> DVec3 {
                if vehicle.base().dimensions().width <= self.base().dimensions().width {
                    DVec3::new(
                        0.0,
                        $attachment * f64::from(LivingEntity::get_scale(self)),
                        0.0,
                    )
                } else {
                    self.default_vehicle_attachment_point(vehicle)
                }
            }

            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_mob(nbt);
            }

            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_mob(nbt);
            }
        }

        impl LivingEntity for $entity {
            fn living_base(&self) -> &LivingEntityBase {
                &self.living_base
            }

            fn get_health(&self) -> f32 {
                *self.entity_data.lock().living_entity().health.get()
            }

            fn set_health(&self, health: f32) {
                let max_health = self.get_max_health();
                let clamped = health.clamp(0.0, max_health);
                self.entity_data
                    .lock()
                    .living_entity_mut()
                    .health
                    .set(clamped);
            }

            fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_SPIDER_HURT)
            }

            fn death_sound(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_SPIDER_DEATH)
            }

            fn can_be_affected(&self, effect: &MobEffectInstance) -> bool {
                effect.effect() != vanilla_mob_effects::POISON
                    && self.default_can_be_affected(effect)
            }

            fn should_drop_experience(&self) -> bool {
                Monster::should_drop_experience_monster(self)
            }

            fn should_drop_loot(&self, world: &World) -> bool {
                Monster::should_drop_loot_monster(self, world)
            }

            fn server_ai_step(&self, entity: &SharedEntity) {
                Mob::mob_server_ai_step(self, entity);
            }

            fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
                Monster::monster_ai_step(self, entity)
            }
        }

        impl Monster for $entity {}

        impl PathfinderMob for $entity {}
    };
}

/// A hostile spider.
#[entity_behavior(class = "Spider")]
pub struct SpiderEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<SpiderEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies SpiderEntity.
unsafe impl DowncastType for SpiderEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/spider");
}

impl SpiderEntity {
    /// Creates a new spider.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved spider.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = SpiderEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: spider_mob_base(),
            entity_data: SyncMutex::new(entity_data),
        }
    }
}

impl SpiderEntity {
    /// The skeleton half of vanilla `Spider.finalizeSpawn`: a fresh skeleton
    /// that rides this spider.
    pub(crate) fn spawn_skeleton_jockey_rider(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
    ) {
        let Ok(skeleton) =
            create_entity_instance(world, &vanilla_entities::SKELETON, self.position())
        else {
            return;
        };
        let (yaw, _) = self.rotation();
        skeleton.set_rotation((yaw, 0.0));
        if let Some(skeleton_mob) = skeleton.as_mob() {
            let _ = skeleton_mob.finalize_spawn(world, spawn_reason, None);
        }
        if let Some(rider) = skeleton.downcast_ref::<SkeletonEntity>() {
            rider.ride_once_spawned(self.id());
        }
        let _ = world.try_add_entity(skeleton);
    }

    /// Vanilla `Spider.finalizeSpawn`.
    fn finalize_spider_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let difficulty = world.current_difficulty_at(self.block_position());
        let group_data = self.finalize_spawn_mob_base(world, spawn_reason, group_data);
        if rand::random_range(0..SKELETON_JOCKEY_BOUND) == 0 {
            self.spawn_skeleton_jockey_rider(world, spawn_reason);
        }
        let group_data = group_data.or_else(|| {
            let effect = (world.difficulty() == Difficulty::Hard
                && rand::random::<f32>()
                    < SPIDER_SPECIAL_EFFECT_CHANCE * difficulty.special_multiplier())
            .then(random_spider_effect);
            Some(SpawnGroupData::SpiderEffects(effect))
        });
        if let Some(SpawnGroupData::SpiderEffects(Some(effect))) = group_data {
            let _ = self.add_mob_effect(MobEffectInstance::new(effect, 0));
        }
        group_data
    }
}

impl_spider_traits!(SpiderEntity, SPIDER_VEHICLE_ATTACHMENT);

impl Mob for SpiderEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SPIDER_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spider_spawn(world, spawn_reason, group_data)
    }
}

/// A hostile cave spider: a smaller spider whose bite poisons.
#[entity_behavior(class = "CaveSpider")]
pub struct CaveSpiderEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<CaveSpiderEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies CaveSpiderEntity.
unsafe impl DowncastType for CaveSpiderEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/cave_spider");
}

impl CaveSpiderEntity {
    /// Creates a new cave spider.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved cave spider.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = CaveSpiderEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: spider_mob_base(),
            entity_data: SyncMutex::new(entity_data),
        }
    }
}

impl_spider_traits!(CaveSpiderEntity, CAVE_SPIDER_VEHICLE_ATTACHMENT);

impl Mob for CaveSpiderEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SPIDER_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        if !self.default_do_hurt_target(entity, world, target) {
            return false;
        }
        if let Some(living) = target.as_living_entity() {
            let poison_seconds = match world.difficulty() {
                Difficulty::Normal => CAVE_SPIDER_POISON_SECONDS_NORMAL,
                Difficulty::Hard => CAVE_SPIDER_POISON_SECONDS_HARD,
                Difficulty::Peaceful | Difficulty::Easy => 0,
            };
            if poison_seconds > 0 {
                let _ = living.add_mob_effect_with_source(
                    MobEffectInstance::with_duration(
                        vanilla_mob_effects::POISON,
                        poison_seconds * 20,
                        0,
                    ),
                    Some(self),
                );
            }
        }
        true
    }

    /// Vanilla `CaveSpider.finalizeSpawn` returns the group data untouched,
    /// skipping both the mob base and the spider effects.
    fn finalize_spawn(
        &self,
        _world: &Arc<World>,
        _spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        group_data
    }
}

#[cfg(test)]
mod tests;
