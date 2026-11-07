//! Claiming and checking points of interest (vanilla `AcquirePoi` and
//! `ValidateNearbyPoi`).

use std::ptr;
use std::sync::Arc;

use rustc_hash::FxHashMap;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{REGISTRY, RegistryExt as _, vanilla_entities};
use steel_utils::entity_events::EntityStatus;
use steel_utils::{BlockPos, GlobalPos, WorldAabb};

use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::path::Path;
use crate::entity::{LivingEntity, PathfinderMob};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `AcquirePoi.SCAN_RANGE`.
pub const SCAN_RANGE: i32 = 48;
/// Vanilla `AcquirePoi`: how many of the closest candidates one scan paths to.
const BATCH_SIZE: usize = 5;
/// Vanilla `AcquirePoi`: the shortest wait between scans.
const RATE: i64 = 20;
/// Vanilla `ValidateNearbyPoi.MAX_DISTANCE`.
const VALIDATE_DISTANCE: f64 = 16.0;

/// Vanilla `AcquirePoi.JitteredLinearRetry`: backs off from a POI the body
/// could not path to, by 40 to 80 more ticks each time, up to 400.
struct JitteredLinearRetry {
    previous_attempt: i64,
    next_scheduled_attempt: i64,
    current_delay: i64,
}

impl JitteredLinearRetry {
    /// Vanilla `MIN_INTERVAL_INCREASE`.
    const MIN_INTERVAL_INCREASE: i64 = 40;
    /// Vanilla `MAX_INTERVAL_INCREASE`.
    const MAX_INTERVAL_INCREASE: i64 = 80;
    /// Vanilla `MAX_RETRY_PATHFINDING_INTERVAL`.
    const MAX_RETRY_INTERVAL: i64 = 400;

    fn new(first_attempt: i64) -> Self {
        let mut retry = Self {
            previous_attempt: 0,
            next_scheduled_attempt: 0,
            current_delay: 0,
        };
        retry.mark_attempt(first_attempt);
        retry
    }

    fn mark_attempt(&mut self, time: i64) {
        self.previous_attempt = time;
        let suggested = self.current_delay
            + rand::random_range(0..Self::MAX_INTERVAL_INCREASE - Self::MIN_INTERVAL_INCREASE)
            + Self::MIN_INTERVAL_INCREASE;
        self.current_delay = suggested.min(Self::MAX_RETRY_INTERVAL);
        self.next_scheduled_attempt = time + self.current_delay;
    }

    const fn is_still_valid(&self, time: i64) -> bool {
        time - self.previous_attempt < Self::MAX_RETRY_INTERVAL
    }

    const fn should_retry(&self, time: i64) -> bool {
        time >= self.next_scheduled_attempt
    }
}

/// The valid range (vanilla `PoiType.validRange`) of a POI type.
fn valid_range(poi_type: usize) -> i32 {
    REGISTRY
        .poi_types
        .by_id(poi_type)
        .and_then(|poi_type| i32::try_from(poi_type.search_distance).ok())
        .unwrap_or(1)
}

/// Vanilla `AcquirePoi.findPathToPois`: one path to whichever POI is
/// easiest to reach.
pub fn find_path_to_pois<E: PathfinderMob + ?Sized>(
    world: &Arc<World>,
    body: &E,
    pois: &[(BlockPos, usize)],
) -> Option<Path> {
    if pois.is_empty() {
        return None;
    }
    let max_range = pois
        .iter()
        .map(|(_, poi_type)| valid_range(*poi_type))
        .fold(1, i32::max);
    let targets: Vec<BlockPos> = pois.iter().map(|(pos, _)| *pos).collect();
    body.create_path_to_targets(world, &targets, max_range)
}

/// Vanilla `AcquirePoi.create`: every second or so, claims the closest
/// reachable POI of `poi_type` with a free ticket and remembers it in
/// `memory_to_acquire`. Only runs while `memory_to_validate` and
/// `memory_to_acquire` are both empty.
pub fn acquire_poi<E, P, V>(
    poi_type: P,
    memory_to_validate: &'static MemoryModuleType<GlobalPos>,
    memory_to_acquire: &'static MemoryModuleType<GlobalPos>,
    only_if_adult: bool,
    on_acquisition_event: Option<EntityStatus>,
    valid_poi: V,
) -> impl BehaviorControl<E>
where
    E: PathfinderMob + ?Sized,
    P: Fn(usize) -> bool + Send + 'static,
    V: Fn(&World, BlockPos) -> bool + Send + 'static,
{
    let mut next_scheduled_start = 0_i64;
    let mut batch_cache: FxHashMap<BlockPos, JitteredLinearRetry> = FxHashMap::default();
    let mut memories = vec![MemoryRequirement::new(
        memory_to_acquire,
        MemoryStatus::ValueAbsent,
    )];
    if !ptr::eq(memory_to_validate, memory_to_acquire) {
        memories.push(MemoryRequirement::new(
            memory_to_validate,
            MemoryStatus::ValueAbsent,
        ));
    }
    OneShot::new(memories, move |world: &Arc<World>, body: &E, time: i64| {
        let Some(brain) = body.brain_memories() else {
            return false;
        };
        {
            let memories = brain.lock();
            if memories.has_value(memory_to_acquire) || memories.has_value(memory_to_validate) {
                return false;
            }
        }
        if only_if_adult && body.is_baby() {
            return false;
        }
        if next_scheduled_start == 0 {
            next_scheduled_start = world.game_time() + rand::random_range(0..RATE);
            return false;
        }
        if world.game_time() < next_scheduled_start {
            return false;
        }
        next_scheduled_start = time + RATE + rand::random_range(0..RATE);
        batch_cache.retain(|_, retry| retry.is_still_valid(time));
        let candidates = world.poi_storage.lock().find_all_closest_first(
            &poi_type,
            |pos| match batch_cache.get_mut(&pos) {
                None => true,
                Some(retry) if !retry.should_retry(time) => false,
                Some(retry) => {
                    retry.mark_attempt(time);
                    true
                }
            },
            body.block_position(),
            SCAN_RANGE,
            OccupationStatus::Free,
        );
        let pois: Vec<(BlockPos, usize)> = candidates
            .into_iter()
            .take(BATCH_SIZE)
            .filter(|(pos, _)| valid_poi(world, *pos))
            .collect();
        let path = find_path_to_pois(world, body, &pois);
        if let Some(path) = path.filter(Path::can_reach) {
            let target = path.target();
            let mut storage = world.poi_storage.lock();
            if storage.get_type(target).is_some() {
                let _taken = storage.take(&poi_type, |_, pos| pos == target, target, 1);
                drop(storage);
                brain.lock().set(
                    memory_to_acquire,
                    Some(GlobalPos::new(world.key.clone(), target)),
                );
                if let Some(event) = on_acquisition_event {
                    body.broadcast_entity_event(event);
                }
                batch_cache.clear();
            }
        } else {
            for (pos, _) in pois {
                batch_cache
                    .entry(pos)
                    .or_insert_with(|| JitteredLinearRetry::new(time));
            }
        }
        true
    })
}

/// Vanilla `ValidateNearbyPoi.create`: once the body is within 16 blocks of
/// the POI in `memory`, forgets it if the POI is gone, or if it is a bed
/// someone else sleeps in (and frees that bed unless a villager sleeps
/// there).
pub fn validate_nearby_poi<E, P>(
    poi_type: P,
    memory: &'static MemoryModuleType<GlobalPos>,
) -> impl BehaviorControl<E>
where
    E: LivingEntity + ?Sized,
    P: Fn(usize) -> bool + Send + 'static,
{
    OneShot::new(
        vec![MemoryRequirement::new(memory, MemoryStatus::ValuePresent)],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(brain) = body.brain_memories() else {
                return false;
            };
            let Some(global) = brain.lock().get(memory) else {
                return false;
            };
            let pos = global.pos;
            let (x, y, z) = pos.get_center();
            let center = glam::DVec3::new(x, y, z);
            if world.key != global.dimension
                || center.distance_squared(body.position()) >= VALIDATE_DISTANCE * VALIDATE_DISTANCE
            {
                return false;
            }
            // The POI shares the body's dimension, so the body's world holds it.
            if !world.poi_storage.lock().exists(pos, &poi_type) {
                brain.lock().erase(memory);
            } else if bed_is_occupied(world, pos, body) {
                brain.lock().erase(memory);
                if !bed_is_occupied_by_villager(world, pos) {
                    let _released = world.poi_storage.lock().release_ticket(pos);
                }
            }
            true
        },
    )
}

/// Vanilla `ValidateNearbyPoi.bedIsOccupied`.
fn bed_is_occupied<E: LivingEntity + ?Sized>(world: &World, pos: BlockPos, body: &E) -> bool {
    let state = world.get_block_state(pos);
    state
        .get_block()
        .has_tag(&BlockTag::VILLAGERS_CAN_SLEEP_ON_BED)
        && state.try_get_value(&BlockStateProperties::OCCUPIED) == Some(true)
        && !body.is_sleeping()
}

/// Vanilla `ValidateNearbyPoi.bedIsOccupiedByVillager`.
fn bed_is_occupied_by_villager(world: &World, pos: BlockPos) -> bool {
    let aabb = WorldAabb::new(
        f64::from(pos.x()),
        f64::from(pos.y()),
        f64::from(pos.z()),
        f64::from(pos.x()) + 1.0,
        f64::from(pos.y()) + 1.0,
        f64::from(pos.z()) + 1.0,
    );
    world.has_entity_in_aabb_matching(&aabb, |entity| {
        entity.entity_type() == &vanilla_entities::VILLAGER
            && entity
                .as_living_entity()
                .is_some_and(LivingEntity::is_sleeping)
    })
}

/// Vanilla `VillagerGoalPackages.validateBedPoi`: a bed villagers can sleep
/// in that nobody occupies.
#[must_use]
pub fn validate_bed_poi(world: &World, pos: BlockPos) -> bool {
    let state = world.get_block_state(pos);
    state
        .get_block()
        .has_tag(&BlockTag::VILLAGERS_CAN_SLEEP_ON_BED)
        && state.try_get_value(&BlockStateProperties::OCCUPIED) == Some(false)
}
