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
const TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE: i64 = 200;

/// Vanilla `StartAttacking`: picks the target `find_target` returns, when
/// `can_attack` allows and the body has no attack target yet.
pub fn start_attacking<E: Mob + ?Sized>(
    can_attack: impl Fn(&Arc<World>, &E) -> bool + Send + 'static,
    find_target: impl Fn(&Arc<World>, &E) -> Option<SharedEntity> + Send + 'static,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&ATTACK_TARGET) || !can_attack(world, body) {
                return false;
            }
            let Some(target) = find_target(world, body) else {
                return false;
            };
            if !target
                .as_living_entity()
                .is_some_and(|living| Mob::can_attack(body, living))
            {
                return false;
            }
            let mut memories = memories.lock();
            memories.set(&ATTACK_TARGET, Some(target));
            memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
            true
        },
    )
}

/// Vanilla `StopAttackingIfTargetInvalid`: forgets the attack target once it
/// is dead, gone, unattackable, matches `stop_when`, or (with
/// `can_grow_tired`) unreachable for 10 seconds.
pub fn stop_attacking_if_target_invalid<E: Mob + ?Sized>(
    stop_when: impl Fn(&Arc<World>, &SharedEntity) -> bool + Send + 'static,
    on_target_erased: impl Fn(&Arc<World>, &E, &SharedEntity) + Send + 'static,
    can_grow_tired: bool,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let (target, cant_reach_since) = {
                let memories = memories.lock();
                let Some(target) = memories.get(&ATTACK_TARGET) else {
                    return false;
                };
                (target, memories.get(&CANT_REACH_WALK_TARGET_SINCE))
            };
            let still_valid = target
                .as_living_entity()
                .is_some_and(|living| Mob::can_attack(body, living))
                && !(can_grow_tired && is_tired_of_trying_to_reach_target(world, cant_reach_since))
                && target.is_alive()
                && same_level(body, &target)
                && !stop_when(world, &target);
            if !still_valid {
                on_target_erased(world, body, &target);
                memories.lock().erase(&ATTACK_TARGET);
            }
            true
        },
    )
}

fn is_tired_of_trying_to_reach_target(world: &World, cant_reach_since: Option<i64>) -> bool {
    cant_reach_since
        .is_some_and(|since| world.game_time() - since > TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE)
}

fn same_level(body: &(impl Entity + ?Sized), target: &SharedEntity) -> bool {
    match (body.level(), target.level()) {
        (Some(body_world), Some(target_world)) => Arc::ptr_eq(&body_world, &target_world),
        _ => false,
    }
}

/// Vanilla `BehaviorUtils.isWithinAttackRange`.
///
/// Brain mobs that shoot (piglins with crossbows) measure their weapon's
/// range instead; no brain mob ported so far holds one.
fn is_within_attack_range(body: &(impl Mob + ?Sized), target: &dyn LivingEntity) -> bool {
    body.is_within_melee_attack_range(target)
}

/// Vanilla `SetWalkTargetFromAttackTargetIfTargetOutOfReach`: chase the attack
/// target until it is in reach.
pub fn set_walk_target_from_attack_target_if_target_out_of_reach<E: Mob + ?Sized>(
    speed_modifier: impl Fn(&E) -> f32 + Send + 'static,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let (target, visible) = {
                let memories = memories.lock();
                let Some(target) = memories.get(&ATTACK_TARGET) else {
                    return false;
                };
                (target, memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES))
            };
            let in_reach = visible.is_some_and(|visible| visible.contains(target.id()))
                && target
                    .as_living_entity()
                    .is_some_and(|living| is_within_attack_range(body, living));
            let speed = speed_modifier(body);
            let mut memories = memories.lock();
            if in_reach {
                memories.erase(&WALK_TARGET);
            } else {
                memories.set(
                    &LOOK_TARGET,
                    Some(PositionTracker::entity(Arc::clone(&target), true)),
                );
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::entity(target, false),
                        speed,
                        0,
                    )),
                );
            }
            true
        },
    )
}

/// Vanilla `MeleeAttack`: hit the attack target when it is visible and in
/// reach, then cool down for `cooldown_between_attacks` ticks.
pub fn melee_attack<E: Mob + ?Sized>(
    can_attack: impl Fn(&E) -> bool + Send + 'static,
    cooldown_between_attacks: i64,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&ATTACK_COOLING_DOWN, MemoryStatus::Registered),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let (target, visible) = {
                let memories = memories.lock();
                if memories.has_value(&ATTACK_COOLING_DOWN) {
                    return false;
                }
                let (Some(target), Some(visible)) = (
                    memories.get(&ATTACK_TARGET),
                    memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES),
                ) else {
                    return false;
                };
                (target, visible)
            };
            if !can_attack(body)
                || !target
                    .as_living_entity()
                    .is_some_and(|living| body.is_within_melee_attack_range(living))
                || !visible.contains(target.id())
            {
                return false;
            }
            let Some(body_entity) = world.get_entity_by_id(body.id()) else {
                return false;
            };
            memories.lock().set(
                &LOOK_TARGET,
                Some(PositionTracker::entity(Arc::clone(&target), true)),
            );
            body.swing_for_attack(InteractionHand::MainHand);
            let _ = body.do_hurt_target(&body_entity, world, &target);
            memories
                .lock()
                .set_with_expiry(&ATTACK_COOLING_DOWN, true, cooldown_between_attacks);
            true
        },
    )
}

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
