use std::sync::Arc;

use crate::entity::ai::brain::memory_types::NEAREST_VISIBLE_WANTED_ITEM;
use crate::entity::ai::brain::{Memories, Sensor};
use crate::entity::entities::ItemEntity;
use steel_utils::Downcast as _;

use crate::entity::{Mob, SharedEntity};
use crate::world::World;

/// Vanilla `NearestItemSensor.XZ_RANGE`.
const XZ_RANGE: f64 = 32.0;
/// Vanilla `NearestItemSensor.Y_RANGE`.
const Y_RANGE: f64 = 16.0;
/// Vanilla `NearestItemSensor.MAX_DISTANCE_TO_WANTED_ITEM`.
const MAX_DISTANCE_TO_WANTED_ITEM: f64 = 32.0;

/// Remembers the closest visible item the body wants to pick up
/// (vanilla `NearestItemSensor`).
pub struct NearestItemSensor;

impl<E: Mob> Sensor<E> for NearestItemSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_VISIBLE_WANTED_ITEM);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let area = body.bounding_box().inflate_xyz(XZ_RANGE, Y_RANGE, XZ_RANGE);
        let position = body.position();
        let mut items: Vec<(SharedEntity, f64)> = world
            .get_entities_in_aabb_matching(&area, |entity| {
                entity.downcast_ref::<ItemEntity>().is_some()
            })
            .into_iter()
            .map(|entity| {
                let distance = entity.position().distance_squared(position);
                (entity, distance)
            })
            .collect();
        items.sort_by(|a, b| a.1.total_cmp(&b.1));
        let wanted = items.into_iter().find_map(|(entity, distance)| {
            let item = entity.downcast_ref::<ItemEntity>()?.get_item();
            (body.wants_to_pick_up(&item)
                && distance < MAX_DISTANCE_TO_WANTED_ITEM * MAX_DISTANCE_TO_WANTED_ITEM
                && body.has_line_of_sight(entity.as_ref()))
            .then_some(entity)
        });
        memories.lock().set(&NEAREST_VISIBLE_WANTED_ITEM, wanted);
    }
}
