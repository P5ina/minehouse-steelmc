//! Vanilla `NautilusAi` and `ZombieNautilusAi`.

use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, vanilla_entities};

use super::AbstractNautilus;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::TimedBehavior;
use crate::entity::ai::brain::behaviors::gate::{GateBehavior, OrderPolicy, RunningPolicy};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, AnimalPanic, ChargeAttack, CountDownCooldownTicks, FollowTemptation,
    LookAtTargetSink, MoveToTargetSink, UniformInt, random_swim, set_walk_target_from_look_target,
    start_attacking,
};
use crate::entity::ai::brain::memory::{AnyMemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    ANGRY_AT, ATTACK_TARGET, ATTACK_TARGET_COOLDOWN, BREED_TARGET, CANT_REACH_WALK_TARGET_SINCE,
    CHARGE_COOLDOWN_TICKS, NEAREST_VISIBLE_LIVING_ENTITIES, TEMPTATION_COOLDOWN_TICKS,
    TEMPTING_PLAYER, WALK_TARGET,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable_ignoring_line_of_sight;
use crate::entity::ai::brain::sensors::{
    AdultSensor, HurtBySensor, NearestLivingEntitySensor, PlayerSensor, TemptingSensor,
};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, MemoryStatus, Sensor};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{AgeableMob, LivingEntity, SharedEntity};
use crate::world::World;

/// Vanilla `NautilusAi.TIME_BETWEEN_NON_PLAYER_ATTACKS`.
const TIME_BETWEEN_NON_PLAYER_ATTACKS: UniformInt = UniformInt::of(2400, 3600);
/// Vanilla `NautilusAi.ANGER_DURATION`.
const ANGER_DURATION: i64 = 400;
/// Vanilla `NautilusAi.TIME_BETWEEN_ATTACKS`.
const TIME_BETWEEN_ATTACKS: i32 = 80;
/// Vanilla `NautilusAi.ATTACK_KNOCKBACK_FORCE`.
const ATTACK_KNOCKBACK_FORCE: f32 = 2.0;
/// Vanilla `NautilusAi.MAX_CHARGE_DISTANCE`.
const MAX_CHARGE_DISTANCE: f64 = 12.0;
/// Vanilla `NautilusAi.MAX_TARGET_DETECTION_DISTANCE`.
const MAX_TARGET_DETECTION_DISTANCE: f64 = 11.0;
const SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER: f32 = 1.0;
const SPEED_MULTIPLIER_WHEN_MAKING_LOVE: f32 = 0.4;
const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 1.6;
const BABY_TEMPTATION_DISTANCE: f64 = 2.5;
const ADULT_TEMPTATION_DISTANCE: f64 = 3.5;
/// Vanilla `NautilusAi.findNearestValidAttackTarget`: half the times a
/// wild nautilus looks for prey it decides not to.
const SKIP_HUNT_CHANCE: f32 = 0.5;

/// How the nautilus and the zombie nautilus brains differ.
pub(super) struct NautilusAiKind {
    /// Only the living nautilus panics and breeds.
    pub(super) living: bool,
    pub(super) tempted_speed: f32,
    pub(super) attack_speed: f32,
    pub(super) dash_sound: SoundEventRef,
}

/// The memories vanilla `Brain.provider` lists for both nautiluses.
pub(super) fn memory_types() -> [&'static dyn AnyMemoryModuleType; 2] {
    [&ANGRY_AT, &ATTACK_TARGET_COOLDOWN]
}

/// Vanilla `SensorType.NAUTILUS_TEMPTATIONS`.
fn is_tempting(item: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item.item(), &ItemTag::NAUTILUS_FOOD)
}

/// The sensors vanilla `Brain.provider` lists for both nautiluses.
pub(super) fn sensors<E: AbstractNautilus>() -> Vec<Box<dyn Sensor<E>>> {
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(AdultSensor),
        Box::new(PlayerSensor),
        Box::new(HurtBySensor),
        Box::new(TemptingSensor::<E>::new(is_tempting)),
    ]
}

/// Vanilla `NautilusAi.getActivities` and `ZombieNautilusAi.getActivities`.
pub(super) fn activities<E: AbstractNautilus>(kind: &NautilusAiKind) -> Vec<ActivityData<E>> {
    vec![
        core_activity(kind),
        idle_activity(kind),
        fight_activity(kind),
    ]
}

fn core_activity<E: AbstractNautilus>(kind: &NautilusAiKind) -> ActivityData<E> {
    let mut behaviors: Vec<Box<dyn BehaviorControl<E>>> = Vec::new();
    if kind.living {
        behaviors.push(Box::new(TimedBehavior::new(AnimalPanic::<E>::new(
            SPEED_MULTIPLIER_WHEN_PANICKING,
        ))));
    }
    behaviors.push(Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))));
    behaviors.push(Box::new(TimedBehavior::new(MoveToTargetSink::new())));
    for cooldown in [
        &TEMPTATION_COOLDOWN_TICKS,
        &CHARGE_COOLDOWN_TICKS,
        &ATTACK_TARGET_COOLDOWN,
    ] {
        behaviors.push(Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
            cooldown,
        ))));
    }
    ActivityData::with_first_priority(Activity::CORE, 0, behaviors)
}

fn idle_activity<E: AbstractNautilus>(kind: &NautilusAiKind) -> ActivityData<E> {
    let tempted_speed = kind.tempted_speed;
    let mut behaviors: Vec<(i32, Box<dyn BehaviorControl<E>>)> = Vec::new();
    // The zombie nautilus does not breed, so its priorities start one lower.
    let mut priority = 1;
    if kind.living {
        behaviors.push((
            priority,
            Box::new(TimedBehavior::new(AnimalMakeLove::with_speed(
                &vanilla_entities::NAUTILUS,
                SPEED_MULTIPLIER_WHEN_MAKING_LOVE,
                2,
            ))),
        ));
        priority += 1;
    }
    behaviors.push((
        priority,
        Box::new(TimedBehavior::new(FollowTemptation::with_distance(
            move |_| tempted_speed,
            |body: &E| {
                if AgeableMob::is_baby(body) {
                    BABY_TEMPTATION_DISTANCE
                } else {
                    ADULT_TEMPTATION_DISTANCE
                }
            },
            false,
        ))),
    ));
    behaviors.push((
        priority + 1,
        Box::new(start_attacking(|world: &Arc<World>, body: &E| {
            find_nearest_valid_attack_target(world, body)
        })),
    ));
    behaviors.push((
        priority + 2,
        Box::new(GateBehavior::new(
            vec![MemoryRequirement::new(
                &WALK_TARGET,
                MemoryStatus::ValueAbsent,
            )],
            Vec::new(),
            OrderPolicy::Ordered,
            RunningPolicy::TryAll,
            vec![
                (
                    Box::new(random_swim(SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER)),
                    2,
                ),
                (
                    Box::new(set_walk_target_from_look_target(
                        |_| true,
                        |_| SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER,
                        3,
                    )),
                    3,
                ),
            ],
        )),
    ));
    ActivityData::new(Activity::IDLE, behaviors)
}

fn fight_activity<E: AbstractNautilus>(kind: &NautilusAiKind) -> ActivityData<E> {
    ActivityData::new(
        Activity::FIGHT,
        vec![(
            0,
            Box::new(TimedBehavior::new(ChargeAttack::new(
                TIME_BETWEEN_ATTACKS,
                attack_target_conditions(),
                kind.attack_speed,
                ATTACK_KNOCKBACK_FORCE,
                MAX_CHARGE_DISTANCE,
                MAX_TARGET_DETECTION_DISTANCE,
                kind.dash_sound,
            ))),
        )],
    )
    .with_conditions(vec![
        MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
        MemoryRequirement::new(&TEMPTING_PLAYER, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
        MemoryRequirement::new(&CHARGE_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
    ])
}

/// Vanilla `NautilusAi.ATTACK_TARGET_CONDITIONS`.
fn attack_target_conditions() -> TargetingConditions {
    TargetingConditions::for_combat().selector(|target, world| {
        (world.get_game_rule(&MOB_GRIEFING)
            || target.entity_type() != &vanilla_entities::ARMOR_STAND)
            && world
                .world_border_snapshot()
                .is_within_bounds(target.bounding_box())
    })
}

/// Vanilla `NautilusAi.initMemories`.
pub(super) fn init_memories(body: &(impl LivingEntity + ?Sized)) {
    if let Some(memories) = body.brain_memories() {
        memories.lock().set(
            &ATTACK_TARGET_COOLDOWN,
            Some(TIME_BETWEEN_NON_PLAYER_ATTACKS.sample()),
        );
    }
}

/// Vanilla `NautilusAi.findNearestValidAttackTarget`: a wild, grown,
/// swimming nautilus goes for whoever angered it, else now and then for
/// nearby prey.
fn find_nearest_valid_attack_target<E: AbstractNautilus>(
    world: &Arc<World>,
    body: &E,
) -> Option<SharedEntity> {
    let memories = body.brain_memories()?;
    let (is_breeding, angry_at, on_cooldown) = {
        let memories = memories.lock();
        (
            memories.has_value(&BREED_TARGET),
            memories.get(&ANGRY_AT),
            memories.has_value(&ATTACK_TARGET_COOLDOWN),
        )
    };
    if is_breeding || !body.is_in_water() || AgeableMob::is_baby(body) || body.is_tame() {
        return None;
    }
    let angry_at = angry_at
        .and_then(|uuid| world.get_entity_by_uuid(&uuid))
        .filter(|entity| {
            entity.is_in_water()
                && entity.as_living_entity().is_some_and(|living| {
                    is_entity_attackable_ignoring_line_of_sight(world, body, living)
                })
        });
    if angry_at.is_some() {
        return angry_at;
    }
    if on_cooldown {
        return None;
    }
    memories.lock().set(
        &ATTACK_TARGET_COOLDOWN,
        Some(TIME_BETWEEN_NON_PLAYER_ATTACKS.sample()),
    );
    if rand::random::<f32>() < SKIP_HUNT_CHANCE {
        return None;
    }
    let visible = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES)?;
    visible.find_closest(is_hostile_target)
}

/// Vanilla `NautilusAi.isHostileTarget`.
fn is_hostile_target(entity: &SharedEntity) -> bool {
    entity.is_in_water()
        && REGISTRY
            .entity_types
            .is_in_tag(entity.entity_type(), &EntityTypeTag::NAUTILUS_HOSTILES)
}

/// Vanilla `NautilusAi.setAngerTarget`.
pub(super) fn set_anger_target(world: &World, body: &dyn LivingEntity, target: &dyn LivingEntity) {
    if !is_entity_attackable_ignoring_line_of_sight(world, body, target) {
        return;
    }
    if let Some(memories) = body.brain_memories() {
        let mut memories = memories.lock();
        memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
        memories.set_with_expiry(&ANGRY_AT, target.uuid(), ANGER_DURATION);
    }
}

/// Vanilla `NautilusAi.updateActivity`.
pub(super) const UPDATE_ACTIVITIES: [Activity; 2] = [Activity::FIGHT, Activity::IDLE];
