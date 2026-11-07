use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::memory_types::IS_IN_WATER;
use crate::entity::ai::brain::{Memories, Sensor};
use crate::world::World;

/// Remembers whether the body is in water (vanilla `IsInWaterSensor`).
pub struct IsInWaterSensor;

impl<E: LivingEntity + ?Sized> Sensor<E> for IsInWaterSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&IS_IN_WATER);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let in_water = body.is_in_water().then_some(());
        memories.lock().set(&IS_IN_WATER, in_water);
    }
}
