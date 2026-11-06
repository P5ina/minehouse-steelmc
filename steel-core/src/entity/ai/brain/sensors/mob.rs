use std::sync::Arc;

use crate::entity::ai::brain::memory::MemoryModuleType;
use crate::entity::ai::brain::memory_types::NEAREST_LIVING_ENTITIES;
use crate::entity::ai::brain::{Memories, Sensor};
use crate::entity::{LivingEntity, SharedEntity};
use crate::world::World;

type MobTest<E> = Box<dyn Fn(&E, &SharedEntity) -> bool + Send>;
type ReadyTest<E> = Box<dyn Fn(&E) -> bool + Send>;

/// Sets a flag memory for a while when a nearby living entity passes a test
/// (vanilla `MobSensor`).
pub struct MobSensor<E: ?Sized> {
    scan_rate: i32,
    mob_test: MobTest<E>,
    ready_test: ReadyTest<E>,
    to_set: &'static MemoryModuleType<bool>,
    memory_time_to_live: i64,
}

impl<E: ?Sized> MobSensor<E> {
    #[must_use]
    pub fn new(
        scan_rate: i32,
        mob_test: impl Fn(&E, &SharedEntity) -> bool + Send + 'static,
        ready_test: impl Fn(&E) -> bool + Send + 'static,
        to_set: &'static MemoryModuleType<bool>,
        memory_time_to_live: i64,
    ) -> Self {
        Self {
            scan_rate,
            mob_test: Box::new(mob_test),
            ready_test: Box::new(ready_test),
            to_set,
            memory_time_to_live,
        }
    }
}

impl<E: LivingEntity> Sensor<E> for MobSensor<E> {
    fn scan_rate(&self) -> i32 {
        self.scan_rate
    }

    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_LIVING_ENTITIES);
        memories.register(self.to_set);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        if !(self.ready_test)(body) {
            memories.lock().erase(self.to_set);
            return;
        }
        let Some(nearby) = memories.lock().get(&NEAREST_LIVING_ENTITIES) else {
            return;
        };
        if nearby.iter().any(|entity| (self.mob_test)(body, entity)) {
            memories
                .lock()
                .set_with_expiry(self.to_set, true, self.memory_time_to_live);
        }
    }
}
