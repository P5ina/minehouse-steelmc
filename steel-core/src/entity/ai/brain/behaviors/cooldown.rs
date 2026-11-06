use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::world::World;

/// Counts a cooldown memory down to zero, then erases it
/// (vanilla `CountDownCooldownTicks`).
pub struct CountDownCooldownTicks {
    cooldown_ticks: &'static MemoryModuleType<i32>,
    entry_condition: [MemoryRequirement; 1],
}

impl CountDownCooldownTicks {
    #[must_use]
    pub fn new(cooldown_ticks: &'static MemoryModuleType<i32>) -> Self {
        Self {
            cooldown_ticks,
            entry_condition: [MemoryRequirement::new(
                cooldown_ticks,
                MemoryStatus::ValuePresent,
            )],
        }
    }

    fn cooldown(&self, body: &(impl LivingEntity + ?Sized)) -> Option<i32> {
        body.brain_memories()?.lock().get(self.cooldown_ticks)
    }
}

impl<E: LivingEntity + ?Sized> Behavior<E> for CountDownCooldownTicks {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &self.entry_condition
    }

    fn timed_out(&self, _time: i64, _end_time: i64) -> bool {
        false
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        self.cooldown(body).is_some_and(|ticks| ticks > 0)
    }

    fn tick(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let mut memories = memories.lock();
        if let Some(ticks) = memories.get(self.cooldown_ticks) {
            memories.set(self.cooldown_ticks, Some(ticks - 1));
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            memories.lock().erase(self.cooldown_ticks);
        }
    }
}
