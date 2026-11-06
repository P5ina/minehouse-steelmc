//! Vanilla `BehaviorUtils` and `PositionTracker.isVisibleBy`.

use crate::entity::ai::brain::memory_types::{
    LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::{LivingEntity, SharedEntity};
use std::sync::Arc;

/// Vanilla `BehaviorUtils.entityIsVisible`: whether `target` is among the
/// body's nearest visible living entities.
pub fn entity_is_visible(body: &(impl LivingEntity + ?Sized), target: &SharedEntity) -> bool {
    let Some(visible) = body
        .brain_memories()
        .and_then(|memories| memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES))
    else {
        return false;
    };
    visible.contains(target.id())
}

/// Vanilla `PositionTracker.isVisibleBy`: blocks are always visible, a
/// living entity only while alive and seen by the body's brain.
pub fn is_visible_by(tracker: &PositionTracker, body: &(impl LivingEntity + ?Sized)) -> bool {
    let Some(entity) = tracker.tracked_entity() else {
        return true;
    };
    if !entity.is_living_entity() {
        return true;
    }
    entity.is_alive() && entity_is_visible(body, entity)
}

/// Vanilla `BehaviorUtils.lookAtEntity`.
pub fn look_at_entity(looker: &(impl LivingEntity + ?Sized), target: &SharedEntity) {
    if let Some(memories) = looker.brain_memories() {
        memories.lock().set(
            &LOOK_TARGET,
            Some(PositionTracker::entity(Arc::clone(target), true)),
        );
    }
}

/// Vanilla `BehaviorUtils.setWalkAndLookTargetMemories`.
pub fn set_walk_and_look_target_memories(
    walker: &(impl LivingEntity + ?Sized),
    target: PositionTracker,
    speed_modifier: f32,
    close_enough_dist: i32,
) {
    let Some(memories) = walker.brain_memories() else {
        return;
    };
    let mut memories = memories.lock();
    memories.set(&LOOK_TARGET, Some(target.clone()));
    memories.set(
        &WALK_TARGET,
        Some(WalkTarget::new(target, speed_modifier, close_enough_dist)),
    );
}

/// Vanilla `BehaviorUtils.lockGazeAndWalkToEachOther`.
pub fn lock_gaze_and_walk_to_each_other(
    first: &SharedEntity,
    second: &SharedEntity,
    speed_modifier: f32,
    close_enough_dist: i32,
) {
    let (Some(first_living), Some(second_living)) =
        (first.as_living_entity(), second.as_living_entity())
    else {
        return;
    };
    look_at_entity(first_living, second);
    look_at_entity(second_living, first);
    set_walk_and_look_target_memories(
        first_living,
        PositionTracker::entity(Arc::clone(second), true),
        speed_modifier,
        close_enough_dist,
    );
    set_walk_and_look_target_memories(
        second_living,
        PositionTracker::entity(Arc::clone(first), true),
        speed_modifier,
        close_enough_dist,
    );
}
