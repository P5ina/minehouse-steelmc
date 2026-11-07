//! Brain behaviors that only touch memories (vanilla
//! `BecomePassiveIfMemoryPresent` and `EraseMemoryIf`).

use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{ATTACK_TARGET, PACIFIED};
use crate::entity::ai::brain::{AnyMemoryModuleType, BehaviorControl, MemoryStatus};
use crate::world::World;

/// Vanilla `BecomePassiveIfMemoryPresent`: while `pacifying_memory` holds a
/// value, drop the attack target and stay pacified for `pacify_duration`
/// ticks.
pub fn become_passive_if_memory_present<E: LivingEntity + ?Sized>(
    pacifying_memory: &'static dyn AnyMemoryModuleType,
    pacify_duration: i64,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&PACIFIED, MemoryStatus::Registered),
            MemoryRequirement::new(pacifying_memory, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let mut memories = memories.lock();
            if memories.has_value(&PACIFIED) || !memories.has_value(pacifying_memory) {
                return false;
            }
            memories.set_with_expiry(&PACIFIED, true, pacify_duration);
            memories.erase(&ATTACK_TARGET);
            true
        },
    )
}

/// Vanilla `EraseMemoryIf`: forget `memory` once `predicate` holds.
pub fn erase_memory_if<E: LivingEntity + ?Sized>(
    predicate: impl Fn(&E) -> bool + Send + 'static,
    memory: &'static dyn AnyMemoryModuleType,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(memory, MemoryStatus::Registered)],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if !memories.lock().has_value(memory) || !predicate(body) {
                return false;
            }
            memories.lock().erase(memory);
            true
        },
    )
}
