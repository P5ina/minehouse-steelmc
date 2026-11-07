//! Vanilla goat (`Goat` and `GoatAi`): long jumps, rams and drops horns.

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::InstrumentComponent;
use steel_registry::data_components::vanilla_components::INSTRUMENT;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::mob_effect::MobEffectRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::GoatEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::vanilla_instrument_tags::InstrumentTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, RegistryHolder, TaggedRegistryExt, sound_events, vanilla_attributes,
    vanilla_damage_types, vanilla_entities, vanilla_items, vanilla_mob_effects,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};
use uuid::Uuid;

use crate::behavior::{ITEM_BEHAVIORS, InteractionResult};
use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, CountDownCooldownTicks, DoNothing, FollowTemptation,
    LongJumpMidJump, LongJumpToRandomPos, LookAtTargetSink, MoveToTargetSink,
    PrepareRamNearestTarget, Swim, UniformInt, baby_follow_adult, random_stroll, run_one,
    set_entity_look_target_sometimes, set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, LONG_JUMP_COOLDOWN_TICKS, NEAREST_VISIBLE_ADULT, RAM_COOLDOWN_TICKS, RAM_TARGET,
    TEMPTATION_COOLDOWN_TICKS, TEMPTING_PLAYER, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::sensors::{
    AdultSensor, HurtBySensor, NearestLivingEntitySensor, TemptingSensor,
};
use crate::entity::ai::brain::{
    ActivityData, BehaviorControl, Brain, Memories, MemoryStatus, Sensor,
};
use crate::entity::ai::path::PathType;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Goat.LONG_JUMPING_DIMENSION_SCALE_FACTOR`.
const LONG_JUMPING_DIMENSION_SCALE_FACTOR: f32 = 0.7;
const BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.531_25, 0.0)];
/// Vanilla `Goat.BABY_DIMENSIONS`.
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.45,
    0.65,
    0.593_75,
    EntityAttachments::new(&BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
const BABY_SCALE: f32 = 0.55;
const ADULT_ATTACK_DAMAGE: f64 = 2.0;
const BABY_ATTACK_DAMAGE: f64 = 1.0;
/// Vanilla `Goat.GOAT_FALL_DAMAGE_REDUCTION`.
const GOAT_FALL_DAMAGE_REDUCTION: i32 = 10;
/// Vanilla `Goat.GOAT_SCREAMING_CHANCE`.
const GOAT_SCREAMING_CHANCE: f64 = 0.02;
/// Vanilla `Goat.UNIHORN_CHANCE`.
const UNIHORN_CHANCE: f32 = 0.1;
const MAX_HEAD_Y_ROT: f32 = 15.0;

/// Vanilla `GoatAi.RAM_PREPARE_TIME`.
const RAM_PREPARE_TIME: i64 = 20;
/// Vanilla `GoatAi.RAM_MAX_DISTANCE`.
const RAM_MAX_DISTANCE: i32 = 7;
/// Vanilla `GoatAi.RAM_MIN_DISTANCE`.
const RAM_MIN_DISTANCE: i32 = 4;
const ADULT_FOLLOW_RANGE: UniformInt = UniformInt::of(5, 16);
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_PREPARING_TO_RAM: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_RAMMING: f32 = 3.0;
const TIME_BETWEEN_LONG_JUMPS: UniformInt = UniformInt::of(600, 1200);
/// Vanilla `GoatAi.MAX_LONG_JUMP_HEIGHT` and `MAX_LONG_JUMP_WIDTH`.
const MAX_LONG_JUMP_SIZE: i32 = 5;
/// Vanilla `GoatAi.MAX_JUMP_VELOCITY_MULTIPLIER`.
const MAX_JUMP_VELOCITY_MULTIPLIER: f32 = 3.571_428_8;
const TIME_BETWEEN_RAMS: UniformInt = UniformInt::of(600, 6000);
const TIME_BETWEEN_RAMS_SCREAMER: UniformInt = UniformInt::of(100, 300);
/// Vanilla `GoatAi.ADULT_RAM_KNOCKBACK_FORCE`.
const ADULT_RAM_KNOCKBACK_FORCE: f64 = 2.5;
/// Vanilla `GoatAi.BABY_RAM_KNOCKBACK_FORCE`.
const BABY_RAM_KNOCKBACK_FORCE: f64 = 1.0;

/// Vanilla `GoatAi.RAM_TARGET_CONDITIONS`.
fn ram_target_conditions() -> TargetingConditions {
    TargetingConditions::for_combat().selector(|target, world| {
        target.entity_type() != &vanilla_entities::GOAT
            && (world.get_game_rule(&MOB_GRIEFING)
                || target.entity_type() != &vanilla_entities::ARMOR_STAND)
            && world
                .world_border_snapshot()
                .is_within_bounds(target.bounding_box())
    })
}

/// Vanilla `UUID.hashCode`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "vanilla folds the UUID bits into an int"
)]
const fn java_uuid_hash_code(uuid: Uuid) -> i32 {
    let (most, least) = uuid.as_u64_pair();
    let hilo = most ^ least;
    ((hilo >> 32) as i32) ^ (hilo as i32)
}

#[entity_behavior(class = "Goat", spawn_rule)]
/// Vanilla goat.
pub struct GoatEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<GoatEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `GoatEntity`.
unsafe impl DowncastType for GoatEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/goat");
}

impl GoatEntity {
    /// Vanilla `Goat.checkGoatSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_goat_spawn_rules;

    fn check_goat_spawn_rules(
        _entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        _spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::GOATS_SPAWNABLE_ON)
            && <Self as Animal>::is_bright_enough_to_spawn(level, pos)
    }

    /// Creates a new goat at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a goat from persisted base entity state.
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
        let ageable_base = AgeableMobBase::new();
        let animal_base = AnimalBase::new();
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        mob_base.navigation().lock().set_can_float(true);
        let mut entity_data = GoatEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let goat = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], Self::sensors(), Self::activities(), None),
        };
        goat.set_pathfinding_malus(PathType::PowderSnow, -1.0);
        goat.set_pathfinding_malus(PathType::OnTopOfPowderSnow, -1.0);
        goat
    }

    fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
        vec![
            Box::new(NearestLivingEntitySensor),
            Box::new(AdultSensor),
            Box::new(HurtBySensor),
            Box::new(TemptingSensor::<Self>::for_animal()),
        ]
    }

    /// Vanilla `GoatAi.initMemories`.
    fn init_memories(&self) {
        let mut memories = self.brain.memories().lock();
        memories.set(
            &LONG_JUMP_COOLDOWN_TICKS,
            Some(TIME_BETWEEN_LONG_JUMPS.sample()),
        );
        memories.set(&RAM_COOLDOWN_TICKS, Some(TIME_BETWEEN_RAMS.sample()));
    }

    /// Vanilla `GoatAi.getActivities`.
    fn activities() -> Vec<ActivityData<Self>> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::long_jump_activity(),
            Self::ram_activity(),
        ]
    }

    fn core_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::CORE,
            0,
            vec![
                Box::new(TimedBehavior::new(Swim::new(0.8))),
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
                Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                    &RAM_COOLDOWN_TICKS,
                ))),
            ],
        )
    }

    fn idle_activity() -> ActivityData<Self> {
        let stroll: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
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
                    0,
                    Box::new(TimedBehavior::new(AnimalMakeLove::new(
                        &vanilla_entities::GOAT,
                    ))),
                ),
                (
                    1,
                    Box::new(TimedBehavior::new(FollowTemptation::new(|_| {
                        SPEED_MULTIPLIER_WHEN_TEMPTED
                    }))),
                ),
                (
                    2,
                    Box::new(baby_follow_adult(
                        ADULT_FOLLOW_RANGE,
                        |_| SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                        &NEAREST_VISIBLE_ADULT,
                        false,
                    )),
                ),
                (3, Box::new(run_one(Vec::new(), stroll))),
            ],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&RAM_COOLDOWN_TICKS, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&LONG_JUMP_COOLDOWN_TICKS, MemoryStatus::ValuePresent),
        ])
    }

    fn long_jump_activity() -> ActivityData<Self> {
        ActivityData::new(
            Activity::LONG_JUMP,
            vec![
                (
                    0,
                    Box::new(TimedBehavior::new(LongJumpMidJump::new(
                        TIME_BETWEEN_LONG_JUMPS,
                        &sound_events::ENTITY_GOAT_STEP,
                    ))),
                ),
                (
                    1,
                    Box::new(TimedBehavior::new(LongJumpToRandomPos::new(
                        TIME_BETWEEN_LONG_JUMPS,
                        MAX_LONG_JUMP_SIZE,
                        MAX_LONG_JUMP_SIZE,
                        MAX_JUMP_VELOCITY_MULTIPLIER,
                        |goat: &Self| {
                            if goat.is_screaming_goat() {
                                &sound_events::ENTITY_GOAT_SCREAMING_LONG_JUMP
                            } else {
                                &sound_events::ENTITY_GOAT_LONG_JUMP
                            }
                        },
                    ))),
                ),
            ],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&TEMPTING_PLAYER, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&LONG_JUMP_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
        ])
    }

    fn ram_activity() -> ActivityData<Self> {
        ActivityData::new(
            Activity::RAM,
            vec![
                (0, Box::new(TimedBehavior::new(RamTarget::new()))),
                (
                    1,
                    Box::new(TimedBehavior::new(PrepareRamNearestTarget::new(
                        |goat: &Self| goat.time_between_rams().min_inclusive,
                        RAM_MIN_DISTANCE,
                        RAM_MAX_DISTANCE,
                        SPEED_MULTIPLIER_WHEN_PREPARING_TO_RAM,
                        ram_target_conditions(),
                        RAM_PREPARE_TIME,
                        |goat: &Self| {
                            if goat.is_screaming_goat() {
                                &sound_events::ENTITY_GOAT_SCREAMING_PREPARE_RAM
                            } else {
                                &sound_events::ENTITY_GOAT_PREPARE_RAM
                            }
                        },
                    ))),
                ),
            ],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&TEMPTING_PLAYER, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
        ])
    }

    /// Vanilla `GoatAi`'s `goat.isScreamingGoat() ? TIME_BETWEEN_RAMS_SCREAMER : TIME_BETWEEN_RAMS`.
    fn time_between_rams(&self) -> UniformInt {
        if self.is_screaming_goat() {
            TIME_BETWEEN_RAMS_SCREAMER
        } else {
            TIME_BETWEEN_RAMS
        }
    }

    /// Vanilla `Goat.isScreamingGoat`.
    #[must_use]
    pub fn is_screaming_goat(&self) -> bool {
        *self.entity_data.lock().is_screaming_goat.get()
    }

    /// Vanilla `Goat.setScreamingGoat`.
    pub fn set_screaming_goat(&self, screaming: bool) {
        self.entity_data.lock().is_screaming_goat.set(screaming);
    }

    /// Vanilla `Goat.hasLeftHorn`.
    #[must_use]
    pub fn has_left_horn(&self) -> bool {
        *self.entity_data.lock().has_left_horn.get()
    }

    /// Vanilla `Goat.hasRightHorn`.
    #[must_use]
    pub fn has_right_horn(&self) -> bool {
        *self.entity_data.lock().has_right_horn.get()
    }

    fn set_left_horn(&self, has_horn: bool) {
        self.entity_data.lock().has_left_horn.set(has_horn);
    }

    fn set_right_horn(&self, has_horn: bool) {
        self.entity_data.lock().has_right_horn.set(has_horn);
    }

    /// Vanilla `Goat.createHorn`: the instrument is picked from the goat's
    /// UUID, so a goat always drops the same horn.
    #[must_use]
    pub fn create_horn(&self) -> ItemStack {
        let tag = if self.is_screaming_goat() {
            InstrumentTag::SCREAMING_GOAT_HORNS
        } else {
            InstrumentTag::REGULAR_GOAT_HORNS
        };
        let mut horn = ItemStack::new(&vanilla_items::GOAT_HORN);
        let instruments = REGISTRY.instruments.get_tag(&tag).unwrap_or_default();
        if instruments.is_empty() {
            return horn;
        }
        let seed = i64::from(java_uuid_hash_code(self.uuid()));
        let mut random = LegacyRandom::from_seed(seed.cast_unsigned());
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "instrument tags hold a handful of entries"
        )]
        let bound = instruments.len() as i32;
        let index = random.next_i32_bounded(bound).unsigned_abs() as usize;
        if let Some(instrument) = instruments.get(index) {
            horn.set(
                INSTRUMENT,
                InstrumentComponent::new(RegistryHolder::reference(instrument)),
            );
        }
        horn
    }

    /// Vanilla `Goat.dropHorn`.
    pub fn drop_horn(&self) -> bool {
        if AgeableMob::is_baby(self) {
            return false;
        }
        let has_left = self.has_left_horn();
        let has_right = self.has_right_horn();
        if !has_left && !has_right {
            return false;
        }
        let drop_left = if !has_left {
            false
        } else if !has_right {
            true
        } else {
            rand::random::<bool>()
        };
        if drop_left {
            self.set_left_horn(false);
        } else {
            self.set_right_horn(false);
        }
        let Some(world) = self.level() else {
            return true;
        };
        let velocity = DVec3::new(
            f64::from(random_between(-0.2, 0.2)),
            f64::from(random_between(0.3, 0.7)),
            f64::from(random_between(-0.2, 0.2)),
        );
        let _ = world.spawn_item_with_velocity(self.position(), self.create_horn(), velocity);
        true
    }

    fn milking_sound(&self) -> SoundEventRef {
        if self.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_MILK
        } else {
            &sound_events::ENTITY_GOAT_MILK
        }
    }

    /// Vanilla `Goat.mobInteract`'s bucket branch.
    fn milk(&self, player: &Player, hand: InteractionHand) {
        player.play_sound(self.milking_sound(), 1.0, 1.0);
        let overflow = player.inventory.lock().apply_filled_result(
            hand,
            ITEM_BEHAVIORS.default_instance(&vanilla_items::MILK_BUCKET),
            player.has_infinite_materials(),
            true,
        );
        if !overflow.is_empty() {
            let _ = player.drop_item(overflow, false, Prediction::Predicted);
        }
    }

    /// Vanilla `Goat.ageBoundaryReached`.
    fn update_attack_damage(&self) {
        let damage = if AgeableMob::is_baby(self) {
            BABY_ATTACK_DAMAGE
        } else {
            ADULT_ATTACK_DAMAGE
        };
        self.attributes()
            .lock()
            .set_base_value(vanilla_attributes::ATTACK_DAMAGE, damage);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

/// Vanilla `Mth.randomBetween`.
fn random_between(min: f32, max: f32) -> f32 {
    min + rand::random::<f32>() * (max - min)
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

const RAM_TARGET_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&RAM_TARGET, MemoryStatus::ValuePresent),
];
/// Vanilla `RamTarget.TIME_OUT_DURATION`.
const RAM_TIME_OUT_DURATION: i32 = 200;
/// Vanilla `RamTarget.RAM_SPEED_FORCE_FACTOR`.
const RAM_SPEED_FORCE_FACTOR: f32 = 1.65;

/// Vanilla `RamTarget`: charges at `RAM_TARGET`, knocking back whatever it
/// hits; a horn-snapping block breaks off a horn instead.
struct RamTarget {
    ram_targeting: TargetingConditions,
    ram_direction: DVec3,
}

impl RamTarget {
    fn new() -> Self {
        Self {
            ram_targeting: ram_target_conditions(),
            ram_direction: DVec3::ZERO,
        }
    }

    fn impact_sound(goat: &GoatEntity) -> SoundEventRef {
        if goat.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_RAM_IMPACT
        } else {
            &sound_events::ENTITY_GOAT_RAM_IMPACT
        }
    }

    fn knockback_force(goat: &GoatEntity) -> f64 {
        if AgeableMob::is_baby(goat) {
            BABY_RAM_KNOCKBACK_FORCE
        } else {
            ADULT_RAM_KNOCKBACK_FORCE
        }
    }

    fn effect_level(goat: &GoatEntity, effect: MobEffectRef) -> i32 {
        goat.mob_effect(effect)
            .map_or(0, |instance| instance.amplifier() + 1)
    }

    /// Vanilla `hasRammedHornBreakingBlock`.
    fn has_rammed_horn_breaking_block(world: &World, goat: &GoatEntity) -> bool {
        let velocity = goat.velocity();
        let horizontal = vanilla_normalize(DVec3::new(velocity.x, 0.0, velocity.z));
        let facing = goat.position() + horizontal;
        let pos = BlockPos::containing(facing.x, facing.y, facing.z);
        let snaps = |pos: BlockPos| {
            world
                .get_block_state(pos)
                .get_block()
                .has_tag(&BlockTag::SNAPS_GOAT_HORN)
        };
        snaps(pos) || snaps(pos.above())
    }

    /// Vanilla `finishRam`.
    fn finish_ram(goat: &GoatEntity) {
        goat.broadcast_entity_event(EntityStatus::EndRam);
        let mut memories = goat.brain.memories().lock();
        memories.set(&RAM_COOLDOWN_TICKS, Some(goat.time_between_rams().sample()));
        memories.erase(&RAM_TARGET);
    }

    /// Vanilla `RamTarget.tick`'s hit on `target`.
    fn ram(&self, world: &Arc<World>, goat: &GoatEntity, target: &SharedEntity) {
        let Some(attacker) = world.get_entity_by_id(goat.id()) else {
            return;
        };
        let damage_source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK_NO_AGGRO)
            .with_causing_entity(Arc::clone(&attacker))
            .with_direct_entity(Arc::clone(&attacker));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vanilla casts the attack damage to float"
        )]
        let damage = goat
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as f32;
        if target.hurt(world, &damage_source, damage) {
            let mut weapon = ItemStack::empty();
            goat.with_equipment_slot(EquipmentSlot::MainHand, &mut |item_stack| {
                weapon = item_stack.copy_with_count(item_stack.count());
            });
            let context = EnchantmentPostAttackContext::new(target, &damage_source);
            enchantment_helper::do_post_attack_effects_with_item_source(
                world,
                target.as_ref(),
                &weapon,
                &context,
            );
        }

        let speed_level = Self::effect_level(goat, vanilla_mob_effects::SPEED);
        let slowness_level = Self::effect_level(goat, vanilla_mob_effects::SLOWNESS);
        #[expect(
            clippy::cast_precision_loss,
            reason = "effect levels are small amplifiers"
        )]
        let speed_boost_power = 0.25 * (speed_level - slowness_level) as f32;
        let speed_factor =
            (goat.get_speed() * RAM_SPEED_FORCE_FACTOR).clamp(0.2, 3.0) + speed_boost_power;
        // Vanilla `applyItemBlocking` halves the knockback of a shield block;
        // Steel's damage pipeline has no item blocking yet, so nothing is
        // ever blocked here.
        let blocking_factor = 1.0_f32;
        if let Some(target) = target.as_living_entity() {
            target.knockback(
                f64::from(blocking_factor * speed_factor) * Self::knockback_force(goat),
                self.ram_direction.x,
                self.ram_direction.z,
            );
        }
        Self::finish_ram(goat);
        world.play_sound_at(
            Self::impact_sound(goat),
            SoundSource::Neutral,
            goat.position(),
            1.0,
            1.0,
            None,
        );
    }
}

impl Behavior<GoatEntity> for RamTarget {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &RAM_TARGET_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (RAM_TIME_OUT_DURATION, RAM_TIME_OUT_DURATION)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &GoatEntity) -> bool {
        body.brain.memories().lock().has_value(&RAM_TARGET)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &GoatEntity, _time: i64) -> bool {
        body.brain.memories().lock().has_value(&RAM_TARGET)
    }

    fn start(&mut self, _world: &Arc<World>, body: &GoatEntity, _time: i64) {
        let cur_pos = body.block_position();
        let mut memories = body.brain.memories().lock();
        let Some(ram_target) = memories.get(&RAM_TARGET) else {
            return;
        };
        self.ram_direction = vanilla_normalize(DVec3::new(
            f64::from(cur_pos.x()) - ram_target.x,
            0.0,
            f64::from(cur_pos.z()) - ram_target.z,
        ));
        memories.set(
            &WALK_TARGET,
            Some(WalkTarget::to_point(
                ram_target,
                SPEED_MULTIPLIER_WHEN_RAMMING,
                0,
            )),
        );
    }

    fn tick(&mut self, world: &Arc<World>, body: &GoatEntity, _time: i64) {
        let bounds = body.bounding_box();
        let nearby = world.get_entities_in_aabb_matching(&bounds, |entity| {
            entity
                .as_living_entity()
                .is_some_and(|target| self.ram_targeting.test(world, Some(body), target))
        });
        if let Some(target) = nearby.first() {
            self.ram(world, body, target);
        } else if Self::has_rammed_horn_breaking_block(world, body) {
            world.play_sound_at(
                Self::impact_sound(body),
                SoundSource::Neutral,
                body.position(),
                1.0,
                1.0,
                None,
            );
            if body.drop_horn() {
                world.play_sound_at(
                    &sound_events::ENTITY_GOAT_HORN_BREAK,
                    SoundSource::Neutral,
                    body.position(),
                    1.0,
                    1.0,
                    None,
                );
            }
            Self::finish_ram(body);
        } else {
            let lost_or_reached_target = {
                let memories = body.brain.memories().lock();
                match (memories.get(&WALK_TARGET), memories.get(&RAM_TARGET)) {
                    (Some(walk_target), Some(ram_target)) => {
                        walk_target
                            .target
                            .current_position()
                            .distance_squared(ram_target)
                            < 0.25 * 0.25
                    }
                    _ => true,
                }
            };
            if lost_or_reached_target {
                Self::finish_ram(body);
            }
        }
    }
}

impl DefaultEntityPush for GoatEntity {}

impl Entity for GoatEntity {
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
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    /// Vanilla `Goat.getDefaultDimensions`, scaled by `getScale`.
    fn dimensions_for_pose(&self, pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        let dimensions = if AgeableMob::is_baby(self) {
            BABY_DIMENSIONS
        } else {
            self.entity_type.dimensions
        };
        let dimensions = if pose == EntityPose::LongJumping {
            dimensions.scale(LONG_JUMPING_DIMENSION_SCALE_FACTOR)
        } else {
            dimensions
        };
        if self.entity_type.fixed {
            dimensions
        } else {
            dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_GOAT_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert("IsScreamingGoat", i8::from(self.is_screaming_goat()));
        nbt.insert("HasLeftHorn", i8::from(self.has_left_horn()));
        nbt.insert("HasRightHorn", i8::from(self.has_right_horn()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        self.set_screaming_goat(nbt.byte("IsScreamingGoat").is_some_and(|value| value != 0));
        self.set_left_horn(nbt.byte("HasLeftHorn").is_none_or(|value| value != 0));
        self.set_right_horn(nbt.byte("HasRightHorn").is_none_or(|value| value != 0));
    }
}

impl LivingEntity for GoatEntity {
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
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn get_age_scale(&self) -> f32 {
        if AgeableMob::is_baby(self) {
            BABY_SCALE
        } else {
            1.0
        }
    }

    /// Vanilla `Goat.setYHeadRot`: the head turns at most 15 degrees from
    /// the body.
    fn set_y_head_rot(&self, y_head_rot: f32) {
        let y_body_rot = self.y_body_rot();
        let delta_from_body = steel_math::wrap_degrees(y_head_rot - y_body_rot);
        let clamped = delta_from_body.clamp(-MAX_HEAD_Y_ROT, MAX_HEAD_Y_ROT);
        self.living_base().set_y_head_rot(y_body_rot + clamped);
    }

    /// Vanilla `Goat.calculateFallDamage`.
    fn calculate_fall_damage(&self, fall_distance: f64, damage_modifier: f32) -> i32 {
        let attributes = self.attributes().lock();
        let safe_fall_distance = attributes
            .get_value(vanilla_attributes::SAFE_FALL_DISTANCE)
            .unwrap_or(vanilla_attributes::SAFE_FALL_DISTANCE.default_value);
        let fall_damage_multiplier = attributes
            .get_value(vanilla_attributes::FALL_DAMAGE_MULTIPLIER)
            .unwrap_or(vanilla_attributes::FALL_DAMAGE_MULTIPLIER.default_value);
        drop(attributes);
        LivingEntityBase::calculate_fall_damage(
            fall_distance,
            damage_modifier,
            safe_fall_distance,
            fall_damage_multiplier,
        ) - GOAT_FALL_DAMAGE_REDUCTION
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_HURT
        } else {
            &sound_events::ENTITY_GOAT_HURT
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_DEATH
        } else {
            &sound_events::ENTITY_GOAT_DEATH
        })
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for GoatEntity {
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
        self.update_attack_damage();
        self.refresh_dimensions();
    }

    /// Vanilla `Goat.getBreedOffspring`.
    fn initialize_breed_offspring(&self, partner: &dyn AgeableMob, offspring: &dyn AgeableMob) {
        let Some(offspring) = offspring.downcast_ref::<Self>() else {
            return;
        };
        offspring.init_memories();
        let selected_parent_screams = if rand::random::<bool>() {
            self.is_screaming_goat()
        } else {
            partner
                .downcast_ref::<Self>()
                .is_some_and(Self::is_screaming_goat)
        };
        let baby_is_screaming =
            selected_parent_screams || rand::random::<f64>() < GOAT_SCREAMING_CHANCE;
        offspring.set_screaming_goat(baby_is_screaming);
    }
}

impl Animal for GoatEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::GOAT_FOOD)
    }

    fn play_eating_sound(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let sound = if self.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_EAT
        } else {
            &sound_events::ENTITY_GOAT_EAT
        };
        world.play_sound_at(
            sound,
            SoundSource::Neutral,
            self.position(),
            1.0,
            random_between(0.8, 1.2),
            None,
        );
    }
}

impl Mob for GoatEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            // Vanilla `GoatAi.updateActivity`.
            self.brain.set_active_activity_to_first_valid(&[
                Activity::RAM,
                Activity::LONG_JUMP,
                Activity::IDLE,
            ]);
        }
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_screaming_goat() {
            &sound_events::ENTITY_GOAT_SCREAMING_AMBIENT
        } else {
            &sound_events::ENTITY_GOAT_AMBIENT
        })
    }

    fn max_head_y_rot(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let is_bucket = player
            .inventory
            .lock()
            .get_item_in_hand(hand)
            .is(&vanilla_items::BUCKET);
        if is_bucket && !AgeableMob::is_baby(self) {
            self.milk(player, hand);
            return InteractionResult::Success;
        }
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        let result = Animal::mob_interact_animal(self, player, hand);
        if result.consumes_action() && self.is_food(&item_stack) {
            self.play_eating_sound();
        }
        result
    }

    /// Vanilla `Goat.finalizeSpawn`.
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.init_memories();
        self.set_screaming_goat(rand::random::<f64>() < GOAT_SCREAMING_CHANCE);
        self.update_attack_damage();
        if !AgeableMob::is_baby(self) && rand::random::<f32>() < UNIHORN_CHANCE {
            if rand::random::<bool>() {
                self.set_left_horn(false);
            } else {
                self.set_right_horn(false);
            }
        }
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for GoatEntity {}

#[cfg(test)]
mod tests;
