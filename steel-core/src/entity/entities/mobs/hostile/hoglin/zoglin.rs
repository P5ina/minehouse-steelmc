//! Vanilla `Zoglin`: a zombified hoglin that attacks anything but zoglins
//! and creepers.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::ZoglinEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use super::{ATTACK_ANIMATION_DURATION, PROBABILITY_OF_SPAWNING_AS_BABY, hurt_and_throw_target};
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::TimedBehavior;
use crate::entity::ai::brain::behaviors::{
    DoNothing, LookAtTargetSink, MoveToTargetSink, UniformInt,
    is_other_target_much_further_away_than_current_attack_target, melee_attack, random_stroll,
    run_one, set_entity_look_target_sometimes,
    set_walk_target_from_attack_target_if_target_out_of_reach, set_walk_target_from_look_target,
    start_attacking, stop_attacking_if_target_invalid, trigger_if,
};
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, CANT_REACH_WALK_TARGET_SINCE, NEAREST_VISIBLE_LIVING_ENTITIES,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable;
use crate::entity::ai::brain::sensors::{NearestLivingEntitySensor, PlayerSensor};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, Brain, Memories, Sensor};
use crate::entity::damage::DamageSource;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster,
    PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 5;
/// Vanilla `ATTACK_INTERVAL` and `BABY_ATTACK_INTERVAL`.
const ATTACK_INTERVAL: i64 = 40;
const BABY_ATTACK_INTERVAL: i64 = 15;
/// Vanilla `ATTACK_DURATION`: how long a hurt zoglin stays angry.
const ATTACK_DURATION: i64 = 200;
const BABY_ATTACK_DAMAGE: f64 = 0.5;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.4;

const BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.875, 0.0)];
/// Vanilla `BABY_DIMENSIONS`.
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.75,
    0.85,
    0.625,
    EntityAttachments::new(&BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);

/// A zoglin.
#[entity_behavior(class = "Zoglin", spawn_rule)]
pub struct ZoglinEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<ZoglinEntityData>,
    brain: Brain<Self>,
    attack_animation_remaining_ticks: AtomicI32,
}

// SAFETY: The owner-scoped type key uniquely identifies ZoglinEntity.
unsafe impl DowncastType for ZoglinEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/zoglin");
}

impl ZoglinEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkAnyLightMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Monster>::check_any_light_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
        )
    };

    /// Creates a new zoglin.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved zoglin.
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
        let mut entity_data = ZoglinEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], Self::sensors(), Self::activities(), None),
            attack_animation_remaining_ticks: AtomicI32::new(0),
        }
    }

    fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
        vec![Box::new(NearestLivingEntitySensor), Box::new(PlayerSensor)]
    }

    /// Vanilla `Zoglin.getActivities`.
    fn activities() -> Vec<ActivityData<Self>> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::fight_activity(),
        ]
    }

    fn core_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::CORE,
            0,
            vec![
                Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
                Box::new(TimedBehavior::new(MoveToTargetSink::new())),
            ],
        )
    }

    fn idle_activity() -> ActivityData<Self> {
        let wander: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
            (
                Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true)),
                2,
            ),
            (
                Box::new(set_walk_target_from_look_target(
                    |_| true,
                    |_| SPEED_MULTIPLIER_WHEN_IDLING,
                    3,
                )),
                2,
            ),
            (Box::new(DoNothing::new(30, 60)), 1),
        ];
        ActivityData::with_first_priority(
            Activity::IDLE,
            10,
            vec![
                Box::new(start_attacking(|world: &Arc<World>, body: &Self| body.find_nearest_valid_attack_target(world),
                )),
                Box::new(set_entity_look_target_sometimes(
                    8.0,
                    UniformInt::of(30, 60),
                    |_| true,
                )),
                Box::new(run_one(Vec::new(), wander)),
            ],
        )
    }

    fn fight_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::FIGHT,
            10,
            vec![
                Box::new(set_walk_target_from_attack_target_if_target_out_of_reach(
                    |_| 1.0,
                )),
                Box::new(trigger_if(
                    |zoglin: &Self| !zoglin.is_baby(),
                    melee_attack(ATTACK_INTERVAL),
                )),
                Box::new(trigger_if(
                    |zoglin: &Self| zoglin.is_baby(),
                    melee_attack(BABY_ATTACK_INTERVAL),
                )),
                Box::new(stop_attacking_if_target_invalid(|_, _, _| {})),
            ],
        )
        .while_memory_present(&ATTACK_TARGET)
    }

    /// Vanilla `findNearestValidAttackTarget`.
    pub(super) fn find_nearest_valid_attack_target(
        &self,
        world: &Arc<World>,
    ) -> Option<SharedEntity> {
        let visible = self
            .brain
            .memories()
            .lock()
            .get(&NEAREST_VISIBLE_LIVING_ENTITIES)?;
        visible.find_closest(|target| {
            target.entity_type() != &vanilla_entities::ZOGLIN
                && target.entity_type() != &vanilla_entities::CREEPER
                && target
                    .as_living_entity()
                    .is_some_and(|living| is_entity_attackable(world, self, living))
        })
    }

    /// The zoglin's brain memories.
    #[must_use]
    pub const fn memories(&self) -> &SyncMutex<Memories> {
        self.brain.memories()
    }

    /// Ticks left of the head-swing animation.
    #[must_use]
    pub fn attack_animation_remaining_ticks(&self) -> i32 {
        self.attack_animation_remaining_ticks
            .load(Ordering::Relaxed)
    }

    /// Vanilla `setAttackTarget`.
    fn set_attack_target(&self, target: SharedEntity) {
        let mut memories = self.brain.memories().lock();
        memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
        memories.set_with_expiry(&ATTACK_TARGET, target, ATTACK_DURATION);
    }

    /// Vanilla `updateActivity`.
    fn update_activity(&self) {
        let old_activity = self.brain.active_non_core_activity();
        self.brain
            .set_active_activity_to_first_valid(&[Activity::FIGHT, Activity::IDLE]);
        let new_activity = self.brain.active_non_core_activity();
        if new_activity == Some(Activity::FIGHT) && old_activity != Some(Activity::FIGHT) {
            self.make_sound(Some(&sound_events::ENTITY_ZOGLIN_ANGRY));
        }
        let has_target = self.brain.memories().lock().has_value(&ATTACK_TARGET);
        self.set_aggressive(has_target);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl DefaultEntityPush for ZoglinEntity {}

impl Entity for ZoglinEntity {
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

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if self.is_baby() {
            BABY_DIMENSIONS.scale(scale)
        } else if self.entity_type.fixed {
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
        self.play_sound(&sound_events::ENTITY_ZOGLIN_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert("IsBaby", i8::from(self.is_baby()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        self.set_baby(nbt.byte("IsBaby").is_some_and(|baby| baby != 0));
    }
}

impl LivingEntity for ZoglinEntity {
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
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn is_baby(&self) -> bool {
        *self.entity_data.lock().baby.get()
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOGLIN_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOGLIN_DEATH)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let was_hurt = self.default_hurt_server(world, source, amount);
        if was_hurt
            && let Some(attacker) = source.causing_entity()
            && let Some(living_attacker) = attacker.as_living_entity()
            && Mob::can_attack(self, living_attacker)
            && !is_other_target_much_further_away_than_current_attack_target(
                self,
                attacker.as_ref(),
                4.0,
            )
        {
            self.set_attack_target(Arc::clone(attacker));
        }
        was_hurt
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
        let ticks = self
            .attack_animation_remaining_ticks
            .load(Ordering::Relaxed);
        if ticks > 0 {
            self.attack_animation_remaining_ticks
                .store(ticks - 1, Ordering::Relaxed);
        }
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for ZoglinEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `getTarget`: the brain's attack target.
    fn target(&self) -> Option<SharedEntity> {
        self.brain
            .memories()
            .lock()
            .get(&ATTACK_TARGET)
            .filter(|target| {
                target
                    .as_living_entity()
                    .is_some_and(|living| self.is_valid_target(living))
            })
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
        }
        self.update_activity();
    }

    fn set_baby(&self, baby: bool) {
        self.entity_data.lock().baby.set(baby);
        if baby {
            self.attributes()
                .lock()
                .set_base_value(vanilla_attributes::ATTACK_DAMAGE, BABY_ATTACK_DAMAGE);
        }
        self.refresh_dimensions();
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        if !target.is_living_entity() {
            return false;
        }
        self.attack_animation_remaining_ticks
            .store(ATTACK_ANIMATION_DURATION, Ordering::Relaxed);
        self.broadcast_entity_event(EntityStatus::StartAttacking);
        self.make_sound(Some(&sound_events::ENTITY_ZOGLIN_ATTACK));
        hurt_and_throw_target(world, entity, target)
    }

    fn mob_can_be_leashed(&self) -> bool {
        true
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.brain.memories().lock().has_value(&ATTACK_TARGET) {
            &sound_events::ENTITY_ZOGLIN_ANGRY
        } else {
            &sound_events::ENTITY_ZOGLIN_AMBIENT
        })
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if rand::random::<f32>() < PROBABILITY_OF_SPAWNING_AS_BABY {
            self.set_baby(true);
        }
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for ZoglinEntity {}

impl Monster for ZoglinEntity {}
