//! Villager job sites, beds and meeting points (vanilla
//! `VillagerProfession.heldJobSite` / `acquirableJobSite`,
//! `PoiCompetitorScan`, `YieldJobSite`, `GoToPotentialJobSite`,
//! `AssignProfessionFromJobSite` and `ResetProfession`).

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use rustc_hash::FxHashMap;
use steel_registry::vanilla_poi_type_tags::PoiTag;
use steel_registry::vanilla_poi_types;
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::{
    REGISTRY, RegistryEntry as _, RegistryExt as _, TaggedRegistryExt as _,
    vanilla_villager_professions,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::{BlockPos, Downcast as _, GlobalPos};

use super::VillagerEntity;
use crate::entity::ai::brain::behavior::{Behavior, OneShot};
use crate::entity::ai::brain::behaviors::utils::set_walk_and_look_target_memories;
use crate::entity::ai::brain::behaviors::{SCAN_RANGE, find_path_to_pois};
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    CANT_REACH_WALK_TARGET_SINCE, HOME, JOB_SITE, LOOK_TARGET, NEAREST_LIVING_ENTITIES,
    POTENTIAL_JOB_SITE, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{Activity, BehaviorControl, MemoryStatus};
use crate::entity::ai::goal::random_pos::default_random_pos_towards;
use crate::entity::ai::path::Path;
use crate::entity::{AgeableMob, Entity, LivingEntity, PathfinderMob as _, SharedEntity};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Whether `profession` is the "no job site" kind (vanilla `PoiType.NONE`
/// as its held job site).
fn has_no_job_site(profession: VillagerProfessionRef) -> bool {
    profession == &vanilla_villager_professions::NONE
        || profession == &vanilla_villager_professions::NITWIT
}

/// Vanilla `VillagerProfession.heldJobSite`: every job profession keeps the
/// workstation POI of the same name; `none` and `nitwit` hold none.
#[must_use]
pub fn held_job_site(profession: VillagerProfessionRef, poi_type: usize) -> bool {
    !has_no_job_site(profession)
        && REGISTRY
            .poi_types
            .by_id(poi_type)
            .is_some_and(|poi_type| poi_type.key == profession.key)
}

/// Vanilla `VillagerProfession.ALL_ACQUIRABLE_JOBS`.
#[must_use]
pub fn is_acquirable_job(poi_type: usize) -> bool {
    REGISTRY.poi_types.by_id(poi_type).is_some_and(|poi_type| {
        REGISTRY
            .poi_types
            .is_in_tag(poi_type, &PoiTag::ACQUIRABLE_JOB_SITE)
    })
}

/// Vanilla `VillagerProfession.acquirableJobSite`: an unemployed villager
/// takes any job site, a nitwit none, everyone else only its own.
#[must_use]
pub fn acquirable_job_site(profession: VillagerProfessionRef, poi_type: usize) -> bool {
    if profession == &vanilla_villager_professions::NONE {
        is_acquirable_job(poi_type)
    } else {
        held_job_site(profession, poi_type)
    }
}

/// The first registered profession that holds `poi_type` (vanilla
/// `BuiltInRegistries.VILLAGER_PROFESSION.listElements().filter(...)`).
fn profession_for_job_site(poi_type: usize) -> Option<VillagerProfessionRef> {
    REGISTRY
        .villager_professions
        .iter()
        .map(|(_, profession)| profession)
        .find(|profession| held_job_site(profession, poi_type))
}

/// The other living villagers among the body's nearest living entities.
fn nearby_villagers(body: &VillagerEntity) -> Vec<SharedEntity> {
    let Some(nearest) = body.brain.memories().lock().get(&NEAREST_LIVING_ENTITIES) else {
        return Vec::new();
    };
    nearest
        .into_iter()
        .filter(|entity| {
            entity.id() != body.id()
                && as_villager(entity).is_some()
                && entity
                    .as_living_entity()
                    .is_some_and(LivingEntity::is_alive)
        })
        .collect()
}

fn as_villager(entity: &SharedEntity) -> Option<&VillagerEntity> {
    let entity: &dyn Entity = &**entity;
    entity.downcast_ref::<VillagerEntity>()
}

/// Whether `pos`'s center is closer than `distance` to the body (vanilla
/// `Vec3i.closerToCenterThan`).
pub(super) fn closer_to_center_than(pos: BlockPos, body: &VillagerEntity, distance: f64) -> bool {
    let (x, y, z) = pos.get_center();
    glam::DVec3::new(x, y, z).distance_squared(body.position()) < distance * distance
}

/// Vanilla `PoiCompetitorScan`: of the villagers that hold this job site,
/// the most experienced keeps it and the others forget it.
pub(super) fn poi_competitor_scan() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&JOB_SITE, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&NEAREST_LIVING_ENTITIES, MemoryStatus::ValuePresent),
        ],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let Some(job_site) = body.brain.memories().lock().get(&JOB_SITE) else {
                return false;
            };
            if !body
                .brain
                .memories()
                .lock()
                .has_value(&NEAREST_LIVING_ENTITIES)
            {
                return false;
            }
            let Some(poi_type) = world.poi_storage.lock().get_type(job_site.pos) else {
                return true;
            };
            let competitors = nearby_villagers(body);
            let mut winner = body;
            for competitor in &competitors {
                let Some(competitor) = as_villager(competitor) else {
                    continue;
                };
                let competes = competitor.brain.memories().lock().get(&JOB_SITE).as_ref()
                    == Some(&job_site)
                    && held_job_site(competitor.profession(), poi_type);
                if !competes {
                    continue;
                }
                // Vanilla `selectWinner`: a tie goes to the newcomer.
                let (new_winner, loser) = if winner.villager_xp() > competitor.villager_xp() {
                    (winner, competitor)
                } else {
                    (competitor, winner)
                };
                loser.brain.memories().lock().erase(&JOB_SITE);
                winner = new_winner;
            }
            true
        },
    )
}

/// Vanilla `YieldJobSite.nearbyWantsJobsite`.
fn nearby_wants_job_site(poi_type: usize, nearby: &VillagerEntity, poi_pos: BlockPos) -> bool {
    let (has_potential, job_site) = {
        let memories = nearby.brain.memories().lock();
        (
            memories.has_value(&POTENTIAL_JOB_SITE),
            memories.get(&JOB_SITE),
        )
    };
    if has_potential || !held_job_site(nearby.profession(), poi_type) {
        return false;
    }
    match job_site {
        None => {
            let range = REGISTRY
                .poi_types
                .by_id(poi_type)
                .and_then(|poi_type| i32::try_from(poi_type.search_distance).ok())
                .unwrap_or(1);
            nearby
                .create_path_to(poi_pos, range)
                .is_some_and(|path| path.can_reach())
        }
        Some(job_site) => job_site.pos == poi_pos,
    }
}

/// Vanilla `YieldJobSite`: an unemployed villager hands the job site it
/// is walking to over to a nearby villager of that profession.
pub(super) fn yield_job_site(speed_modifier: f32) -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&POTENTIAL_JOB_SITE, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&JOB_SITE, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&NEAREST_LIVING_ENTITIES, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let potential = {
                let memories = body.brain.memories().lock();
                if memories.has_value(&JOB_SITE) || !memories.has_value(&NEAREST_LIVING_ENTITIES) {
                    return false;
                }
                let Some(potential) = memories.get(&POTENTIAL_JOB_SITE) else {
                    return false;
                };
                potential
            };
            if AgeableMob::is_baby(body) || body.profession() != &vanilla_villager_professions::NONE
            {
                return false;
            }
            let poi_pos = potential.pos;
            let Some(poi_type) = world.poi_storage.lock().get_type(poi_pos) else {
                return true;
            };
            let nearby = nearby_villagers(body);
            let taker = nearby
                .iter()
                .filter_map(as_villager)
                .find(|villager| nearby_wants_job_site(poi_type, villager, poi_pos));
            if let Some(taker) = taker {
                {
                    let mut memories = body.brain.memories().lock();
                    memories.erase(&WALK_TARGET);
                    memories.erase(&LOOK_TARGET);
                    memories.erase(&POTENTIAL_JOB_SITE);
                }
                if !taker.brain.memories().lock().has_value(&JOB_SITE) {
                    set_walk_and_look_target_memories(
                        taker,
                        PositionTracker::block(poi_pos),
                        speed_modifier,
                        1,
                    );
                    taker.brain.memories().lock().set(
                        &POTENTIAL_JOB_SITE,
                        Some(GlobalPos::new(world.key.clone(), poi_pos)),
                    );
                }
            }
            true
        },
    )
}

const GO_TO_POTENTIAL_JOB_SITE_ENTRY: [MemoryRequirement; 1] = [MemoryRequirement::new(
    &POTENTIAL_JOB_SITE,
    MemoryStatus::ValuePresent,
)];

/// Vanilla `GoToPotentialJobSite`: walks to the job site it wants for up
/// to a minute, and frees it when it gives up or takes it.
pub(super) struct GoToPotentialJobSite {
    pub(super) speed_modifier: f32,
}

impl Behavior<VillagerEntity> for GoToPotentialJobSite {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &GO_TO_POTENTIAL_JOB_SITE_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        // Vanilla `TICKS_UNTIL_TIMEOUT`.
        (1200, 1200)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        body.brain
            .active_non_core_activity()
            .is_none_or(|activity| {
                activity == Activity::IDLE
                    || activity == Activity::WORK
                    || activity == Activity::PLAY
            })
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        body.brain.memories().lock().has_value(&POTENTIAL_JOB_SITE)
    }

    fn tick(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        let Some(potential) = body.brain.memories().lock().get(&POTENTIAL_JOB_SITE) else {
            return;
        };
        set_walk_and_look_target_memories(
            body,
            PositionTracker::block(potential.pos),
            self.speed_modifier,
            1,
        );
    }

    fn stop(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        let potential = body.brain.memories().lock().get(&POTENTIAL_JOB_SITE);
        // Vanilla releases the POI in the job site's own dimension; it was
        // claimed in this one, which is the only POI storage a villager reaches.
        if let Some(potential) = potential
            && potential.dimension == world.key
        {
            let mut storage = world.poi_storage.lock();
            if storage.exists(potential.pos, |_| true) {
                let _released = storage.release_ticket(potential.pos);
            }
        }
        body.brain.memories().lock().erase(&POTENTIAL_JOB_SITE);
    }
}

/// Vanilla `AssignProfessionFromJobSite`: once at the job site it wanted,
/// keeps it and, if unemployed, takes up the profession that works there.
pub(super) fn assign_profession_from_job_site() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&POTENTIAL_JOB_SITE, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&JOB_SITE, MemoryStatus::Registered),
        ],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let Some(potential) = body.brain.memories().lock().get(&POTENTIAL_JOB_SITE) else {
                return false;
            };
            if !closer_to_center_than(potential.pos, body, 2.0) {
                return false;
            }
            {
                let mut memories = body.brain.memories().lock();
                memories.erase(&POTENTIAL_JOB_SITE);
                memories.set(&JOB_SITE, Some(potential.clone()));
            }
            body.broadcast_entity_event(EntityStatus::VillagerHappy);
            if body.profession() != &vanilla_villager_professions::NONE {
                return true;
            }
            // Vanilla reads the POI from the job site's dimension; the body
            // stands within two blocks of it, so that is its own world.
            if potential.dimension != world.key {
                return true;
            }
            let poi_type = world.poi_storage.lock().get_type(potential.pos);
            if let Some(profession) = poi_type.and_then(profession_for_job_site) {
                body.set_profession(profession);
                body.request_brain_refresh();
            }
            true
        },
    )
}

/// Vanilla `ResetProfession`: a villager that lost its job site before
/// ever trading forgets its profession.
pub(super) fn reset_profession() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![MemoryRequirement::new(&JOB_SITE, MemoryStatus::ValueAbsent)],
        |_world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if body.brain.memories().lock().has_value(&JOB_SITE) {
                return false;
            }
            let profession = body.profession();
            let can_be_fired = !has_no_job_site(profession);
            if can_be_fired && body.villager_xp() == 0 && body.villager_level() <= 1 {
                body.set_profession(&vanilla_villager_professions::NONE);
                body.request_brain_refresh();
                true
            } else {
                false
            }
        },
    )
}

/// Vanilla `Vec3i.distManhattan`.
const fn dist_manhattan(a: BlockPos, b: BlockPos) -> i32 {
    (a.x() - b.x()).abs() + (a.y() - b.y()).abs() + (a.z() - b.z()).abs()
}

/// Vanilla `SetWalkTargetFromBlockMemory`: walks towards the POI in
/// `memory`, a random hop at a time while farther than `too_far_distance`,
/// and gives it up when it is in another dimension, unreachable for longer
/// than `too_long_unreachable_duration`, or no hop gets closer.
pub(super) fn set_walk_target_from_block_memory(
    memory: &'static MemoryModuleType<GlobalPos>,
    speed_modifier: f32,
    close_enough_dist: i32,
    too_far_distance: i32,
    too_long_unreachable_duration: i64,
) -> impl BehaviorControl<VillagerEntity> {
    /// Vanilla `MAX_TRIES`.
    const MAX_TRIES: i32 = 1000;
    OneShot::new(
        vec![
            MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(memory, MemoryStatus::ValuePresent),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, time: i64| {
            let (target, cant_reach_since) = {
                let memories = body.brain.memories().lock();
                if memories.has_value(&WALK_TARGET) {
                    return false;
                }
                let Some(target) = memories.get(memory) else {
                    return false;
                };
                (target, memories.get(&CANT_REACH_WALK_TARGET_SINCE))
            };
            let reachable = cant_reach_since
                .is_none_or(|since| world.game_time() - since <= too_long_unreachable_duration);
            if target.dimension != world.key || !reachable {
                give_up_poi(body, memory, time);
                return true;
            }
            let body_pos = body.block_position();
            if dist_manhattan(target.pos, body_pos) > too_far_distance {
                let towards = glam::DVec3::new(
                    f64::from(target.pos.x()) + 0.5,
                    f64::from(target.pos.y()),
                    f64::from(target.pos.z()) + 0.5,
                );
                let mut tries = 0;
                let hop = loop {
                    let hop =
                        default_random_pos_towards(body, 15, 7, towards, f64::from(FRAC_PI_2));
                    tries += 1;
                    if tries == MAX_TRIES {
                        give_up_poi(body, memory, time);
                        return true;
                    }
                    if let Some(hop) = hop
                        && dist_manhattan(BlockPos::containing(hop.x, hop.y, hop.z), body_pos)
                            <= too_far_distance
                    {
                        break hop;
                    }
                };
                body.brain.memories().lock().set(
                    &WALK_TARGET,
                    Some(WalkTarget::to_point(hop, speed_modifier, close_enough_dist)),
                );
            } else if dist_manhattan(target.pos, body_pos) > close_enough_dist {
                body.brain.memories().lock().set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::block(target.pos),
                        speed_modifier,
                        close_enough_dist,
                    )),
                );
            }
            true
        },
    )
}

/// The failure branch of vanilla `SetWalkTargetFromBlockMemory`.
fn give_up_poi(body: &VillagerEntity, memory: &'static MemoryModuleType<GlobalPos>, time: i64) {
    body.release_poi(memory);
    let mut memories = body.brain.memories().lock();
    memories.erase(memory);
    memories.set(&CANT_REACH_WALK_TARGET_SINCE, Some(time));
}

/// Vanilla `SetClosestHomeAsWalkTarget`: a homeless villager walks to the
/// closest reachable bed, trying up to four new beds a second.
pub(super) fn set_closest_home_as_walk_target(
    speed_modifier: f32,
) -> impl BehaviorControl<VillagerEntity> {
    /// Vanilla `CACHE_TIMEOUT`.
    const CACHE_TIMEOUT: i64 = 40;
    /// Vanilla `BATCH_SIZE`.
    const BATCH_SIZE: i32 = 5;
    /// Vanilla `RATE`.
    const RATE: i64 = 20;
    /// Vanilla `OK_DISTANCE_SQR`.
    const OK_DISTANCE_SQR: i64 = 4;
    let mut batch_cache: FxHashMap<BlockPos, i64> = FxHashMap::default();
    let mut last_update = 0_i64;
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&HOME, MemoryStatus::ValueAbsent),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            {
                let memories = body.brain.memories().lock();
                if memories.has_value(&WALK_TARGET) || memories.has_value(&HOME) {
                    return false;
                }
            }
            if world.game_time() - last_update < RATE {
                return false;
            }
            let body_pos = body.block_position();
            let closest = world
                .poi_storage
                .lock()
                .find_all_closest_first(
                    &is_home,
                    |_| true,
                    body_pos,
                    SCAN_RANGE,
                    OccupationStatus::Any,
                )
                .first()
                .map(|(pos, _)| *pos);
            let Some(closest) = closest else {
                return false;
            };
            if block_dist_sqr(closest, body_pos) <= OK_DISTANCE_SQR {
                return false;
            }
            let mut tried_count = 0;
            last_update = world.game_time() + rand::random_range(0..RATE);
            let pois: Vec<(BlockPos, usize)> = world
                .poi_storage
                .lock()
                .get_in_range(&is_home, body_pos, SCAN_RANGE, OccupationStatus::Any)
                .into_iter()
                .filter(|(pos, _)| {
                    if batch_cache.contains_key(pos) {
                        return false;
                    }
                    tried_count += 1;
                    if tried_count >= BATCH_SIZE {
                        return false;
                    }
                    batch_cache.insert(*pos, last_update + CACHE_TIMEOUT);
                    true
                })
                .collect();
            let path = find_path_to_pois(world, body, &pois);
            if let Some(path) = path.filter(Path::can_reach) {
                let target = path.target();
                if world.poi_storage.lock().get_type(target).is_some() {
                    body.brain.memories().lock().set(
                        &WALK_TARGET,
                        Some(WalkTarget::new(
                            PositionTracker::block(target),
                            speed_modifier,
                            1,
                        )),
                    );
                }
            } else if tried_count < BATCH_SIZE {
                batch_cache.retain(|_, expiry| *expiry >= last_update);
            }
            true
        },
    )
}

/// Vanilla `p -> p.is(PoiTypes.HOME)`.
pub(in crate::entity::entities::mobs::villagers) fn is_home(poi_type: usize) -> bool {
    vanilla_poi_types::HOME.try_id() == Some(poi_type)
}

/// Vanilla `p -> p.is(PoiTypes.MEETING)`.
pub(in crate::entity::entities::mobs::villagers) fn is_meeting(poi_type: usize) -> bool {
    vanilla_poi_types::MEETING.try_id() == Some(poi_type)
}

/// Vanilla `Vec3i.distSqr`.
fn block_dist_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.x() - b.x());
    let dy = i64::from(a.y() - b.y());
    let dz = i64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}
