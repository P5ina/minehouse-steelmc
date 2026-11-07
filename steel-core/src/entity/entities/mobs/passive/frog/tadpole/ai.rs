//! Vanilla `TadpoleAi`.

use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, vanilla_entities};

use super::TadpoleEntity;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::gate::{GateBehavior, OrderPolicy, RunningPolicy};
use crate::entity::ai::brain::behaviors::{
    AnimalPanic, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink, MoveToTargetSink,
    UniformInt, random_swim, set_entity_look_target_sometimes, set_walk_target_from_look_target,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{TEMPTATION_COOLDOWN_TICKS, WALK_TARGET};
use crate::entity::ai::brain::sensors::{
    HurtBySensor, NearestLivingEntitySensor, PlayerSensor, TemptingSensor,
};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, MemoryStatus, Sensor};
use crate::entity::{Entity, LivingEntity};
use crate::world::World;

const SPEED_MULTIPLIER_WHEN_PANICKING: f32 = 2.0;
const SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER: f32 = 0.5;
const SPEED_MULTIPLIER_WHEN_TEMPTED: f32 = 1.25;
const LOOK_AT_PLAYER_DISTANCE: f32 = 6.0;
const LOOK_AT_PLAYER_INTERVAL: UniformInt = UniformInt::of(30, 60);

/// Vanilla `SensorType.FROG_TEMPTATIONS`.
pub(super) fn is_frog_food(item: &ItemStack) -> bool {
    REGISTRY.items.is_in_tag(item.item(), &ItemTag::FROG_FOOD)
}

/// The sensors vanilla's tadpole `Brain.provider` lists.
pub(super) fn sensors() -> Vec<Box<dyn Sensor<TadpoleEntity>>> {
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(PlayerSensor),
        Box::new(HurtBySensor),
        Box::new(TemptingSensor::<TadpoleEntity>::new(is_frog_food)),
    ]
}

/// Vanilla `TadpoleAi.getActivities`.
pub(super) fn activities() -> Vec<ActivityData<TadpoleEntity>> {
    vec![core_activity(), idle_activity()]
}

/// Vanilla `TadpoleAi.updateActivity`.
pub(super) fn update_activity(body: &TadpoleEntity) {
    body.brain()
        .set_active_activity_to_first_valid(&[Activity::IDLE]);
}

/// Vanilla `BehaviorBuilder.triggerIf`.
fn trigger_if(
    predicate: impl Fn(&TadpoleEntity) -> bool + Send + 'static,
) -> impl BehaviorControl<TadpoleEntity> {
    OneShot::new(
        Vec::new(),
        move |_world: &Arc<World>, body: &TadpoleEntity, _time: i64| predicate(body),
    )
}

fn core_activity() -> ActivityData<TadpoleEntity> {
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
        ],
    )
}

fn idle_activity() -> ActivityData<TadpoleEntity> {
    ActivityData::new(
        Activity::IDLE,
        vec![
            (
                0,
                Box::new(set_entity_look_target_sometimes(
                    LOOK_AT_PLAYER_DISTANCE,
                    LOOK_AT_PLAYER_INTERVAL,
                    |entity: &dyn LivingEntity| entity.entity_type() == &vanilla_entities::PLAYER,
                )),
            ),
            (
                1,
                Box::new(TimedBehavior::new(FollowTemptation::new(|_| {
                    SPEED_MULTIPLIER_WHEN_TEMPTED
                }))),
            ),
            (
                2,
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
                        (Box::new(trigger_if(Entity::is_in_water)), 5),
                    ],
                )),
            ),
        ],
    )
}
