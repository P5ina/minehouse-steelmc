use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::memory_types::{HURT_BY, HURT_BY_ENTITY};
use crate::entity::ai::brain::{Memories, Sensor};
use crate::world::World;

/// Remembers the last damage and the living entity behind it
/// (vanilla `HurtBySensor`).
pub struct HurtBySensor;

impl<E: LivingEntity> Sensor<E> for HurtBySensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&HURT_BY);
        memories.register(&HURT_BY_ENTITY);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let damage_source = body.last_damage_source();
        let mut memories = memories.lock();
        if let Some(damage_source) = damage_source {
            let attacker = damage_source
                .causing_entity()
                .filter(|entity| entity.is_living_entity())
                .cloned();
            memories.set(&HURT_BY, Some(damage_source));
            if let Some(attacker) = attacker {
                memories.set(&HURT_BY_ENTITY, Some(attacker));
            }
        } else {
            memories.erase(&HURT_BY);
        }

        if memories.get_ref(&HURT_BY_ENTITY).is_some_and(|entity| {
            !entity.is_alive()
                || entity
                    .level()
                    .is_none_or(|level| !Arc::ptr_eq(&level, world))
        }) {
            memories.erase(&HURT_BY_ENTITY);
        }
    }
}
