//! Villagers having a baby: vanilla `VillagerMakeLove`.

use std::sync::Arc;

use steel_registry::{vanilla_entities, vanilla_villager_professions};
use steel_utils::entity_events::EntityStatus;
use steel_utils::{BlockPos, Downcast as _, GlobalPos};

use super::super::super::villager_data::villager_type_at;
use super::super::VillagerEntity;
use super::poi::is_home;
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::behaviors::utils::{
    entity_is_visible, lock_gaze_and_walk_to_each_other,
};
use crate::entity::ai::brain::behaviors::valid_range;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{BREED_TARGET, HOME, NEAREST_VISIBLE_LIVING_ENTITIES};
use crate::entity::{
    AgeableMob as _, Entity as _, PathfinderMob as _, SharedEntity, next_entity_id,
};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `VillagerMakeLove`: how close the partner must be, squared.
const BREED_DISTANCE_SQR: f64 = 5.0;
/// Vanilla `VillagerMakeLove.takeVacantBed`: how far a free bed may be.
const BED_SEARCH_RANGE: i32 = 48;

const VILLAGER_MAKE_LOVE_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
];

/// Vanilla `VillagerMakeLove`: two villagers who can breed stand together
/// for 275 to 324 ticks, then have a baby if a bed is free for it.
pub(super) struct VillagerMakeLove {
    birth_timestamp: i64,
}

impl VillagerMakeLove {
    pub(super) const fn new() -> Self {
        Self { birth_timestamp: 0 }
    }
}

/// Whether `entity` is a villager that can breed (vanilla `AgeableMob::canBreed`
/// as used by `InteractWith` and `VillagerMakeLove`).
pub(super) fn villager_can_breed(entity: &SharedEntity) -> bool {
    entity
        .downcast_ref::<VillagerEntity>()
        .is_some_and(VillagerEntity::can_breed)
}

/// The partner, if it is a living, visible villager.
fn breed_partner(body: &VillagerEntity) -> Option<SharedEntity> {
    let target = body.brain.memories().lock().get(&BREED_TARGET)?;
    (target.entity_type() == &vanilla_entities::VILLAGER
        && target.is_alive()
        && entity_is_visible(body, &target))
    .then_some(target)
}

/// Vanilla `isBreedingPossible`.
fn is_breeding_possible(body: &VillagerEntity) -> bool {
    breed_partner(body).is_some_and(|partner| body.can_breed() && villager_can_breed(&partner))
}

/// Vanilla `takeVacantBed`: claims the first free bed within 48 blocks that
/// the villager can walk to.
fn take_vacant_bed(world: &Arc<World>, body: &VillagerEntity) -> Option<BlockPos> {
    let candidates = world.poi_storage.lock().get_in_range(
        &is_home,
        body.block_position(),
        BED_SEARCH_RANGE,
        OccupationStatus::Free,
    );
    let (pos, _) = candidates.into_iter().find(|(pos, poi_type)| {
        body.create_path_to(*pos, valid_range(*poi_type))
            .is_some_and(|path| path.can_reach())
    })?;
    world.poi_storage.lock().reserve_ticket(pos).then_some(pos)
}

/// Vanilla `Villager.getBreedOffspring` and `VillagerMakeLove.breed`.
fn breed(
    world: &Arc<World>,
    source: &VillagerEntity,
    target: &VillagerEntity,
) -> Option<SharedEntity> {
    let biome_roll: f64 = rand::random();
    let villager_type = if biome_roll < 0.5 {
        villager_type_at(world, source.block_position())
    } else if biome_roll < 0.75 {
        source.villager_type()
    } else {
        target.villager_type()
    };
    let child = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        source.position(),
        Arc::downgrade(world),
    );
    child.set_villager_type(villager_type);
    child.set_profession(&vanilla_villager_professions::NONE);
    child.set_villager_data_finalized(true);
    source.set_age(6000);
    target.set_age(6000);
    child.set_age(-24_000);
    child.set_rotation((0.0, 0.0));
    child.set_old_position_to_current();
    let child: SharedEntity = Arc::new(child);
    if let Err(error) = world.try_add_entity(Arc::clone(&child)) {
        log::error!("failed to add a villager baby to the world: {error}");
        return None;
    }
    child.broadcast_entity_event(EntityStatus::LoveHearts);
    Some(child)
}

impl Behavior<VillagerEntity> for VillagerMakeLove {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &VILLAGER_MAKE_LOVE_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (350, 350)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        is_breeding_possible(body)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) -> bool {
        time <= self.birth_timestamp && is_breeding_possible(body)
    }

    fn start(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        let Some(target) = body.brain.memories().lock().get(&BREED_TARGET) else {
            return;
        };
        if let Some(body_entity) = world.get_entity_by_id(body.id()) {
            lock_gaze_and_walk_to_each_other(&body_entity, &target, 0.5, 2);
        }
        target.broadcast_entity_event(EntityStatus::InLoveHearts);
        body.broadcast_entity_event(EntityStatus::InLoveHearts);
        self.birth_timestamp = time + 275 + i64::from(rand::random_range(0..50));
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        let Some(target_entity) = body.brain.memories().lock().get(&BREED_TARGET) else {
            return;
        };
        let Some(target) = target_entity.downcast_ref::<VillagerEntity>() else {
            return;
        };
        if body.position().distance_squared(target.position()) > BREED_DISTANCE_SQR {
            return;
        }
        if let Some(body_entity) = world.get_entity_by_id(body.id()) {
            lock_gaze_and_walk_to_each_other(&body_entity, &target_entity, 0.5, 2);
        }
        if time >= self.birth_timestamp {
            body.eat_and_digest_food();
            target.eat_and_digest_food();
            try_to_give_birth(world, body, &target_entity, target);
        } else if rand::random_range(0..35) == 0 {
            target_entity.broadcast_entity_event(EntityStatus::LoveHearts);
            body.broadcast_entity_event(EntityStatus::LoveHearts);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        body.brain.memories().lock().erase(&BREED_TARGET);
    }
}

/// Vanilla `tryToGiveBirth`: without a free bed both get angry.
fn try_to_give_birth(
    world: &Arc<World>,
    body: &VillagerEntity,
    target_entity: &SharedEntity,
    target: &VillagerEntity,
) {
    let Some(bed) = take_vacant_bed(world, body) else {
        target_entity.broadcast_entity_event(EntityStatus::VillagerAngry);
        body.broadcast_entity_event(EntityStatus::VillagerAngry);
        return;
    };
    match breed(world, body, target) {
        Some(child) => {
            if let Some(child) = child.downcast_ref::<VillagerEntity>() {
                child
                    .brain
                    .memories()
                    .lock()
                    .set(&HOME, Some(GlobalPos::new(world.key.clone(), bed)));
            }
        }
        None => {
            let _released = world.poi_storage.lock().release_ticket(bed);
        }
    }
}
