//! Picking something nearby to walk up to or interact with (vanilla
//! `InteractWith` and `SetLookAndInteract`).

use std::sync::Arc;

use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    INTERACTION_TARGET, LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker, WALK_TARGET,
    WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::{LivingEntity, SharedEntity};
use crate::world::World;

/// Walks up to the closest visible entity passing `target_filter` within
/// `interaction_range`, remembering it in `interaction_target`, while
/// `self_filter` holds (vanilla `InteractWith.of`).
pub fn interact_with<E: LivingEntity + ?Sized>(
    interaction_range: i32,
    self_filter: impl Fn(&E) -> bool + Send + 'static,
    target_filter: impl Fn(&SharedEntity) -> bool + Send + 'static,
    interaction_target: &'static MemoryModuleType<SharedEntity>,
    speed_modifier: f32,
    stop_distance: i32,
) -> impl BehaviorControl<E> {
    let range_sqr = f64::from(interaction_range * interaction_range);
    OneShot::new(
        vec![
            MemoryRequirement::new(interaction_target, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let visible = {
                let memories = memories.lock();
                if memories.has_value(&WALK_TARGET) {
                    return false;
                }
                let Some(visible) = memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES) else {
                    return false;
                };
                visible
            };
            if !self_filter(body) || !visible.contains_matching(&target_filter) {
                return false;
            }
            let position = body.position();
            if let Some(target) = visible.find_closest(|entity| {
                entity.position().distance_squared(position) <= range_sqr && target_filter(entity)
            }) {
                let mut memories = memories.lock();
                memories.set(interaction_target, Some(Arc::clone(&target)));
                memories.set(
                    &LOOK_TARGET,
                    Some(PositionTracker::entity(Arc::clone(&target), true)),
                );
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::entity(target, false),
                        speed_modifier,
                        stop_distance,
                    )),
                );
            }
            true
        },
    )
}

/// Vanilla `InteractWith.of(type, ...)` without self or target filters
/// beyond the entity type test `is_type`.
pub fn interact_with_type<E: LivingEntity + ?Sized>(
    is_type: impl Fn(&SharedEntity) -> bool + Send + 'static,
    interaction_range: i32,
    interaction_target: &'static MemoryModuleType<SharedEntity>,
    speed_modifier: f32,
    stop_distance: i32,
) -> impl BehaviorControl<E> {
    interact_with(
        interaction_range,
        |_: &E| true,
        is_type,
        interaction_target,
        speed_modifier,
        stop_distance,
    )
}

/// Looks at the closest visible entity passing `is_type` within
/// `interaction_range` and remembers it as the interaction target
/// (vanilla `SetLookAndInteract`).
pub fn set_look_and_interact<E: LivingEntity + ?Sized>(
    is_type: impl Fn(&SharedEntity) -> bool + Send + 'static,
    interaction_range: i32,
) -> impl BehaviorControl<E> {
    let range_sqr = f64::from(interaction_range * interaction_range);
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let mut memories = memories.lock();
            if memories.has_value(&INTERACTION_TARGET) {
                return false;
            }
            let Some(visible) = memories.get_ref(&NEAREST_VISIBLE_LIVING_ENTITIES) else {
                return false;
            };
            let position = body.position();
            let Some(target) = visible.find_closest(|entity| {
                entity.position().distance_squared(position) <= range_sqr && is_type(entity)
            }) else {
                return false;
            };
            memories.set(&INTERACTION_TARGET, Some(Arc::clone(&target)));
            memories.set(&LOOK_TARGET, Some(PositionTracker::entity(target, true)));
            true
        },
    )
}
