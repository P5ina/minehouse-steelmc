//! Vanilla sniffer (`Sniffer` and `SnifferAi`): sniffs out and digs up
//! ancient seeds, and lays an egg instead of breeding a baby directly.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_data::SnifferState;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::LootContext;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::SnifferEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt, sound_events, vanilla_entities, vanilla_game_events,
    vanilla_items, vanilla_loot_tables,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{
    BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey, GlobalPos,
};

use crate::behavior::InteractionResult;
use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, CountDownCooldownTicks, DoNothing, FollowTemptation,
    LookAtTargetSink, MoveToTargetSink, Swim, random_stroll, run_one, set_entity_look_target,
    set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, IS_PANICKING, PositionTracker, SNIFF_COOLDOWN, SNIFFER_DIGGING,
    SNIFFER_EXPLORED_POSITIONS, SNIFFER_HAPPY, SNIFFER_SNIFFING_TARGET, TEMPTATION_COOLDOWN_TICKS,
    TEMPTING_PLAYER, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::sensors::{HurtBySensor, NearestLivingEntitySensor, TemptingSensor};
use crate::entity::ai::brain::{
    ActivityData, BehaviorControl, Brain, Memories, MemoryStatus, Sensor,
};
use crate::entity::ai::goal::random_pos::land_random_pos;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::leash::Leashable as _;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    PathfinderMob, SharedEntity, entity_loot_ref, living_loot_equipment, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;
use crate::world::game_event::GameEventContext;

#[cfg(test)]
mod tests;

/// Vanilla `Sniffer.SNIFFER_BABY_START_AGE`.
const SNIFFER_BABY_START_AGE: i32 = -48_000;
/// Vanilla `Sniffer.DIGGING_DROP_SEED_OFFSET_TICKS`.
const DIGGING_DROP_SEED_OFFSET_TICKS: i32 = 120;
/// Vanilla `Sniffer.DIGGING_BB_HEIGHT_OFFSET`.
const DIGGING_BB_HEIGHT_OFFSET: f32 = 0.4;
/// Vanilla `DIGGING_DIMENSIONS` eye height.
const DIGGING_EYE_HEIGHT: f32 = 0.81;
/// Vanilla `Sniffer.getMaxHeadYRot`.
const MAX_HEAD_Y_ROT: f32 = 50.0;
/// Vanilla `storeExploredPosition` keeps this many older positions.
const EXPLORED_POSITIONS_KEPT: usize = 20;

/// Vanilla `SnifferAi.MAX_LOOK_DISTANCE`.
const MAX_LOOK_DISTANCE: f32 = 6.0;
/// Vanilla `SnifferAi.SNIFFING_COOLDOWN_TICKS`.
const SNIFFING_COOLDOWN_TICKS: i64 = 9600;
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_SNIFFING: f32 = 1.25;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;

#[entity_behavior(class = "Sniffer")]
/// Vanilla sniffer.
pub struct SnifferEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<SnifferEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SnifferEntity`.
unsafe impl DowncastType for SnifferEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/sniffer");
}

impl SnifferEntity {
    /// Creates a new sniffer at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a sniffer from persisted base entity state.
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
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::Water, -1.0);
            malus.set(PathType::OnTopOfPowderSnow, -1.0);
            malus.set(PathType::DamageCautious, -1.0);
        }
        let mut entity_data = SnifferEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(
                &[&SNIFFER_EXPLORED_POSITIONS],
                Self::sensors(),
                Self::activities(),
                None,
            ),
        }
    }

    /// Vanilla `SensorType.NEAREST_LIVING_ENTITIES`, `HURT_BY` and
    /// `FOOD_TEMPTATIONS`; Steel has no `NEAREST_PLAYERS` sensor, and nothing
    /// in the sniffer's brain reads its memories.
    fn sensors() -> Vec<Box<dyn Sensor<Self>>> {
        vec![
            Box::new(NearestLivingEntitySensor),
            Box::new(HurtBySensor),
            Box::new(TemptingSensor::<Self>::for_animal()),
        ]
    }

    /// Vanilla `SnifferAi.getActivities`.
    fn activities() -> Vec<ActivityData<Self>> {
        vec![
            Self::core_activity(),
            Self::idle_activity(),
            Self::sniffing_activity(),
            Self::dig_activity(),
        ]
    }

    fn core_activity() -> ActivityData<Self> {
        ActivityData::with_first_priority(
            Activity::CORE,
            0,
            vec![
                Box::new(TimedBehavior::new(Swim::new(0.8))),
                Box::new(TimedBehavior::new(ResetSniffingOnStart(AnimalPanic::new(
                    SPEED_MULTIPLIER_WHEN_PANICKING,
                )))),
                Box::new(TimedBehavior::new(MoveToTargetSink::with_timeout(500, 700))),
                Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                    &TEMPTATION_COOLDOWN_TICKS,
                ))),
            ],
        )
    }

    fn sniffing_activity() -> ActivityData<Self> {
        ActivityData::new(
            Activity::SNIFF,
            vec![(0, Box::new(TimedBehavior::new(Searching)))],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&SNIFFER_SNIFFING_TARGET, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValuePresent),
        ])
    }

    fn dig_activity() -> ActivityData<Self> {
        ActivityData::new(
            Activity::DIG,
            vec![
                (0, Box::new(TimedBehavior::new(Digging::new(160, 180)))),
                (0, Box::new(TimedBehavior::new(FinishedDigging::new(40)))),
            ],
        )
        .with_conditions(vec![
            MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&SNIFFER_DIGGING, MemoryStatus::ValuePresent),
        ])
    }

    fn idle_activity() -> ActivityData<Self> {
        let wander: Vec<(Box<dyn BehaviorControl<Self>>, i32)> = vec![
            (
                Box::new(set_walk_target_from_look_target(
                    |_| true,
                    |_| SPEED_MULTIPLIER_WHEN_IDLING,
                    3,
                )),
                2,
            ),
            (Box::new(TimedBehavior::new(Scenting::new(40, 80))), 1),
            (Box::new(TimedBehavior::new(Sniffing::new(40, 80))), 1),
            (
                Box::new(set_entity_look_target(MAX_LOOK_DISTANCE, |entity| {
                    entity.entity_type() == &vanilla_entities::PLAYER
                })),
                1,
            ),
            (
                Box::new(random_stroll(SPEED_MULTIPLIER_WHEN_IDLING, true)),
                1,
            ),
            (Box::new(DoNothing::new(5, 20)), 2),
        ];
        ActivityData::new(
            Activity::IDLE,
            vec![
                (
                    0,
                    Box::new(TimedBehavior::new(ResetSniffingOnStart(
                        AnimalMakeLove::new(&vanilla_entities::SNIFFER),
                    ))),
                ),
                (
                    1,
                    Box::new(TimedBehavior::new(ResetSniffingOnStart(
                        FollowTemptation::with_distance(
                            |_| SPEED_MULTIPLIER_WHEN_TEMPTED,
                            |sniffer: &Self| {
                                if AgeableMob::is_baby(sniffer) {
                                    2.5
                                } else {
                                    3.5
                                }
                            },
                            false,
                        ),
                    ))),
                ),
                (
                    2,
                    Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
                ),
                (3, Box::new(TimedBehavior::new(FeelingHappy::new(40, 100)))),
                (4, Box::new(run_one(Vec::new(), wander))),
            ],
        )
        .with_conditions(vec![MemoryRequirement::new(
            &SNIFFER_DIGGING,
            MemoryStatus::ValueAbsent,
        )])
    }

    /// The sniffer's synced state.
    #[must_use]
    pub fn state(&self) -> SnifferState {
        *self.entity_data.lock().state.get()
    }

    /// Vanilla `Sniffer.setState`; a changed state refreshes the dimensions
    /// (vanilla `onSyncedDataUpdated`).
    fn set_state(&self, state: SnifferState) {
        let changed = {
            let mut data = self.entity_data.lock();
            let changed = *data.state.get() != state;
            data.state.set(state);
            changed
        };
        if changed {
            self.refresh_dimensions();
        }
    }

    /// The tick the digging sniffer drops its seed (vanilla
    /// `DATA_DROP_SEED_AT_TICK`).
    #[must_use]
    pub fn drop_seed_at_tick(&self) -> i32 {
        *self.entity_data.lock().drop_seed_at_tick.get()
    }

    /// Vanilla `Sniffer.isSearching`.
    #[must_use]
    pub fn is_searching(&self) -> bool {
        self.state() == SnifferState::Searching
    }

    /// Vanilla `Sniffer.isTempted`.
    #[must_use]
    pub fn is_tempted(&self) -> bool {
        self.brain.memories().lock().has_value(&TEMPTING_PLAYER)
    }

    /// Vanilla `Sniffer.canSniff`.
    #[must_use]
    pub fn can_sniff(&self) -> bool {
        !self.is_tempted()
            && !self.is_panicking()
            && !self.is_in_water()
            && !self.is_in_love()
            && self.on_ground()
            && !self.is_passenger()
            && !self.is_leashed()
    }

    /// Vanilla `Sniffer.canPlayDiggingSound`.
    #[must_use]
    pub fn can_play_digging_sound(&self) -> bool {
        matches!(
            self.state(),
            SnifferState::Digging | SnifferState::Searching
        )
    }

    /// Vanilla `Sniffer.getHeadPosition`: 2.25 blocks ahead along the look.
    fn head_position(&self) -> DVec3 {
        self.position() + self.look_angle() * 2.25
    }

    /// Vanilla `Sniffer.getHeadBlock`.
    fn head_block(&self) -> BlockPos {
        let head = self.head_position();
        BlockPos::containing(head.x, self.position().y + f64::from(0.2_f32), head.z)
    }

    /// Vanilla `Sniffer.transitionTo`.
    pub fn transition_to(&self, state: SnifferState) {
        match state {
            SnifferState::Idling | SnifferState::Searching => self.set_state(state),
            SnifferState::FeelingHappy => {
                self.play_sound(&sound_events::ENTITY_SNIFFER_HAPPY, 1.0, 1.0);
                self.set_state(state);
            }
            SnifferState::Scenting => {
                self.set_state(state);
                // Vanilla `onScentingStart`.
                let pitch = if AgeableMob::is_baby(self) { 1.3 } else { 1.0 };
                self.play_sound(&sound_events::ENTITY_SNIFFER_SCENTING, 1.0, pitch);
            }
            SnifferState::Sniffing => {
                self.play_sound(&sound_events::ENTITY_SNIFFER_SNIFFING, 1.0, 1.0);
                self.set_state(state);
            }
            SnifferState::Digging => {
                self.set_state(state);
                // Vanilla `onDiggingStart`.
                let drop_at = self.tick_count() + DIGGING_DROP_SEED_OFFSET_TICKS;
                self.entity_data.lock().drop_seed_at_tick.set(drop_at);
                self.broadcast_entity_event(EntityStatus::SnifferDiggingSound);
            }
            SnifferState::Rising => {
                self.play_sound(&sound_events::ENTITY_SNIFFER_DIGGING_STOP, 1.0, 1.0);
                self.set_state(state);
            }
        }
    }

    /// Vanilla `SnifferAi.resetSniffing`.
    fn reset_sniffing(&self) {
        {
            let mut memories = self.brain.memories().lock();
            memories.erase(&SNIFFER_DIGGING);
            memories.erase(&SNIFFER_SNIFFING_TARGET);
        }
        self.transition_to(SnifferState::Idling);
    }

    /// Vanilla `Sniffer.onDiggingComplete`.
    fn on_digging_complete(&self, success: bool) {
        if success && let Some(pos) = self.on_pos(1.0e-5) {
            self.store_explored_position(pos);
        }
    }

    /// Vanilla `Sniffer.calculateDigPosition`.
    fn calculate_dig_position(&self) -> Option<BlockPos> {
        let world = self.level()?;
        let border = world.world_border_snapshot();
        (0..5)
            .filter_map(|idx| land_random_pos(self, 10 + 2 * idx, 3))
            .map(|pos| BlockPos::containing(pos.x, pos.y, pos.z))
            .filter(|&pos| border.is_block_within_bounds(pos))
            .map(|pos| pos.below())
            .find(|&pos| self.can_dig_at(pos))
    }

    /// Vanilla `Sniffer.canDig()`.
    #[must_use]
    pub fn can_dig(&self) -> bool {
        !self.is_panicking()
            && !self.is_tempted()
            && !AgeableMob::is_baby(self)
            && !self.is_in_water()
            && self.on_ground()
            && !self.is_passenger()
            && self.can_dig_at(self.head_block().below())
    }

    /// Vanilla `Sniffer.canDig(BlockPos)`.
    fn can_dig_at(&self, pos: BlockPos) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        if !world
            .get_block_state(pos)
            .get_block()
            .has_tag(&BlockTag::SNIFFER_DIGGABLE_BLOCK)
        {
            return false;
        }
        let global = GlobalPos::new(world.key.clone(), pos);
        if self.explored_positions().contains(&global) {
            return false;
        }
        self.create_path_to(pos, 1)
            .is_some_and(|path| path.can_reach())
    }

    /// Vanilla `Sniffer.getExploredPositions`.
    #[must_use]
    pub fn explored_positions(&self) -> Vec<GlobalPos> {
        self.brain
            .memories()
            .lock()
            .get(&SNIFFER_EXPLORED_POSITIONS)
            .unwrap_or_default()
    }

    /// Vanilla `Sniffer.storeExploredPosition`: the newest first, after at
    /// most 20 older ones.
    fn store_explored_position(&self, pos: BlockPos) {
        let Some(world) = self.level() else {
            return;
        };
        let mut updated = self.explored_positions();
        updated.truncate(EXPLORED_POSITIONS_KEPT);
        updated.insert(0, GlobalPos::new(world.key.clone(), pos));
        self.brain
            .memories()
            .lock()
            .set(&SNIFFER_EXPLORED_POSITIONS, Some(updated));
    }

    /// Vanilla `Sniffer.dropSeed`.
    fn drop_seed(&self, world: &Arc<World>) {
        if self.drop_seed_at_tick() != self.tick_count() {
            return;
        }
        let head = self.head_block();
        let position = self.position();
        let mut rng = rand::rng();
        let equipment = living_loot_equipment(self);
        let mut context = LootContext::new(&mut rng)
            .with_origin(position.x, position.y, position.z)
            .with_this_entity(entity_loot_ref(self, Some(&equipment)));
        let at = DVec3::new(
            f64::from(head.x()),
            f64::from(head.y()),
            f64::from(head.z()),
        );
        for item in vanilla_loot_tables::GAMEPLAY_SNIFFER_DIGGING.get_random_items(&mut context) {
            if let Some(entity) = world.spawn_item(at, item) {
                entity.set_default_pickup_delay();
            }
        }
        self.play_sound(&sound_events::ENTITY_SNIFFER_DROP_SEED, 1.0, 1.0);
    }

    /// The server side of vanilla `Sniffer.emitDiggingParticles`: particles
    /// and the hit sound are client-only.
    fn emit_digging_game_event(&self, world: &Arc<World>) {
        if self.tick_count() % 10 == 0 {
            world.game_event(
                &vanilla_game_events::ENTITY_ACTION,
                self.head_block(),
                &GameEventContext::new(Some(self.as_entity_event_source()), None),
            );
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

/// Vanilla `SnifferAi`'s anonymous behaviors that reset sniffing on start.
struct ResetSniffingOnStart<B>(B);

impl<B: Behavior<SnifferEntity>> Behavior<SnifferEntity> for ResetSniffingOnStart<B> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        self.0.entry_condition()
    }

    fn duration(&self) -> (i32, i32) {
        self.0.duration()
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &SnifferEntity) -> bool {
        self.0.check_extra_start_conditions(world, body)
    }

    fn start(&mut self, world: &Arc<World>, body: &SnifferEntity, time: i64) {
        body.reset_sniffing();
        self.0.start(world, body, time);
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &SnifferEntity, time: i64) -> bool {
        self.0.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &SnifferEntity, time: i64) {
        self.0.tick(world, body, time);
    }

    fn stop(&mut self, world: &Arc<World>, body: &SnifferEntity, time: i64) {
        self.0.stop(world, body, time);
    }

    fn stop_with_end_time(
        &mut self,
        world: &Arc<World>,
        body: &SnifferEntity,
        time: i64,
        end_time: i64,
    ) {
        self.0.stop_with_end_time(world, body, time, end_time);
    }

    fn timed_out(&self, time: i64, end_time: i64) -> bool {
        self.0.timed_out(time, end_time)
    }
}

const DIGGING_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_DIGGING, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&SNIFF_COOLDOWN, MemoryStatus::ValueAbsent),
];

/// Vanilla `SnifferAi.Digging`.
struct Digging {
    min_duration: i32,
    max_duration: i32,
}

impl Digging {
    const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
        }
    }
}

impl Behavior<SnifferEntity> for Digging {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &DIGGING_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_duration, self.max_duration)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &SnifferEntity) -> bool {
        body.can_sniff()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) -> bool {
        body.brain.memories().lock().has_value(&SNIFFER_DIGGING)
            && body.can_dig()
            && !body.is_in_love()
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Digging);
    }

    fn stop_with_end_time(
        &mut self,
        _world: &Arc<World>,
        body: &SnifferEntity,
        time: i64,
        end_time: i64,
    ) {
        if self.timed_out(time, end_time) {
            body.brain.memories().lock().set_with_expiry(
                &SNIFF_COOLDOWN,
                (),
                SNIFFING_COOLDOWN_TICKS,
            );
        } else {
            body.reset_sniffing();
        }
    }
}

const FEELING_HAPPY_ENTRY: [MemoryRequirement; 1] = [MemoryRequirement::new(
    &SNIFFER_HAPPY,
    MemoryStatus::ValuePresent,
)];

/// Vanilla `SnifferAi.FeelingHappy`.
struct FeelingHappy {
    min_duration: i32,
    max_duration: i32,
}

impl FeelingHappy {
    const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
        }
    }
}

impl Behavior<SnifferEntity> for FeelingHappy {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &FEELING_HAPPY_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_duration, self.max_duration)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &SnifferEntity, _time: i64) -> bool {
        true
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::FeelingHappy);
    }

    fn stop(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Idling);
        body.brain.memories().lock().erase(&SNIFFER_HAPPY);
    }
}

const FINISHED_DIGGING_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_DIGGING, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&SNIFF_COOLDOWN, MemoryStatus::ValuePresent),
];

/// Vanilla `SnifferAi.FinishedDigging`.
struct FinishedDigging {
    duration: i32,
}

impl FinishedDigging {
    const fn new(duration: i32) -> Self {
        Self { duration }
    }
}

impl Behavior<SnifferEntity> for FinishedDigging {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &FINISHED_DIGGING_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.duration, self.duration)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) -> bool {
        body.brain.memories().lock().has_value(&SNIFFER_DIGGING)
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Rising);
    }

    fn stop_with_end_time(
        &mut self,
        _world: &Arc<World>,
        body: &SnifferEntity,
        time: i64,
        end_time: i64,
    ) {
        let finished = self.timed_out(time, end_time);
        body.transition_to(SnifferState::Idling);
        body.on_digging_complete(finished);
        let mut memories = body.brain.memories().lock();
        memories.erase(&SNIFFER_DIGGING);
        memories.set(&SNIFFER_HAPPY, Some(true));
    }
}

const SCENTING_ENTRY: [MemoryRequirement; 5] = [
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_DIGGING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_SNIFFING_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_HAPPY, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
];

/// Vanilla `SnifferAi.Scenting`.
struct Scenting {
    min_duration: i32,
    max_duration: i32,
}

impl Scenting {
    const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
        }
    }
}

impl Behavior<SnifferEntity> for Scenting {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SCENTING_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_duration, self.max_duration)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &SnifferEntity) -> bool {
        !body.is_tempted()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &SnifferEntity, _time: i64) -> bool {
        true
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Scenting);
    }

    fn stop(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Idling);
    }
}

const SEARCHING_ENTRY: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_SNIFFING_TARGET, MemoryStatus::ValuePresent),
];

/// Vanilla `SnifferAi.Searching`: walks to the sniffed position.
struct Searching;

impl Behavior<SnifferEntity> for Searching {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SEARCHING_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (600, 600)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &SnifferEntity) -> bool {
        body.can_sniff()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) -> bool {
        if !body.can_sniff() {
            body.transition_to(SnifferState::Idling);
            return false;
        }
        let memories = body.brain.memories().lock();
        let walk_target = memories
            .get(&WALK_TARGET)
            .map(|target| target.target.current_block_position());
        let sniffing_target = memories.get(&SNIFFER_SNIFFING_TARGET);
        matches!((walk_target, sniffing_target), (Some(walk), Some(sniff)) if walk == sniff)
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Searching);
    }

    fn stop(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        if body.can_dig() && body.can_sniff() {
            body.brain
                .memories()
                .lock()
                .set(&SNIFFER_DIGGING, Some(true));
        }
        let mut memories = body.brain.memories().lock();
        memories.erase(&WALK_TARGET);
        memories.erase(&SNIFFER_SNIFFING_TARGET);
    }
}

const SNIFFING_ENTRY: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFFER_SNIFFING_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SNIFF_COOLDOWN, MemoryStatus::ValueAbsent),
];

/// Vanilla `SnifferAi.Sniffing`: picks a diggable spot when it finishes.
struct Sniffing {
    min_duration: i32,
    max_duration: i32,
}

impl Sniffing {
    const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
        }
    }
}

impl Behavior<SnifferEntity> for Sniffing {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SNIFFING_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_duration, self.max_duration)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &SnifferEntity) -> bool {
        !AgeableMob::is_baby(body) && body.can_sniff()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) -> bool {
        body.can_sniff()
    }

    fn start(&mut self, _world: &Arc<World>, body: &SnifferEntity, _time: i64) {
        body.transition_to(SnifferState::Sniffing);
    }

    fn stop_with_end_time(
        &mut self,
        _world: &Arc<World>,
        body: &SnifferEntity,
        time: i64,
        end_time: i64,
    ) {
        let finished = self.timed_out(time, end_time);
        body.transition_to(SnifferState::Idling);
        if finished && let Some(pos) = body.calculate_dig_position() {
            let mut memories = body.brain.memories().lock();
            memories.set(&SNIFFER_SNIFFING_TARGET, Some(pos));
            memories.set(
                &WALK_TARGET,
                Some(WalkTarget::new(
                    PositionTracker::block(pos),
                    SPEED_MULTIPLIER_WHEN_SNIFFING,
                    0,
                )),
            );
        }
    }
}

impl DefaultEntityPush for SnifferEntity {}

impl Entity for SnifferEntity {
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
        // Vanilla `Sniffer.tick`: the searching sound is client-only.
        if self.state() == SnifferState::Digging
            && let Some(world) = self.level()
        {
            self.emit_digging_game_event(&world);
            self.drop_seed(&world);
        }
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    /// Vanilla `Sniffer.getDefaultDimensions`, scaled by `getScale`.
    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let dimensions = self.entity_type.dimensions;
        let dimensions = if self.state() == SnifferState::Digging {
            EntityDimensions::new(
                dimensions.width,
                dimensions.height - DIGGING_BB_HEIGHT_OFFSET,
                DIGGING_EYE_HEIGHT,
            )
        } else {
            dimensions
        };
        let dimensions = dimensions.scale(self.get_age_scale());
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

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_SNIFFER_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Brain", self.brain.save());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
    }
}

impl LivingEntity for SnifferEntity {
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

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SNIFFER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SNIFFER_DEATH)
    }

    fn die(&self, source: &DamageSource) {
        self.transition_to(SnifferState::Idling);
        self.default_die(source);
    }

    /// Vanilla `Sniffer.jumpFromGround`: nudges a stuck walker forward.
    fn jump_from_ground(&self) {
        self.default_jump_from_ground();
        let speed_modifier = self
            .mob_base
            .controls()
            .lock()
            .move_control
            .speed_modifier();
        if speed_modifier > 0.0 {
            let velocity = self.velocity();
            if velocity.x * velocity.x + velocity.z * velocity.z < 0.01 {
                self.move_relative(0.1, DVec3::new(0.0, 0.0, 1.0));
            }
        }
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

impl AgeableMob for SnifferEntity {
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

    fn get_baby_start_age(&self) -> i32 {
        SNIFFER_BABY_START_AGE
    }
}

impl Animal for SnifferEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::SNIFFER_FOOD)
    }

    /// Vanilla `Sniffer.canMate`: both sniffers idle, scenting or happy.
    fn can_mate(&self, partner: &dyn Animal) -> bool {
        let Some(partner_sniffer) = partner.downcast_ref::<Self>() else {
            return false;
        };
        let can_breed_in = |state: SnifferState| {
            matches!(
                state,
                SnifferState::Idling | SnifferState::Scenting | SnifferState::FeelingHappy
            )
        };
        can_breed_in(self.state())
            && can_breed_in(partner_sniffer.state())
            && self.uuid() != partner.uuid()
            && self.entity_type() == partner.entity_type()
            && self.is_in_love()
            && partner.is_in_love()
    }

    /// Vanilla `Sniffer.spawnChildFromBreeding`: drops a sniffer egg.
    fn spawn_child_from_breeding(&self, world: &Arc<World>, partner: &dyn Animal) {
        let egg = ItemStack::new(&vanilla_items::SNIFFER_EGG);
        self.finalize_spawn_child_from_breeding(world, partner, None);
        let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 0.5;
        self.play_sound(&sound_events::BLOCK_SNIFFER_EGG_PLOP, 1.0, pitch);
        if let Some(entity) = world.spawn_item(self.position(), egg) {
            entity.set_default_pickup_delay();
        }
    }

    fn play_eating_sound(&self) {
        let pitch = rand::random::<f32>() * 0.4 + 0.8;
        self.play_sound(&sound_events::ENTITY_SNIFFER_EAT, 1.0, pitch);
    }
}

impl Mob for SnifferEntity {
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
            // Vanilla `SnifferAi.updateActivity`.
            self.brain.set_active_activity_to_first_valid(&[
                Activity::DIG,
                Activity::SNIFF,
                Activity::IDLE,
            ]);
        }
        Animal::custom_server_ai_step_animal(self);
    }

    fn on_pathfinding_start(&self) {
        if self.is_on_fire() || self.is_in_water() {
            self.set_pathfinding_malus(PathType::Water, 0.0);
        }
    }

    fn on_pathfinding_done(&self) {
        self.set_pathfinding_malus(PathType::Water, -1.0);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if matches!(
            self.state(),
            SnifferState::Digging | SnifferState::Searching
        ) {
            None
        } else {
            Some(&sound_events::ENTITY_SNIFFER_IDLE)
        }
    }

    fn max_head_y_rot(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    /// Vanilla `Sniffer.mobInteract`: plays the eating sound again on top of
    /// `Animal.mobInteract`'s when food was used.
    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let is_food = {
            let inventory = player.inventory.lock();
            self.is_food(inventory.get_item_in_hand(hand))
        };
        let result = Animal::mob_interact_animal(self, player, hand);
        if result.consumes_action() && is_food {
            self.play_eating_sound();
        }
        result
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for SnifferEntity {}
