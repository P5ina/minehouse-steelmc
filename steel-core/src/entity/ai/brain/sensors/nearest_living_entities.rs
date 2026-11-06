use std::sync::Arc;

use steel_registry::vanilla_attributes;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::memory_types::{
    NEAREST_LIVING_ENTITIES, NEAREST_VISIBLE_LIVING_ENTITIES,
};
use crate::entity::ai::brain::nearest_visible::NearestVisibleLivingEntities;
use crate::entity::ai::brain::{Memories, Sensor};
use crate::world::World;

/// Remembers living entities within follow range, closest first
/// (vanilla `NearestLivingEntitySensor`).
pub struct NearestLivingEntitySensor;

impl<E: LivingEntity> Sensor<E> for NearestLivingEntitySensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_LIVING_ENTITIES);
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let Some(body_entity) = world.get_entity_by_id(body.id()) else {
            return;
        };
        let follow_range = body
            .attributes()
            .lock()
            .required_value(vanilla_attributes::FOLLOW_RANGE);
        let search = body.bounding_box().inflate(follow_range);
        let body_id = body.id();
        let mut entities = world.get_entities_in_aabb_matching(&search, |entity| {
            entity.id() != body_id && entity.is_living_entity() && entity.is_alive()
        });
        let position = body.position();
        entities.sort_by(|a, b| {
            a.position()
                .distance_squared(position)
                .total_cmp(&b.position().distance_squared(position))
        });

        let visible = NearestVisibleLivingEntities::new(world, &body_entity, entities.clone());
        let mut memories = memories.lock();
        memories.set(&NEAREST_LIVING_ENTITIES, Some(entities));
        memories.set(&NEAREST_VISIBLE_LIVING_ENTITIES, Some(visible));
    }
}
