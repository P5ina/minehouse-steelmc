//! Vanilla `CreakingAi`.

use std::sync::Arc;

use super::CreakingEntity;
use crate::entity::LivingEntity;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    DoNothing, LookAtTargetSink, MoveToTargetSink, Swim, UniformInt, melee_attack_when,
    random_stroll, run_one, set_entity_look_target_sometimes,
    set_walk_target_from_attack_target_if_target_out_of_reach, set_walk_target_from_look_target,
    start_attacking, stop_attacking_if_target_invalid_when,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, NEAREST_VISIBLE_ATTACKABLE_PLAYER, NEAREST_VISIBLE_ATTACKABLE_PLAYERS,
};
use crate::entity::ai::brain::sensors::{NearestLivingEntitySensor, PlayerSensor};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, MemoryStatus, Sensor};
use crate::world::World;

/// Vanilla `Creaking.SPEED_MULTIPLIER_WHEN_IDLING`.
const SPEED_MULTIPLIER_WHEN_IDLING: f32 = 0.3;
/// Vanilla `Creaking.ATTACK_INTERVAL`.
const ATTACK_INTERVAL: i32 = 40;

/// Vanilla `CreakingAi.getActivities`.
pub(super) fn activities() -> Vec<ActivityData<CreakingEntity>> {
    vec![core_activity(), idle_activity(), fight_activity()]
}

/// Vanilla `Creaking.BRAIN_PROVIDER` sensors.
pub(super) fn sensors() -> Vec<Box<dyn Sensor<CreakingEntity>>> {
    vec![Box::new(NearestLivingEntitySensor), Box::new(PlayerSensor)]
}

/// Vanilla `CreakingAi`'s `Swim` subclass: only while it can move.
struct CreakingSwim(Swim);

impl Behavior<CreakingEntity> for CreakingSwim {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        <Swim as Behavior<CreakingEntity>>::entry_condition(&self.0)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &CreakingEntity) -> bool {
        body.can_move() && self.0.check_extra_start_conditions(world, body)
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &CreakingEntity, time: i64) -> bool {
        self.0.can_still_use(world, body, time)
    }

    fn tick(&mut self, world: &Arc<World>, body: &CreakingEntity, time: i64) {
        self.0.tick(world, body, time);
    }
}

fn core_activity() -> ActivityData<CreakingEntity> {
    ActivityData::with_first_priority(
        Activity::CORE,
        0,
        vec![
            Box::new(TimedBehavior::new(CreakingSwim(Swim::new(0.8)))),
            Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
            Box::new(TimedBehavior::new(MoveToTargetSink::new())),
        ],
    )
}

fn idle_activity() -> ActivityData<CreakingEntity> {
    let wander: Vec<(Box<dyn BehaviorControl<CreakingEntity>>, i32)> = vec![
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
            Box::new(start_attacking(
                |_world: &Arc<World>, creaking: &CreakingEntity| {
                    if !creaking.is_active() {
                        return None;
                    }
                    creaking
                        .memories()
                        .lock()
                        .get(&NEAREST_VISIBLE_ATTACKABLE_PLAYER)
                },
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

fn fight_activity() -> ActivityData<CreakingEntity> {
    ActivityData::with_first_priority(
        Activity::FIGHT,
        10,
        vec![
            Box::new(set_walk_target_from_attack_target_if_target_out_of_reach(
                |_| 1.0,
            )),
            Box::new(melee_attack_when(CreakingEntity::can_move, ATTACK_INTERVAL)),
            Box::new(stop_attacking_if_target_invalid_when(
                |_world: &Arc<World>, creaking: &CreakingEntity, target: &dyn LivingEntity| {
                    !is_attack_target_still_reachable(creaking, target)
                },
                |_, _, _| {},
            )),
        ],
    )
    .with_conditions(vec![MemoryRequirement::new(
        &ATTACK_TARGET,
        MemoryStatus::ValuePresent,
    )])
}

/// Vanilla `CreakingAi.isAttackTargetStillReachable`: the target is still
/// one of the visible attackable players.
fn is_attack_target_still_reachable(creaking: &CreakingEntity, target: &dyn LivingEntity) -> bool {
    if target.as_player().is_none() {
        return false;
    }
    let players = creaking
        .memories()
        .lock()
        .get(&NEAREST_VISIBLE_ATTACKABLE_PLAYERS);
    players.is_some_and(|players| players.iter().any(|player| player.id() == target.id()))
}

/// Vanilla `CreakingAi.updateActivity`.
pub(super) fn update_activity(creaking: &CreakingEntity) {
    if creaking.can_move() {
        creaking
            .brain
            .set_active_activity_to_first_valid(&[Activity::FIGHT, Activity::IDLE]);
    } else {
        creaking.brain.use_default_activity();
    }
}
