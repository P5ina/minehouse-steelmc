//! Vanilla `Hoglin` and `HoglinAi`: a Nether boar that charges players,
//! flees from repellents and outnumbering piglins, and zombifies outside
//! the Nether.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::HoglinEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_entities, vanilla_mob_effects,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};

use super::sensor::HoglinSpecificSensor;
use super::{ATTACK_ANIMATION_DURATION, PROBABILITY_OF_SPAWNING_AS_BABY, hurt_and_throw_target};
use crate::behavior::InteractionResult;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::TimedBehavior;
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, DoNothing, LookAtTargetSink, MoveToTargetSink, UniformInt, baby_follow_adult,
    become_passive_if_memory_present, erase_memory_if, get_nearest_target,
    is_other_target_much_further_away_than_current_attack_target, melee_attack, random_stroll,
    run_one, set_entity_look_target_sometimes, set_walk_target_away_from_entity,
    set_walk_target_away_from_pos, set_walk_target_from_attack_target_if_target_out_of_reach,
    set_walk_target_from_look_target, start_attacking, stop_attacking_if_target_invalid,
    trigger_if,
};
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, AVOID_TARGET, BREED_TARGET, CANT_REACH_WALK_TARGET_SINCE, NEAREST_REPELLENT,
    NEAREST_VISIBLE_ADULT, NEAREST_VISIBLE_ADULT_HOGLINS, NEAREST_VISIBLE_ADULT_PIGLIN,
    NEAREST_VISIBLE_ATTACKABLE_PLAYER, PACIFIED, VISIBLE_ADULT_HOGLIN_COUNT,
    VISIBLE_ADULT_PIGLIN_COUNT, WALK_TARGET,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable;
use crate::entity::ai::brain::sensors::{AdultSensor, NearestLivingEntitySensor, PlayerSensor};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, Brain, Memories, Sensor};
use crate::entity::conversion::ConversionParams;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Enemy, Entity, EntityBase,
    EntityBaseLoad, EntityMovementEmission, EntityPose, EntitySpawnReason, EntitySyncedData,
    LivingEntity, LivingEntityBase, Mob, MobBase, MobEffectInstance, PathfinderMob, SharedEntity,
    SpawnGroupData, SpawnRule, next_entity_id, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, ServerLevelAccessor, World};

const XP_REWARD: i32 = 5;
const BABY_XP_REWARD: i32 = 3;
const ATTACK_DAMAGE: f64 = 6.0;
const BABY_ATTACK_DAMAGE: f64 = 0.5;
/// Vanilla `Hoglin.CONVERSION_TIME`.
const CONVERSION_TIME: i32 = 300;
/// Ticks of nausea a fresh zoglin gets.
const CONVERSION_NAUSEA_TICKS: i32 = 200;

/// Vanilla `HoglinAi.RETREAT_DURATION` (5 to 20 seconds).
const RETREAT_DURATION: UniformInt = UniformInt::of(100, 400);
/// Vanilla `HoglinAi.ATTACK_DURATION`.
const ATTACK_DURATION: i64 = 200;
const DESIRED_DISTANCE_FROM_PIGLIN_WHEN_IDLING: i32 = 8;
const DESIRED_DISTANCE_FROM_PIGLIN_WHEN_RETREATING: i32 = 15;
const DESIRED_DISTANCE_FROM_REPELLENT: i32 = 8;
const ATTACK_INTERVAL: i64 = 40;
const BABY_ATTACK_INTERVAL: i64 = 15;
const REPELLENT_PACIFY_TIME: i64 = 200;
const ADULT_FOLLOW_RANGE: UniformInt = UniformInt::of(5, 16);
const SPEED_MULTIPLIER_WHEN_AVOIDING_REPELLENT: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_RETREATING: f32 = 1.3;
const SPEED_MULTIPLIER_WHEN_MAKING_LOVE: f32 = 0.6;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.4;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 0.6;
/// Blocks within which a walk target counts as near the repellent.
const REPELLENT_AVOID_RADIUS: f64 = 8.0;

const BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.875, 0.0)];
/// Vanilla `BABY_DIMENSIONS`.
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.75,
    0.85,
    0.625,
    EntityAttachments::new(&BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);

/// A hoglin.
#[entity_behavior(class = "Hoglin", spawn_rule)]
pub struct HoglinEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<HoglinEntityData>,
    brain: Brain<Self>,
    attack_animation_remaining_ticks: AtomicI32,
    time_in_overworld: AtomicI32,
    cannot_be_hunted: SyncMutex<bool>,
}

// SAFETY: The owner-scoped type key uniquely identifies HoglinEntity.
unsafe impl DowncastType for HoglinEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/hoglin");
}

impl HoglinEntity {
    /// Vanilla `Hoglin.checkHoglinSpawnRules`: anywhere but on a nether wart
    /// block.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_hoglin_spawn_rules;

    fn check_hoglin_spawn_rules(
        _entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        _spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        level.get_block_state(pos.below()).get_block() != &vanilla_blocks::NETHER_WART_BLOCK
    }

    /// Creates a new hoglin.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved hoglin.
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
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        let mut entity_data = HoglinEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            animal_base: AnimalBase::new(),
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], Self::sensors(), Self::activities(), None),
            attack_animation_remaining_ticks: AtomicI32::new(0),
            time_in_overworld: AtomicI32::new(0),
            cannot_be_hunted: SyncMutex::new(false),
        }
    }

    fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
        vec![
            Box::new(NearestLivingEntitySensor),
            Box::new(PlayerSensor),
            Box::new(AdultSensor),
            Box::new(HoglinSpecificSensor),
        ]
    }

    /// Vanilla `HoglinAi.getActivities`.
    fn activities() -> Vec<ActivityData<Self>> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::fight_activity(),
            Self::retreat_activity(),
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
        ActivityData::with_first_priority(
            Activity::IDLE,
            10,
            vec![
                Box::new(become_passive_if_memory_present(
                    &NEAREST_REPELLENT,
                    REPELLENT_PACIFY_TIME,
                )),
                Box::new(TimedBehavior::new(AnimalMakeLove::with_speed(
                    &vanilla_entities::HOGLIN,
                    SPEED_MULTIPLIER_WHEN_MAKING_LOVE,
                    2,
                ))),
                Box::new(set_walk_target_away_from_pos(
                    &NEAREST_REPELLENT,
                    SPEED_MULTIPLIER_WHEN_AVOIDING_REPELLENT,
                    DESIRED_DISTANCE_FROM_REPELLENT,
                    true,
                )),
                Box::new(start_attacking(
                    |_, _| true,
                    |_world: &Arc<World>, body: &Self| body.find_nearest_valid_attack_target(),
                )),
                Box::new(trigger_if(
                    Self::is_adult,
                    set_walk_target_away_from_entity(
                        &NEAREST_VISIBLE_ADULT_PIGLIN,
                        SPEED_MULTIPLIER_WHEN_IDLING,
                        DESIRED_DISTANCE_FROM_PIGLIN_WHEN_IDLING,
                        false,
                    ),
                )),
                Box::new(set_entity_look_target_sometimes(
                    8.0,
                    UniformInt::of(30, 60),
                    |_| true,
                )),
                Box::new(baby_follow_adult(
                    ADULT_FOLLOW_RANGE,
                    |_| SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                    &NEAREST_VISIBLE_ADULT,
                    false,
                )),
                Box::new(Self::idle_movement_behaviors()),
            ],
        )
    }

    fn fight_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::FIGHT,
            10,
            vec![
                Box::new(become_passive_if_memory_present(
                    &NEAREST_REPELLENT,
                    REPELLENT_PACIFY_TIME,
                )),
                Box::new(TimedBehavior::new(AnimalMakeLove::with_speed(
                    &vanilla_entities::HOGLIN,
                    SPEED_MULTIPLIER_WHEN_MAKING_LOVE,
                    2,
                ))),
                Box::new(set_walk_target_from_attack_target_if_target_out_of_reach(
                    |_| 1.0,
                )),
                Box::new(trigger_if(
                    Self::is_adult,
                    melee_attack(|_| true, ATTACK_INTERVAL),
                )),
                Box::new(trigger_if(
                    |hoglin: &Self| AgeableMob::is_baby(hoglin),
                    melee_attack(|_| true, BABY_ATTACK_INTERVAL),
                )),
                Box::new(stop_attacking_if_target_invalid(
                    |_, _| false,
                    |_, _, _| {},
                    true,
                )),
                Box::new(erase_memory_if(Self::is_breeding, &ATTACK_TARGET)),
            ],
        )
        .while_memory_present(&ATTACK_TARGET)
    }

    fn retreat_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::AVOID,
            10,
            vec![
                Box::new(set_walk_target_away_from_entity(
                    &AVOID_TARGET,
                    SPEED_MULTIPLIER_WHEN_RETREATING,
                    DESIRED_DISTANCE_FROM_PIGLIN_WHEN_RETREATING,
                    false,
                )),
                Box::new(Self::idle_movement_behaviors()),
                Box::new(set_entity_look_target_sometimes(
                    8.0,
                    UniformInt::of(30, 60),
                    |_| true,
                )),
                Box::new(erase_memory_if(Self::wants_to_stop_fleeing, &AVOID_TARGET)),
            ],
        )
        .while_memory_present(&AVOID_TARGET)
    }

    fn idle_movement_behaviors() -> impl BehaviorControl<Self> {
        let behaviors: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
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
        run_one(Vec::new(), behaviors)
    }

    /// The hoglin's brain memories.
    #[must_use]
    pub const fn memories(&self) -> &SyncMutex<Memories> {
        self.brain.memories()
    }

    /// Vanilla `isAdult`.
    #[must_use]
    pub fn is_adult(&self) -> bool {
        !AgeableMob::is_baby(self)
    }

    /// Ticks left of the head-swing animation.
    #[must_use]
    pub fn attack_animation_remaining_ticks(&self) -> i32 {
        self.attack_animation_remaining_ticks
            .load(Ordering::Relaxed)
    }

    /// Vanilla `setImmuneToZombification`.
    pub fn set_immune_to_zombification(&self, immune: bool) {
        self.entity_data.lock().immune_to_zombification.set(immune);
    }

    fn is_immune_to_zombification(&self) -> bool {
        *self.entity_data.lock().immune_to_zombification.get()
    }

    /// Vanilla `setTimeInOverworld`.
    pub fn set_time_in_overworld(&self, ticks: i32) {
        self.time_in_overworld.store(ticks, Ordering::Relaxed);
    }

    /// Vanilla `isConverting`: zombifying in a dimension where piglins do.
    #[must_use]
    pub fn is_converting(&self) -> bool {
        !self.is_immune_to_zombification()
            && !self.is_no_ai()
            && self
                .level()
                .is_some_and(|world| world.dimension_type.piglins_zombify)
    }

    /// Vanilla `canBeHunted`: piglins may hunt adults that were not bred.
    #[must_use]
    pub fn can_be_hunted(&self) -> bool {
        self.is_adult() && !*self.cannot_be_hunted.lock()
    }

    /// Vanilla `finishConversion`.
    fn finish_conversion(&self) {
        let _ = self.convert_to(
            &vanilla_entities::ZOGLIN,
            ConversionParams::single(true, false),
            Some(&|zoglin: &dyn Mob| {
                zoglin.add_mob_effect(MobEffectInstance::with_duration(
                    vanilla_mob_effects::NAUSEA,
                    CONVERSION_NAUSEA_TICKS,
                    0,
                ));
            }),
        );
    }

    // --- HoglinAi ---

    /// Vanilla `HoglinAi.updateActivity`.
    fn update_activity(&self) {
        let old_activity = self.brain.active_non_core_activity();
        self.brain.set_active_activity_to_first_valid(&[
            Activity::FIGHT,
            Activity::AVOID,
            Activity::IDLE,
        ]);
        let new_activity = self.brain.active_non_core_activity();
        if old_activity != new_activity
            && let Some(sound) = self.sound_for_current_activity()
        {
            self.make_sound(Some(sound));
        }
        let has_target = self.brain.memories().lock().has_value(&ATTACK_TARGET);
        self.set_aggressive(has_target);
    }

    /// Vanilla `HoglinAi.onHitTarget`.
    fn on_hit_target(&self, target: &SharedEntity) {
        if AgeableMob::is_baby(self) {
            return;
        }
        if target.entity_type() == &vanilla_entities::PIGLIN && self.piglins_outnumber_hoglins() {
            self.set_avoid_target(Arc::clone(target));
            self.broadcast_retreat(target);
        } else {
            self.broadcast_attack_target(target);
        }
    }

    fn visible_adult_hoglins(&self) -> Vec<SharedEntity> {
        self.brain
            .memories()
            .lock()
            .get(&NEAREST_VISIBLE_ADULT_HOGLINS)
            .unwrap_or_default()
    }

    fn broadcast_retreat(&self, target: &SharedEntity) {
        for hoglin in self.visible_adult_hoglins() {
            if let Some(hoglin) = hoglin.downcast_ref::<Self>() {
                hoglin.retreat_from_nearest_target(Arc::clone(target));
            }
        }
    }

    /// Vanilla `retreatFromNearestTarget`.
    fn retreat_from_nearest_target(&self, new_avoid_target: SharedEntity) {
        let (avoid_target, attack_target) = {
            let memories = self.brain.memories().lock();
            (memories.get(&AVOID_TARGET), memories.get(&ATTACK_TARGET))
        };
        let nearest = get_nearest_target(self, avoid_target, new_avoid_target);
        let nearest = get_nearest_target(self, attack_target, nearest);
        self.set_avoid_target(nearest);
    }

    /// Vanilla `setAvoidTarget`.
    fn set_avoid_target(&self, avoid_target: SharedEntity) {
        let mut memories = self.brain.memories().lock();
        memories.erase(&ATTACK_TARGET);
        memories.erase(&WALK_TARGET);
        memories.set_with_expiry(
            &AVOID_TARGET,
            avoid_target,
            i64::from(RETREAT_DURATION.sample()),
        );
    }

    /// Vanilla `findNearestValidAttackTarget`.
    fn find_nearest_valid_attack_target(&self) -> Option<SharedEntity> {
        if Self::is_pacified(self) || Self::is_breeding(self) {
            return None;
        }
        self.brain
            .memories()
            .lock()
            .get(&NEAREST_VISIBLE_ATTACKABLE_PLAYER)
    }

    /// Vanilla `isPosNearNearestRepellent`.
    fn is_pos_near_nearest_repellent(&self, pos: BlockPos) -> bool {
        self.brain
            .memories()
            .lock()
            .get(&NEAREST_REPELLENT)
            .is_some_and(|repellent| {
                let dx = f64::from(repellent.x() - pos.x());
                let dy = f64::from(repellent.y() - pos.y());
                let dz = f64::from(repellent.z() - pos.z());
                dx * dx + dy * dy + dz * dz < REPELLENT_AVOID_RADIUS * REPELLENT_AVOID_RADIUS
            })
    }

    fn wants_to_stop_fleeing(&self) -> bool {
        self.is_adult() && !self.piglins_outnumber_hoglins()
    }

    fn piglins_outnumber_hoglins(&self) -> bool {
        if AgeableMob::is_baby(self) {
            return false;
        }
        let memories = self.brain.memories().lock();
        let piglin_count = memories.get(&VISIBLE_ADULT_PIGLIN_COUNT).unwrap_or(0);
        let hoglin_count = memories.get(&VISIBLE_ADULT_HOGLIN_COUNT).unwrap_or(0) + 1;
        piglin_count > hoglin_count
    }

    /// Vanilla `HoglinAi.wasHurtBy`.
    fn was_hurt_by(&self, world: &World, attacker: &SharedEntity) {
        {
            let mut memories = self.brain.memories().lock();
            memories.erase(&PACIFIED);
            memories.erase(&BREED_TARGET);
        }
        if AgeableMob::is_baby(self) {
            self.retreat_from_nearest_target(Arc::clone(attacker));
        } else {
            self.maybe_retaliate(world, attacker);
        }
    }

    fn maybe_retaliate(&self, world: &World, attacker: &SharedEntity) {
        if self.brain.is_active(Activity::AVOID)
            && attacker.entity_type() == &vanilla_entities::PIGLIN
        {
            return;
        }
        if attacker.entity_type() == &vanilla_entities::HOGLIN {
            return;
        }
        if is_other_target_much_further_away_than_current_attack_target(
            self,
            attacker.as_ref(),
            4.0,
        ) {
            return;
        }
        if attacker
            .as_living_entity()
            .is_some_and(|living| is_entity_attackable(world, self, living))
        {
            self.set_attack_target(Arc::clone(attacker));
            self.broadcast_attack_target(attacker);
        }
    }

    /// Vanilla `setAttackTarget`.
    fn set_attack_target(&self, target: SharedEntity) {
        let mut memories = self.brain.memories().lock();
        memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
        memories.erase(&BREED_TARGET);
        memories.set_with_expiry(&ATTACK_TARGET, target, ATTACK_DURATION);
    }

    fn broadcast_attack_target(&self, target: &SharedEntity) {
        for hoglin in self.visible_adult_hoglins() {
            if let Some(hoglin) = hoglin.downcast_ref::<Self>() {
                hoglin.set_attack_target_if_closer_than_current(Arc::clone(target));
            }
        }
    }

    fn set_attack_target_if_closer_than_current(&self, new_target: SharedEntity) {
        if Self::is_pacified(self) {
            return;
        }
        let current = self.brain.memories().lock().get(&ATTACK_TARGET);
        let nearest = get_nearest_target(self, current, new_target);
        self.set_attack_target(nearest);
    }

    /// Vanilla `getSoundForCurrentActivity`.
    fn sound_for_current_activity(&self) -> Option<SoundEventRef> {
        let activity = self.brain.active_non_core_activity()?;
        Some(if activity == Activity::AVOID || self.is_converting() {
            &sound_events::ENTITY_HOGLIN_RETREAT
        } else if activity == Activity::FIGHT {
            &sound_events::ENTITY_HOGLIN_ANGRY
        } else if self.brain.memories().lock().has_value(&NEAREST_REPELLENT) {
            &sound_events::ENTITY_HOGLIN_RETREAT
        } else {
            &sound_events::ENTITY_HOGLIN_AMBIENT
        })
    }

    fn is_breeding(&self) -> bool {
        self.brain.memories().lock().has_value(&BREED_TARGET)
    }

    /// Vanilla `HoglinAi.isPacified`.
    fn is_pacified(&self) -> bool {
        self.brain.memories().lock().has_value(&PACIFIED)
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl DefaultEntityPush for HoglinEntity {}

impl Entity for HoglinEntity {
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
        if AgeableMob::is_baby(self) {
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
        SoundSource::Hostile
    }

    fn swim_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_SWIM
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_HOSTILE_SPLASH
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_HOGLIN_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert(
            "IsImmuneToZombification",
            i8::from(self.is_immune_to_zombification()),
        );
        nbt.insert(
            "TimeInOverworld",
            self.time_in_overworld.load(Ordering::Relaxed),
        );
        nbt.insert("CannotBeHunted", i8::from(*self.cannot_be_hunted.lock()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        self.set_immune_to_zombification(
            nbt.byte("IsImmuneToZombification")
                .is_some_and(|immune| immune != 0),
        );
        self.time_in_overworld
            .store(nbt.int("TimeInOverworld").unwrap_or(0), Ordering::Relaxed);
        *self.cannot_be_hunted.lock() =
            nbt.byte("CannotBeHunted").is_some_and(|cannot| cannot != 0);
    }
}

impl LivingEntity for HoglinEntity {
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

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_HOGLIN_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_HOGLIN_DEATH)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let was_hurt = self.default_hurt_server(world, source, amount);
        if was_hurt
            && let Some(attacker) = source.causing_entity()
            && attacker.is_living_entity()
        {
            self.was_hurt_by(world, attacker);
        }
        was_hurt
    }

    fn should_drop_experience(&self) -> bool {
        true
    }

    fn base_experience_reward(&self) -> i32 {
        self.xp_reward()
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
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for HoglinEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob.age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob.baby.set(baby);
    }

    /// Vanilla `ageBoundaryReached`: babies give less experience and hit
    /// softer.
    fn age_boundary_changed(&self, baby: bool) {
        let (xp_reward, attack_damage) = if baby {
            (BABY_XP_REWARD, BABY_ATTACK_DAMAGE)
        } else {
            (XP_REWARD, ATTACK_DAMAGE)
        };
        self.set_xp_reward(xp_reward);
        self.attributes()
            .lock()
            .set_base_value(vanilla_attributes::ATTACK_DAMAGE, attack_damage);
        self.refresh_dimensions();
    }

    /// Vanilla `getBreedOffspring`: bred hoglins never despawn.
    fn create_breed_offspring(&self, world: &Arc<World>) -> Option<SharedEntity> {
        let offspring = Self::new(
            &vanilla_entities::HOGLIN,
            next_entity_id(),
            self.position(),
            Arc::downgrade(world),
        );
        offspring.set_persistence_required();
        Some(Arc::new(offspring))
    }
}

impl Animal for HoglinEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::HOGLIN_FOOD)
    }

    fn can_fall_in_love(&self) -> bool {
        !Self::is_pacified(self) && self.in_love_time() <= 0
    }

    fn remove_when_far_away_animal(&self, _dist_sqr: f64) -> bool {
        true
    }
}

impl Mob for HoglinEntity {
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
        if self.is_converting() {
            let time = self.time_in_overworld.fetch_add(1, Ordering::Relaxed) + 1;
            if time > CONVERSION_TIME {
                self.make_sound(Some(&sound_events::ENTITY_HOGLIN_CONVERTED_TO_ZOMBIFIED));
                self.finish_conversion();
            }
        } else {
            self.time_in_overworld.store(0, Ordering::Relaxed);
        }
        Animal::custom_server_ai_step_animal(self);
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        if !target.is_living_entity() {
            return false;
        }
        self.attack_animation_remaining_ticks
            .store(ATTACK_ANIMATION_DURATION, Ordering::Relaxed);
        self.broadcast_entity_event(EntityStatus::StartAttacking);
        self.make_sound(Some(&sound_events::ENTITY_HOGLIN_ATTACK));
        self.on_hit_target(target);
        hurt_and_throw_target(world, entity, target)
    }

    fn mob_can_be_leashed(&self) -> bool {
        true
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        self.sound_for_current_activity()
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let result = Animal::mob_interact_animal(self, player, hand);
        if result.consumes_action() {
            self.set_persistence_required();
        }
        result
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
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().ageable_mob.mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().ageable_mob.mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for HoglinEntity {
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        if self.is_pos_near_nearest_repellent(pos) {
            -1.0
        } else if level.get_block_state(pos.below()).get_block() == &vanilla_blocks::CRIMSON_NYLIUM
        {
            10.0
        } else {
            0.0
        }
    }
}

impl Enemy for HoglinEntity {}
