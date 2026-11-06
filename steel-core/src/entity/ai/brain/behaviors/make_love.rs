use std::sync::Arc;

use steel_registry::entity_type::EntityTypeRef;

use super::utils::{entity_is_visible, lock_gaze_and_walk_to_each_other};
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, IS_PANICKING, LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, WALK_TARGET,
};
use crate::entity::{Animal, LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

const BREED_RANGE: f64 = 3.0;
const MIN_DURATION: i32 = 60;
const MAX_DURATION: i32 = 110;

const MAKE_LOVE_ENTRY: [MemoryRequirement; 5] = [
    MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
];

/// Walks to a visible partner in love and breeds (vanilla `AnimalMakeLove`).
pub struct AnimalMakeLove {
    partner_type: EntityTypeRef,
    speed_modifier: f32,
    close_enough_distance: i32,
    spawn_child_at_time: i64,
}

impl AnimalMakeLove {
    #[must_use]
    pub const fn new(partner_type: EntityTypeRef) -> Self {
        Self::with_speed(partner_type, 1.0, 2)
    }

    #[must_use]
    pub const fn with_speed(
        partner_type: EntityTypeRef,
        speed_modifier: f32,
        close_enough_distance: i32,
    ) -> Self {
        Self {
            partner_type,
            speed_modifier,
            close_enough_distance,
            spawn_child_at_time: 0,
        }
    }

    fn find_valid_breed_partner(&self, body: &(impl Animal + ?Sized)) -> Option<SharedEntity> {
        // The memories lock must be released before searching: visibility
        // checks read the body's attack target.
        let visible = body
            .brain_memories()?
            .lock()
            .get(&NEAREST_VISIBLE_LIVING_ENTITIES)?;
        visible.find_closest(|entity| {
            entity.entity_type() == self.partner_type
                && entity
                    .as_animal()
                    .is_some_and(|animal| body.can_mate(animal) && !entity_is_panicking(entity))
        })
    }

    fn breed_target(&self, body: &(impl LivingEntity + ?Sized)) -> Option<SharedEntity> {
        body.brain_memories()?
            .lock()
            .get(&BREED_TARGET)
            .filter(|partner| partner.entity_type() == self.partner_type)
    }
}

fn entity_is_panicking(entity: &SharedEntity) -> bool {
    entity
        .as_pathfinder_mob()
        .is_some_and(PathfinderMob::is_panicking)
}

impl<E: Animal + PathfinderMob> Behavior<E> for AnimalMakeLove {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &MAKE_LOVE_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (MAX_DURATION, MAX_DURATION)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        body.is_in_love() && self.find_valid_breed_partner(body).is_some()
    }

    fn start(&mut self, world: &Arc<World>, body: &E, time: i64) {
        let Some(partner) = self.find_valid_breed_partner(body) else {
            return;
        };
        let Some(body_entity) = world.get_entity_by_id(body.id()) else {
            return;
        };
        if let Some(memories) = body.brain_memories() {
            memories
                .lock()
                .set(&BREED_TARGET, Some(Arc::clone(&partner)));
        }
        if let Some(memories) = partner
            .as_living_entity()
            .and_then(LivingEntity::brain_memories)
        {
            memories
                .lock()
                .set(&BREED_TARGET, Some(Arc::clone(&body_entity)));
        }
        lock_gaze_and_walk_to_each_other(
            &body_entity,
            &partner,
            self.speed_modifier,
            self.close_enough_distance,
        );
        let duration = MIN_DURATION + rand::random_range(0..MAX_DURATION - MIN_DURATION);
        self.spawn_child_at_time = time + i64::from(duration);
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, time: i64) -> bool {
        let Some(partner) = self.breed_target(body) else {
            return false;
        };
        partner.is_alive()
            && partner
                .as_animal()
                .is_some_and(|animal| body.can_mate(animal))
            && entity_is_visible(body, &partner)
            && time <= self.spawn_child_at_time
            && !body.is_panicking()
            && !entity_is_panicking(&partner)
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        let Some(partner) = self.breed_target(body) else {
            return;
        };
        let Some(body_entity) = world.get_entity_by_id(body.id()) else {
            return;
        };
        lock_gaze_and_walk_to_each_other(
            &body_entity,
            &partner,
            self.speed_modifier,
            self.close_enough_distance,
        );
        if body.position().distance_squared(partner.position()) >= BREED_RANGE * BREED_RANGE
            || time < self.spawn_child_at_time
        {
            return;
        }
        let Some(partner_animal) = partner.as_animal() else {
            return;
        };
        body.spawn_child_from_breeding(world, partner_animal);
        if let Some(memories) = body.brain_memories() {
            memories.lock().erase(&BREED_TARGET);
        }
        if let Some(memories) = partner_animal.brain_memories() {
            memories.lock().erase(&BREED_TARGET);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.erase(&BREED_TARGET);
            memories.erase(&WALK_TARGET);
            memories.erase(&LOOK_TARGET);
        }
        self.spawn_child_at_time = 0;
    }
}
