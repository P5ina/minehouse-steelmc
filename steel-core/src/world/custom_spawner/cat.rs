//! Vanilla `CatSpawner`: stray cats in villages and witch huts.

use std::sync::Arc;

use rand::{Rng, RngExt as _};
use steel_registry::vanilla_entities::CAT;
use steel_registry::{RegistryEntry as _, vanilla_poi_types};
use steel_utils::{BlockPos, WorldAabb};

use super::{CustomSpawner, has_chunks_at};
use crate::entity::{Entity as _, EntitySpawnReason, SpawnPlacements, create_entity_instance};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `TICK_DELAY`: one attempt a minute.
const TICK_DELAY: i32 = 1_200;
/// The cat lands `8 + nextInt(24)` blocks off the player on each axis.
const MIN_PLAYER_DISTANCE: i32 = 8;
const PLAYER_DISTANCE_SPREAD: i32 = 24;
/// Vanilla `delta`: the area around the spawn position that must be loaded.
const LOADED_AREA_RADIUS: i32 = 10;
/// Villages count within this many sections of the spawn position.
const VILLAGE_SECTION_DISTANCE: i32 = 2;
/// A village needs more than this many claimed beds within `VILLAGE_RADIUS`.
const MIN_VILLAGE_HOMES: usize = 4;
const VILLAGE_RADIUS: i32 = 48;
/// A village keeps spawning cats while fewer than this many are around.
const MAX_VILLAGE_CATS: usize = 5;
const HUT_RADIUS: f64 = 16.0;
/// Vertical reach of both cat counts.
const CAT_COUNT_HEIGHT: f64 = 8.0;
/// Vanilla `StructureTags.CATS_SPAWN_IN` (swamp huts).
const CATS_SPAWN_IN: &str = "#minecraft:cats_spawn_in";

/// Vanilla `CatSpawner`.
#[derive(Default)]
pub(crate) struct CatSpawner {
    next_tick: i32,
}

impl CustomSpawner for CatSpawner {
    fn tick(&mut self, world: &Arc<World>, _spawn_enemies: bool) {
        self.next_tick -= 1;
        if self.next_tick > 0 {
            return;
        }
        self.next_tick = TICK_DELAY;
        let mut random = rand::rng();
        let Some(spawn_pos) = cat_spawn_pos(world, &mut random) else {
            return;
        };
        if world.is_close_to_village(spawn_pos, VILLAGE_SECTION_DISTANCE) {
            spawn_in_village(world, spawn_pos);
        } else if world.structure_with_piece_at(CATS_SPAWN_IN, spawn_pos) {
            spawn_in_hut(world, spawn_pos);
        }
    }
}

/// The player half of vanilla `CatSpawner.tick`: a random living player, and
/// the position 8 to 31 blocks off them on each axis, if it is loaded and a
/// cat could stand there.
pub(super) fn cat_spawn_pos(world: &World, random: &mut impl Rng) -> Option<BlockPos> {
    // Vanilla `ServerLevel.getRandomPlayer`: a uniform pick among living players.
    let mut players = Vec::new();
    world.players.iter_players(|_, player| {
        if player.is_alive() {
            players.push(Arc::clone(player));
        }
        true
    });
    if players.is_empty() {
        return None;
    }
    let player = &players[random.random_range(0..players.len())];
    let x = cat_offset(random);
    let z = cat_offset(random);
    let spawn_pos = player.block_position().offset(x, 0, z);
    (has_chunks_at(
        world,
        spawn_pos.x() - LOADED_AREA_RADIUS,
        spawn_pos.z() - LOADED_AREA_RADIUS,
        spawn_pos.x() + LOADED_AREA_RADIUS,
        spawn_pos.z() + LOADED_AREA_RADIUS,
    ) && SpawnPlacements::is_spawn_position_ok(&CAT, world, spawn_pos))
    .then_some(spawn_pos)
}

/// Vanilla `(8 + nextInt(24)) * (nextBoolean() ? -1 : 1)`.
fn cat_offset(random: &mut impl Rng) -> i32 {
    let distance = MIN_PLAYER_DISTANCE + random.random_range(0..PLAYER_DISTANCE_SPREAD);
    if random.random::<bool>() {
        -distance
    } else {
        distance
    }
}

/// Vanilla `spawnInVillage`: more than four claimed beds within 48 blocks and
/// fewer than five cats around.
pub(super) fn spawn_in_village(world: &Arc<World>, spawn_pos: BlockPos) {
    let Some(home) = vanilla_poi_types::HOME.try_id() else {
        return;
    };
    let homes = world.poi_storage.lock().count(
        &|poi_type| poi_type == home,
        spawn_pos,
        VILLAGE_RADIUS,
        OccupationStatus::Occupied,
    );
    if homes > MIN_VILLAGE_HOMES
        && cats_around(world, spawn_pos, f64::from(VILLAGE_RADIUS)) < MAX_VILLAGE_CATS
    {
        spawn_cat(world, spawn_pos, false);
    }
}

/// Vanilla `spawnInHut`: one persistent cat per hut.
pub(super) fn spawn_in_hut(world: &Arc<World>, spawn_pos: BlockPos) {
    if cats_around(world, spawn_pos, HUT_RADIUS) == 0 {
        spawn_cat(world, spawn_pos, true);
    }
}

/// Vanilla `getEntitiesOfClass(Cat.class, new AABB(spawnPos).inflate(r, 8, r))`.
fn cats_around(world: &World, spawn_pos: BlockPos, horizontal_radius: f64) -> usize {
    let (x, y, z) = (
        f64::from(spawn_pos.x()),
        f64::from(spawn_pos.y()),
        f64::from(spawn_pos.z()),
    );
    let area = WorldAabb::new(x, y, z, x + 1.0, y + 1.0, z + 1.0).inflate_xyz(
        horizontal_radius,
        CAT_COUNT_HEIGHT,
        horizontal_radius,
    );
    world
        .get_entities_in_aabb_matching(&area, |entity| {
            entity.entity_type() == &CAT && !entity.is_spectator()
        })
        .len()
}

/// Vanilla `spawnCat`. As in vanilla, the cat is finalized before it moves
/// into place, so its spawn variant is picked at the world origin.
fn spawn_cat(world: &Arc<World>, spawn_pos: BlockPos, make_persistent: bool) {
    // Vanilla `EntityType.create` returns null for a type it cannot build;
    // Steel reports that as an error and no cat spawns.
    let Ok(cat) = create_entity_instance(world, &CAT, glam::DVec3::ZERO) else {
        return;
    };
    if let Some(mob) = cat.as_mob() {
        mob.finalize_spawn(world, EntitySpawnReason::Natural, None);
        if make_persistent {
            mob.set_persistence_required();
        }
    }
    let (x, y, z) = spawn_pos.get_bottom_center();
    cat.snap_to(glam::DVec3::new(x, y, z), 0.0, 0.0);
    if let Err(error) = world.try_add_fresh_entity_with_passengers(cat) {
        log::debug!("Cat spawn failed: {error:?}");
    }
}
