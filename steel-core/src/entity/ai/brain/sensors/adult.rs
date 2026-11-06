use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::memory_types::{
    NEAREST_VISIBLE_ADULT, NEAREST_VISIBLE_LIVING_ENTITIES,
};
use crate::entity::ai::brain::{Memories, Sensor};
use crate::world::World;

/// Remembers the closest visible adult of the body's own type
/// (vanilla `AdultSensor`).
pub struct AdultSensor;

impl<E: LivingEntity> Sensor<E> for AdultSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_VISIBLE_ADULT);
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let Some(visible) = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES) else {
            return;
        };
        let entity_type = body.entity_type();
        let adult = visible.find_closest(|entity| {
            entity.entity_type() == entity_type
                && entity
                    .as_living_entity()
                    .is_some_and(|living| !living.is_baby())
        });
        memories.lock().set(&NEAREST_VISIBLE_ADULT, adult);
    }
}
