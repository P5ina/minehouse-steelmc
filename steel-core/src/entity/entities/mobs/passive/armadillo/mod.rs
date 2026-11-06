//! Vanilla armadillo (`Armadillo` and `ArmadilloAi`): the first brain mob.

use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_data::ArmadilloState;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::{LootContext, LootTableRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::ArmadilloEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt, sound_events, vanilla_entities, vanilla_game_events,
    vanilla_items, vanilla_loot_tables,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, CountDownCooldownTicks, DoNothing, FollowTemptation,
    LookAtTargetSink, MoveToTargetSink, RandomLookAround, Swim, UniformInt, baby_follow_adult,
    random_stroll, run_one, set_entity_look_target_sometimes, set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    DANGER_DETECTED_RECENTLY, GAZE_COOLDOWN_TICKS, IS_PANICKING, NEAREST_VISIBLE_ADULT,
    TEMPTATION_COOLDOWN_TICKS, WALK_TARGET,
};
use crate::entity::ai::brain::sensors::{
    AdultSensor, HurtBySensor, MobSensor, NearestLivingEntitySensor, TemptingSensor,
};
use crate::entity::ai::brain::{
    ActivityData, BehaviorControl, Brain, Memories, MemoryStatus, Sensor,
};
use crate::entity::damage::DamageSource;
use crate::entity::leash::Leashable as _;
use crate::entity::living_entity::shearing_loot_items_with_rng;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnRule, entity_loot_ref,
    living_loot_equipment, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

const BABY_SCALE: f32 = 0.6;
const BABY_EYE_HEIGHT: f32 = 0.218_75;
const MAX_HEAD_Y_ROT: f32 = 32.0;
const SCARE_CHECK_INTERVAL: i32 = 80;
const SCARE_DISTANCE_HORIZONTAL: f64 = 7.0;
const SCARE_DISTANCE_VERTICAL: f64 = 2.0;
const SCUTE_DROP_TIME: i32 = 20 * 60 * 5;
const BRUSH_DAMAGE: i32 = 16;

const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_MAKING_LOVE: f32 = 1.0;
const DEFAULT_CLOSE_ENOUGH_DIST: f64 = 2.0;
const BABY_CLOSE_ENOUGH_DIST: f64 = 1.0;
const ADULT_FOLLOW_RANGE: UniformInt = UniformInt::of(5, 16);

/// Ticks a state's client animation lasts (vanilla `ArmadilloState.animationDuration`).
const fn animation_duration(state: ArmadilloState) -> i64 {
    match state {
        ArmadilloState::Idle => 0,
        ArmadilloState::Rolling => 10,
        ArmadilloState::Scared => 50,
        ArmadilloState::Unrolling => 30,
    }
}

/// Vanilla `ArmadilloState.isThreatened`.
const fn is_threatened(state: ArmadilloState) -> bool {
    !matches!(state, ArmadilloState::Idle)
}

/// Vanilla `ArmadilloState.shouldHideInShell`.
const fn should_hide_in_shell(state: ArmadilloState, ticks_in_state: i64) -> bool {
    match state {
        ArmadilloState::Idle => false,
        ArmadilloState::Rolling => ticks_in_state > 5,
        ArmadilloState::Scared => true,
        ArmadilloState::Unrolling => ticks_in_state < 26,
    }
}

const fn state_name(state: ArmadilloState) -> &'static str {
    match state {
        ArmadilloState::Idle => "idle",
        ArmadilloState::Rolling => "rolling",
        ArmadilloState::Scared => "scared",
        ArmadilloState::Unrolling => "unrolling",
    }
}

fn state_by_name(name: &str) -> Option<ArmadilloState> {
    match name {
        "idle" => Some(ArmadilloState::Idle),
        "rolling" => Some(ArmadilloState::Rolling),
        "scared" => Some(ArmadilloState::Scared),
        "unrolling" => Some(ArmadilloState::Unrolling),
        _ => None,
    }
}

#[entity_behavior(class = "Armadillo", spawn_rule)]
/// Vanilla armadillo: rolls into its shell when threatened and sheds scutes.
pub struct ArmadilloEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<ArmadilloEntityData>,
    brain: Brain<Self>,
    in_state_ticks: AtomicI64,
    scute_time: AtomicI32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ArmadilloEntity`.
unsafe impl DowncastType for ArmadilloEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/armadillo");
}

impl ArmadilloEntity {
    /// Vanilla `Armadillo.checkArmadilloSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_armadillo_spawn_rules;

    fn check_armadillo_spawn_rules(
        _entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        _spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::ARMADILLO_SPAWNABLE_ON)
            && <Self as Animal>::is_bright_enough_to_spawn(level, pos)
    }

    /// Creates a new armadillo at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs an armadillo from persisted base entity state.
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
        let mut entity_data = ArmadilloEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], Self::sensors(), Self::activities(), None),
            in_state_ticks: AtomicI64::new(0),
            scute_time: AtomicI32::new(Self::pick_next_scute_drop_time()),
        }
    }

    fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
        vec![
            Box::new(NearestLivingEntitySensor),
            Box::new(HurtBySensor),
            Box::new(TemptingSensor::<Self>::for_animal()),
            Box::new(AdultSensor),
            // Vanilla `SensorType.ARMADILLO_SCARE_DETECTED`.
            Box::new(MobSensor::new(
                5,
                |armadillo: &Self, entity: &SharedEntity| armadillo.is_scared_by(entity),
                Self::can_stay_rolled_up,
                &DANGER_DETECTED_RECENTLY,
                i64::from(SCARE_CHECK_INTERVAL),
            )),
        ]
    }

    /// Vanilla `ArmadilloAi.getActivities`.
    fn activities() -> Vec<ActivityData<Self>> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::scared_activity(),
        ]
    }

    fn core_activity() -> ActivityData<Self> {
        let rolling_out = OneShot::new(
            vec![MemoryRequirement::new(
                &DANGER_DETECTED_RECENTLY,
                MemoryStatus::Registered,
            )],
            |_world: &Arc<World>, body: &Self, _time: i64| {
                let danger = body
                    .brain
                    .memories()
                    .lock()
                    .has_value(&DANGER_DETECTED_RECENTLY);
                if danger || !body.is_scared() {
                    return false;
                }
                body.roll_out();
                true
            },
        );
        ActivityData::with_first_priority(
            Activity::CORE,
            0,
            vec![
                Box::new(TimedBehavior::new(Swim::new(0.8))),
                Box::new(TimedBehavior::new(ArmadilloPanic(
                    AnimalPanic::with_damage_types(SPEED_MULTIPLIER_WHEN_PANICKING, |_| {
                        DamageTypeTag::PANIC_ENVIRONMENTAL_CAUSES
                    }),
                ))),
                Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
                Box::new(TimedBehavior::new(ArmadilloMoveToTarget(
                    MoveToTargetSink::new(),
                ))),
                Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                    &TEMPTATION_COOLDOWN_TICKS,
                ))),
                Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                    &GAZE_COOLDOWN_TICKS,
                ))),
                Box::new(rolling_out),
            ],
        )
    }

    fn idle_activity() -> ActivityData<Self> {
        let follow: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
            (
                Box::new(TimedBehavior::new(FollowTemptation::with_distance(
                    |_| SPEED_MULTIPLIER_WHEN_TEMPTED,
                    |armadillo: &Self| {
                        if AgeableMob::is_baby(armadillo) {
                            BABY_CLOSE_ENOUGH_DIST
                        } else {
                            DEFAULT_CLOSE_ENOUGH_DIST
                        }
                    },
                    false,
                ))),
                1,
            ),
            (
                Box::new(baby_follow_adult(
                    ADULT_FOLLOW_RANGE,
                    |_| SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT,
                    &NEAREST_VISIBLE_ADULT,
                    false,
                )),
                1,
            ),
        ];
        let stroll: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
            (
                Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true)),
                1,
            ),
            (
                Box::new(set_walk_target_from_look_target(
                    |_| true,
                    |_| SPEED_MULTIPLIER_WHEN_IDLING,
                    3,
                )),
                1,
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
                    1,
                    Box::new(TimedBehavior::new(AnimalMakeLove::with_speed(
                        &vanilla_entities::ARMADILLO,
                        SPEED_MULTIPLIER_WHEN_MAKING_LOVE,
                        1,
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

    fn scared_activity() -> ActivityData<Self> {
        ActivityData::new(
            Activity::PANIC,
            vec![(0, Box::new(TimedBehavior::new(ArmadilloBallUp::default())))],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&DANGER_DETECTED_RECENTLY, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
        ])
    }

    fn pick_next_scute_drop_time() -> i32 {
        rand::random_range(0..SCUTE_DROP_TIME) + SCUTE_DROP_TIME
    }

    /// The armadillo's synced state.
    #[must_use]
    pub fn state(&self) -> ArmadilloState {
        *self.entity_data.lock().armadillo_state.get()
    }

    /// Vanilla `Armadillo.switchToState`; a changed state restarts the
    /// in-state tick counter (vanilla `onSyncedDataUpdated`).
    pub fn switch_to_state(&self, state: ArmadilloState) {
        let changed = {
            let mut data = self.entity_data.lock();
            let changed = *data.armadillo_state.get() != state;
            data.armadillo_state.set(state);
            changed
        };
        if changed {
            self.in_state_ticks.store(0, Ordering::Relaxed);
        }
    }

    /// Vanilla `Armadillo.isScared`: in any state but idle.
    #[must_use]
    pub fn is_scared(&self) -> bool {
        self.state() != ArmadilloState::Idle
    }

    /// Vanilla `Armadillo.shouldHideInShell`.
    #[must_use]
    pub fn should_hide_in_shell(&self) -> bool {
        should_hide_in_shell(self.state(), self.in_state_ticks.load(Ordering::Relaxed))
    }

    /// Vanilla `Armadillo.shouldSwitchToScaredState`.
    #[must_use]
    pub fn should_switch_to_scared_state(&self) -> bool {
        self.state() == ArmadilloState::Rolling
            && self.in_state_ticks.load(Ordering::Relaxed)
                > animation_duration(ArmadilloState::Rolling)
    }

    /// Vanilla `Armadillo.isScaredBy`.
    #[must_use]
    pub fn is_scared_by(&self, entity: &SharedEntity) -> bool {
        if !self
            .bounding_box()
            .inflate_xyz(
                SCARE_DISTANCE_HORIZONTAL,
                SCARE_DISTANCE_VERTICAL,
                SCARE_DISTANCE_HORIZONTAL,
            )
            .intersects(entity.bounding_box())
        {
            return false;
        }
        if REGISTRY
            .entity_types
            .is_in_tag(entity.entity_type(), &EntityTypeTag::UNDEAD)
        {
            return true;
        }
        if self
            .last_hurt_by_mob()
            .is_some_and(|attacker| attacker.id() == entity.id())
        {
            return true;
        }
        let Some(player) = entity.as_player() else {
            return false;
        };
        !player.is_spectator() && (player.is_sprinting() || player.is_passenger())
    }

    /// Vanilla `Armadillo.canStayRolledUp`.
    #[must_use]
    pub fn can_stay_rolled_up(&self) -> bool {
        !self.is_panicking()
            && !self.is_in_liquid()
            && !self.is_leashed()
            && !self.is_passenger()
            && !self.is_vehicle()
    }

    fn is_in_liquid(&self) -> bool {
        self.is_in_water() || self.is_in_lava()
    }

    /// Vanilla `Armadillo.rollUp`.
    pub fn roll_up(&self) {
        if self.is_scared() {
            return;
        }
        self.stop_in_place();
        self.reset_love();
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.make_sound(Some(&sound_events::ENTITY_ARMADILLO_ROLL));
        self.switch_to_state(ArmadilloState::Rolling);
    }

    /// Vanilla `Armadillo.rollOut`.
    pub fn roll_out(&self) {
        if !self.is_scared() {
            return;
        }
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.make_sound(Some(&sound_events::ENTITY_ARMADILLO_UNROLL_FINISH));
        self.switch_to_state(ArmadilloState::Idle);
    }

    /// Vanilla `Armadillo.brushOffScute`: drops the brush loot unless a baby.
    pub fn brush_off_scute(&self, tool: &ItemStack) -> bool {
        if AgeableMob::is_baby(self) {
            return false;
        }
        // Vanilla `dropFromEntityInteractLootTable` also passes the
        // interacting entity, which the `brush/armadillo` table does not read.
        let mut rng = rand::rng();
        for item in shearing_loot_items_with_rng(
            self,
            &vanilla_loot_tables::BRUSH_ARMADILLO,
            tool,
            &mut rng,
        ) {
            let _ = self.spawn_at_location(item, 0.0);
        }
        self.play_sound(&sound_events::ENTITY_ARMADILLO_BRUSH, 1.0, 1.0);
        self.game_event(&vanilla_game_events::ENTITY_INTERACT);
        true
    }

    /// Vanilla `LivingEntity.dropFromGiftLootTable`.
    fn drop_gift_loot_table(&self, loot_table: LootTableRef) -> bool {
        let position = self.position();
        let mut rng = rand::rng();
        let equipment = living_loot_equipment(self);
        let mut context = LootContext::new(&mut rng)
            .with_origin(position.x, position.y, position.z)
            .with_this_entity(entity_loot_ref(self, Some(&equipment)));
        let items = loot_table.get_random_items(&mut context);
        let dropped_any = !items.is_empty();
        for item in items {
            let _ = self.spawn_at_location(item, 0.0);
        }
        dropped_any
    }

    fn tick_scute_drop(&self, world: &World) {
        if !Entity::is_alive(self) || self.scute_time.fetch_sub(1, Ordering::Relaxed) - 1 > 0 {
            return;
        }
        if !self.should_drop_loot(world) {
            return;
        }
        if self.drop_gift_loot_table(&vanilla_loot_tables::GAMEPLAY_ARMADILLO_SHED) {
            let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0;
            self.play_sound(&sound_events::ENTITY_ARMADILLO_SCUTE_DROP, 1.0, pitch);
            self.game_event(&vanilla_game_events::ENTITY_PLACE);
        }
        self.scute_time
            .store(Self::pick_next_scute_drop_time(), Ordering::Relaxed);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

/// Vanilla `ArmadilloAi.ArmadilloPanic`: rolls out before panicking.
struct ArmadilloPanic(AnimalPanic<ArmadilloEntity>);

impl Behavior<ArmadilloEntity> for ArmadilloPanic {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        self.0.entry_condition()
    }

    fn duration(&self) -> (i32, i32) {
        self.0.duration()
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &ArmadilloEntity) -> bool {
        self.0.check_extra_start_conditions(world, body)
    }

    fn start(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        body.roll_out();
        self.0.start(world, body, time);
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) -> bool {
        self.0.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        self.0.tick(world, body, time);
    }

    fn stop(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        self.0.stop(world, body, time);
    }
}

/// Vanilla `ArmadilloAi`'s `MoveToTargetSink` that stays put while scared.
struct ArmadilloMoveToTarget(MoveToTargetSink);

impl Behavior<ArmadilloEntity> for ArmadilloMoveToTarget {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        Behavior::<ArmadilloEntity>::entry_condition(&self.0)
    }

    fn duration(&self) -> (i32, i32) {
        Behavior::<ArmadilloEntity>::duration(&self.0)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &ArmadilloEntity) -> bool {
        !body.is_scared() && self.0.check_extra_start_conditions(world, body)
    }

    fn start(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        self.0.start(world, body, time);
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) -> bool {
        self.0.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        self.0.tick(world, body, time);
    }

    fn stop(&mut self, world: &Arc<World>, body: &ArmadilloEntity, time: i64) {
        self.0.stop(world, body, time);
    }
}

/// Five minutes in the shell at most (vanilla `BALL_UP_STAY_IN_STATE`).
const BALL_UP_STAY_IN_STATE: i32 = 5 * 60 * 20;
const DANGER_DETECTED_RECENTLY_DANGER_THRESHOLD: i64 = 75;

/// Vanilla `ArmadilloAi.ArmadilloBallUp`: rolls up and peeks out until the
/// danger is gone.
#[derive(Default)]
struct ArmadilloBallUp {
    next_peek_timer: i32,
    danger_was_around: bool,
}

impl ArmadilloBallUp {
    fn pick_next_peek_timer() -> i32 {
        // `animation_duration` values are small constants.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the scared animation lasts 50 ticks"
        )]
        let scared = animation_duration(ArmadilloState::Scared) as i32;
        scared + rand::random_range(100..=400)
    }
}

impl Behavior<ArmadilloEntity> for ArmadilloBallUp {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &[]
    }

    fn duration(&self) -> (i32, i32) {
        (BALL_UP_STAY_IN_STATE, BALL_UP_STAY_IN_STATE)
    }

    fn check_extra_start_conditions(
        &mut self,
        _world: &Arc<World>,
        body: &ArmadilloEntity,
    ) -> bool {
        body.on_ground() && !body.is_in_liquid()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &ArmadilloEntity, _time: i64) -> bool {
        is_threatened(body.state())
    }

    fn start(&mut self, _world: &Arc<World>, body: &ArmadilloEntity, _time: i64) {
        body.roll_up();
    }

    fn stop(&mut self, _world: &Arc<World>, body: &ArmadilloEntity, _time: i64) {
        if !body.can_stay_rolled_up() {
            body.roll_out();
        }
    }

    fn tick(&mut self, _world: &Arc<World>, body: &ArmadilloEntity, _time: i64) {
        if self.next_peek_timer > 0 {
            self.next_peek_timer -= 1;
        }

        if body.should_switch_to_scared_state() {
            body.switch_to_state(ArmadilloState::Scared);
            if body.on_ground() {
                body.play_sound(&sound_events::ENTITY_ARMADILLO_LAND, 1.0, 1.0);
            }
            return;
        }

        let state = body.state();
        // Vanilla reads the memory's time to live directly; it is present
        // while the panic activity runs.
        let danger_tick_counter = body
            .brain
            .memories()
            .lock()
            .time_until_expiry(&DANGER_DETECTED_RECENTLY)
            .unwrap_or(0);
        let danger_is_around = danger_tick_counter > DANGER_DETECTED_RECENTLY_DANGER_THRESHOLD;
        if danger_is_around != self.danger_was_around {
            self.next_peek_timer = Self::pick_next_peek_timer();
        }
        self.danger_was_around = danger_is_around;

        let unrolling = animation_duration(ArmadilloState::Unrolling);
        if state == ArmadilloState::Scared {
            if self.next_peek_timer == 0 && body.on_ground() && danger_is_around {
                body.broadcast_entity_event(EntityStatus::ArmadilloPeek);
                self.next_peek_timer = Self::pick_next_peek_timer();
            }
            if danger_tick_counter < unrolling {
                body.play_sound(&sound_events::ENTITY_ARMADILLO_UNROLL_START, 1.0, 1.0);
                body.switch_to_state(ArmadilloState::Unrolling);
            }
        } else if state == ArmadilloState::Unrolling && danger_tick_counter > unrolling {
            body.switch_to_state(ArmadilloState::Scared);
        }
    }
}

impl DefaultEntityPush for ArmadilloEntity {}

impl Entity for ArmadilloEntity {
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
        if self.is_scared() {
            self.clamp_head_rotation_to_body();
        }
        self.in_state_ticks.fetch_add(1, Ordering::Relaxed);
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            let mut baby = self.entity_type.dimensions.scale(BABY_SCALE);
            baby.eye_height = BABY_EYE_HEIGHT;
            baby.scale(scale)
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
        SoundSource::Neutral
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_ARMADILLO_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert("state", state_name(self.state()));
        nbt.insert("scute_time", self.scute_time.load(Ordering::Relaxed));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        let state = nbt
            .string("state")
            .and_then(|name| state_by_name(name.to_str().as_ref()))
            .unwrap_or(ArmadilloState::Idle);
        self.switch_to_state(state);
        if let Some(scute_time) = nbt.int("scute_time") {
            self.scute_time.store(scute_time, Ordering::Relaxed);
        }
    }
}

impl LivingEntity for ArmadilloEntity {
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

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let amount = if self.is_scared() {
            (amount - 1.0) / 2.0
        } else {
            amount
        };
        self.default_hurt_server(world, source, amount)
    }

    fn actually_hurt(&self, world: &World, source: &DamageSource, amount: f32) {
        self.default_actually_hurt(world, source, amount);
        if self.is_no_ai() || self.is_dead_or_dying() {
            return;
        }
        if source
            .causing_entity()
            .is_some_and(|entity| entity.is_living_entity())
        {
            self.brain.memories().lock().set_with_expiry(
                &DANGER_DETECTED_RECENTLY,
                true,
                i64::from(SCARE_CHECK_INTERVAL),
            );
            if self.can_stay_rolled_up() {
                self.roll_up();
            }
        } else if source.is(&DamageTypeTag::PANIC_ENVIRONMENTAL_CAUSES) {
            self.roll_out();
        }
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_scared() {
            &sound_events::ENTITY_ARMADILLO_HURT_REDUCED
        } else {
            &sound_events::ENTITY_ARMADILLO_HURT
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ARMADILLO_DEATH)
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

impl AgeableMob for ArmadilloEntity {
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
}

impl Animal for ArmadilloEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::ARMADILLO_FOOD)
    }

    fn can_fall_in_love(&self) -> bool {
        self.in_love_time() <= 0 && !self.is_scared()
    }

    fn play_eating_sound(&self) {
        self.make_sound(Some(&sound_events::ENTITY_ARMADILLO_EAT));
    }
}

impl Mob for ArmadilloEntity {
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
            // Vanilla `ArmadilloAi.updateActivity`.
            self.brain
                .set_active_activity_to_first_valid(&[Activity::PANIC, Activity::IDLE]);
            self.tick_scute_drop(&world);
        }
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if self.is_scared() {
            None
        } else {
            Some(&sound_events::ENTITY_ARMADILLO_AMBIENT)
        }
    }

    fn max_head_y_rot(&self) -> f32 {
        if self.is_scared() {
            0.0
        } else {
            MAX_HEAD_Y_ROT
        }
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if item_stack.is(&vanilla_items::BRUSH) && self.brush_off_scute(&item_stack) {
            player.inventory.lock().hurt_item_in_hand(
                hand,
                BRUSH_DAMAGE,
                player.has_infinite_materials(),
            );
            return InteractionResult::Success;
        }
        if self.is_scared() {
            InteractionResult::Fail
        } else {
            Animal::mob_interact_animal(self, player, hand)
        }
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for ArmadilloEntity {}
