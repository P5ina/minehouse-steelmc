//! Vanilla `HoglinSpecificSensor`.

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entities;

use super::HoglinEntity;
use crate::entity::Entity;
use crate::entity::ai::brain::memory_types::{
    NEAREST_REPELLENT, NEAREST_VISIBLE_ADULT_HOGLINS, NEAREST_VISIBLE_ADULT_PIGLIN,
    NEAREST_VISIBLE_LIVING_ENTITIES, VISIBLE_ADULT_HOGLIN_COUNT, VISIBLE_ADULT_PIGLIN_COUNT,
};
use crate::entity::ai::brain::{Memories, Sensor};
use crate::world::World;

/// Vanilla `HoglinAi.REPELLENT_DETECTION_RANGE_HORIZONTAL` and `_VERTICAL`.
const REPELLENT_DETECTION_RANGE_HORIZONTAL: i32 = 8;
const REPELLENT_DETECTION_RANGE_VERTICAL: i32 = 4;

/// Finds the nearest hoglin repellent and counts the adult piglins and
/// hoglins in sight.
pub(super) struct HoglinSpecificSensor;

impl Sensor<HoglinEntity> for HoglinSpecificSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
        memories.register(&NEAREST_REPELLENT);
        memories.register(&NEAREST_VISIBLE_ADULT_PIGLIN);
        memories.register(&NEAREST_VISIBLE_ADULT_HOGLINS);
        memories.register(&VISIBLE_ADULT_PIGLIN_COUNT);
        memories.register(&VISIBLE_ADULT_HOGLIN_COUNT);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &HoglinEntity) {
        let repellent = body
            .block_position()
            .within_manhattan(
                REPELLENT_DETECTION_RANGE_HORIZONTAL,
                REPELLENT_DETECTION_RANGE_VERTICAL,
                REPELLENT_DETECTION_RANGE_HORIZONTAL,
            )
            .find(|pos| {
                world
                    .get_block_state(*pos)
                    .get_block()
                    .has_tag(&BlockTag::HOGLIN_REPELLENTS)
            });

        let visible = body.memories().lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES);
        let mut adult_piglin = None;
        let mut adult_piglin_count = 0;
        let mut adult_hoglins = Vec::new();
        if let Some(visible) = visible {
            for entity in visible.find_all(|entity| {
                let entity_type = entity.entity_type();
                (entity_type == &vanilla_entities::PIGLIN
                    || entity_type == &vanilla_entities::HOGLIN)
                    && entity
                        .as_living_entity()
                        .is_some_and(|living| !living.is_baby())
            }) {
                if entity.entity_type() == &vanilla_entities::PIGLIN {
                    adult_piglin_count += 1;
                    if adult_piglin.is_none() {
                        adult_piglin = Some(Arc::clone(entity));
                    }
                } else {
                    adult_hoglins.push(Arc::clone(entity));
                }
            }
        }
        let adult_hoglin_count = i32::try_from(adult_hoglins.len()).unwrap_or(i32::MAX);

        let mut memories = body.memories().lock();
        memories.set(&NEAREST_REPELLENT, repellent);
        memories.set(&NEAREST_VISIBLE_ADULT_PIGLIN, adult_piglin);
        memories.set(&NEAREST_VISIBLE_ADULT_HOGLINS, Some(adult_hoglins));
        memories.set(&VISIBLE_ADULT_PIGLIN_COUNT, Some(adult_piglin_count));
        memories.set(&VISIBLE_ADULT_HOGLIN_COUNT, Some(adult_hoglin_count));
    }
}
