//! Brain combat behaviors (vanilla `StartAttacking`,
//! `StopAttackingIfTargetInvalid`, `SetWalkTargetFromAttackTargetIfTargetOutOfReach`,
//! `MeleeAttack` and `BehaviorBuilder.triggerIf`).

use std::sync::Arc;

use steel_utils::types::InteractionHand;

use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_COOLING_DOWN, ATTACK_TARGET, CANT_REACH_WALK_TARGET_SINCE, LOOK_TARGET,
    NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, BehaviorStatus, Memories, MemoryStatus};
use crate::entity::{Entity, LivingEntity, Mob, SharedEntity};
use crate::world::World;

/// Ticks a brain keeps trying to reach its attack target before giving up.

/// Vanilla `BehaviorBuilder.triggerIf(predicate, behavior)`: runs `behavior`
/// only while `predicate` holds when it would start.
pub fn trigger_if<E: ?Sized, B: BehaviorControl<E>>(
    predicate: impl Fn(&E) -> bool + Send + 'static,
    behavior: B,
) -> impl BehaviorControl<E> {
    TriggerIf {
        predicate,
        behavior,
    }
}

struct TriggerIf<P, B> {
    predicate: P,
    behavior: B,
}

impl<E, P, B> BehaviorControl<E> for TriggerIf<P, B>
where
    E: ?Sized,
    P: Fn(&E) -> bool + Send,
    B: BehaviorControl<E>,
{
    fn status(&self) -> BehaviorStatus {
        self.behavior.status()
    }

    fn register_memories(&self, memories: &mut Memories) {
        self.behavior.register_memories(memories);
    }

    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        (self.predicate)(body) && self.behavior.try_start(world, body, time)
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.behavior.tick_or_stop(world, body, time);
    }

    fn do_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.behavior.do_stop(world, body, time);
    }
}

/// Vanilla `BehaviorUtils.isOtherTargetMuchFurtherAwayThanCurrentAttackTarget`.
#[must_use]
pub fn is_other_target_much_further_away_than_current_attack_target(
    body: &(impl LivingEntity + ?Sized),
    other_target: &dyn Entity,
    how_much_further_away: f64,
) -> bool {
    let Some(current) = body
        .brain_memories()
        .and_then(|memories| memories.lock().get(&ATTACK_TARGET))
    else {
        return false;
    };
    let position = body.position();
    let current_distance_sqr = position.distance_squared(current.position());
    let other_distance_sqr = position.distance_squared(other_target.position());
    other_distance_sqr > current_distance_sqr + how_much_further_away * how_much_further_away
}

/// Vanilla `BehaviorUtils.getNearestTarget`: `new_target`, unless `current`
/// is set and strictly closer to the body.
#[must_use]
pub fn get_nearest_target(
    body: &(impl Entity + ?Sized),
    current: Option<SharedEntity>,
    new_target: SharedEntity,
) -> SharedEntity {
    let Some(current) = current else {
        return new_target;
    };
    let position = body.position();
    if position.distance_squared(current.position())
        < position.distance_squared(new_target.position())
    {
        current
    } else {
        new_target
    }
}
