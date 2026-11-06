//! Vanilla `PatrolSpawner`: pillager patrols near players by day.

use std::sync::Arc;

use glam::DVec3;
use rand::{Rng, RngExt as _};
use steel_registry::vanilla_entities::PILLAGER;
use steel_registry::vanilla_game_rules::SPAWN_PATROLS;
use steel_utils::{BlockPos, ChunkPos};

use super::CustomSpawner;
use crate::chunk::heightmap::HeightmapType;
use crate::entity::entities::mobs::raiders::PillagerEntity;
use crate::entity::{
    Entity as _, EntitySpawnReason, PatrollingMonster, create_entity_instance,
    is_valid_empty_spawn_block,
};
use crate::world::World;

/// Vanilla `nextTick += 12000 + random.nextInt(1200)`.
const BASE_TICK_DELAY: i32 = 12_000;
const TICK_DELAY_SPREAD: i32 = 1_200;
/// Vanilla `random.nextInt(5) == 0`: one attempt in five spawns.
const SPAWN_ATTEMPT_ROLL: i32 = 5;
/// No patrol spawns within this many sections of a village.
const VILLAGE_SECTION_DISTANCE: i32 = 2;
/// The patrol lands `24 + nextInt(24)` blocks away on each axis.
const MIN_PLAYER_DISTANCE: i32 = 24;
const PLAYER_DISTANCE_SPREAD: i32 = 24;
/// Vanilla `delta`: the area around the spawn position that must be loaded.
const LOADED_AREA_RADIUS: i32 = 10;
/// Members spread by `nextInt(5) - nextInt(5)` blocks from the previous one.
const MEMBER_SPREAD: i32 = 5;

/// Vanilla `PatrolSpawner`.
#[derive(Default)]
pub(crate) struct PatrolSpawner {
    next_tick: i32,
}

impl CustomSpawner for PatrolSpawner {
    fn tick(&mut self, world: &Arc<World>, spawn_enemies: bool) {
        if !spawn_enemies || !world.get_game_rule(&SPAWN_PATROLS) {
            return;
        }
        let mut random = rand::rng();
        self.next_tick -= 1;
        if self.next_tick > 0 {
            return;
        }
        self.next_tick += BASE_TICK_DELAY + random.random_range(0..TICK_DELAY_SPREAD);
        if !world.is_bright_outside() || random.random_range(0..SPAWN_ATTEMPT_ROLL) != 0 {
            return;
        }
        if let Some(spawn_pos) = patrol_spawn_pos(world, &mut random) {
            spawn_patrol(world, spawn_pos, &mut random);
        }
    }
}

/// The player half of vanilla `PatrolSpawner.tick`: a random player away from
/// villages, and the position 24 to 47 blocks off them on each axis where the
/// patrol may spawn.
pub(super) fn patrol_spawn_pos(world: &World, random: &mut impl Rng) -> Option<BlockPos> {
    let mut players = Vec::new();
    world.players.iter_players(|_, player| {
        players.push(Arc::clone(player));
        true
    });
    if players.is_empty() {
        return None;
    }
    let player = &players[random.random_range(0..players.len())];
    if player.is_spectator() {
        return None;
    }
    let player_pos = player.block_position();
    if world.is_close_to_village(player_pos, VILLAGE_SECTION_DISTANCE) {
        return None;
    }
    let x = patrol_offset(random);
    let z = patrol_offset(random);
    let spawn_pos = player_pos.offset(x, 0, z);
    (has_chunks_at(
        world,
        spawn_pos.x() - LOADED_AREA_RADIUS,
        spawn_pos.z() - LOADED_AREA_RADIUS,
        spawn_pos.x() + LOADED_AREA_RADIUS,
        spawn_pos.z() + LOADED_AREA_RADIUS,
    ) && world.can_pillager_patrol_spawn(spawn_pos))
    .then_some(spawn_pos)
}

/// Vanilla `(24 + nextInt(24)) * (nextBoolean() ? -1 : 1)`.
fn patrol_offset(random: &mut impl Rng) -> i32 {
    let distance = MIN_PLAYER_DISTANCE + random.random_range(0..PLAYER_DISTANCE_SPREAD);
    if random.random::<bool>() {
        -distance
    } else {
        distance
    }
}

/// Vanilla `LevelReader.hasChunksAt(x0, z0, x1, z1)`: every chunk in the block
/// rectangle is loaded.
fn has_chunks_at(world: &World, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
    let min = ChunkPos::from_block_pos(BlockPos::new(min_x, 0, min_z));
    let max = ChunkPos::from_block_pos(BlockPos::new(max_x, 0, max_z));
    (min.0.x..=max.0.x).all(|chunk_x| {
        (min.0.y..=max.0.y).all(|chunk_z| {
            world.is_full_chunk_loaded_at(BlockPos::new(chunk_x << 4, 0, chunk_z << 4))
        })
    })
}

/// Vanilla's group loop: `ceil(effective difficulty) + 1` pillagers, the first
/// one leading. The patrol stops if the leader cannot spawn.
pub(super) fn spawn_patrol(world: &Arc<World>, mut spawn_pos: BlockPos, random: &mut impl Rng) {
    let effective_difficulty = world
        .current_difficulty_at(spawn_pos)
        .effective_difficulty();
    // Effective difficulty stays within 0..=6.75, so the cast cannot truncate.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla (int) Math.ceil of a small float"
    )]
    let group_size = effective_difficulty.ceil() as i32 + 1;
    for member in 0..group_size {
        spawn_pos = BlockPos::new(
            spawn_pos.x(),
            world
                .heightmap_pos(HeightmapType::MotionBlockingNoLeaves, spawn_pos)
                .y(),
            spawn_pos.z(),
        );
        let is_leader = member == 0;
        if !spawn_patrol_member(world, spawn_pos, is_leader) && is_leader {
            break;
        }
        spawn_pos = BlockPos::new(
            spawn_pos.x() + random.random_range(0..MEMBER_SPREAD)
                - random.random_range(0..MEMBER_SPREAD),
            spawn_pos.y(),
            spawn_pos.z() + random.random_range(0..MEMBER_SPREAD)
                - random.random_range(0..MEMBER_SPREAD),
        );
    }
}

/// Vanilla `PatrolSpawner.spawnPatrolMember`.
fn spawn_patrol_member(world: &Arc<World>, pos: BlockPos, is_leader: bool) -> bool {
    if !is_valid_empty_spawn_block(world, pos, &PILLAGER)
        || !<PillagerEntity as PatrollingMonster>::check_patrolling_monster_spawn_rules(
            &PILLAGER,
            world.as_ref(),
            EntitySpawnReason::Patrol,
            pos,
        )
    {
        return false;
    }
    let position = DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()));
    // Vanilla `EntityType.create` returns null for a type it cannot build;
    // Steel reports that as an error and the member counts as not spawned.
    // Vanilla creates the mob at the origin and picks the leader's patrol
    // target before `setPos`, so the target lies within 500 blocks of 0, 0.
    let Ok(pillager) = create_entity_instance(world, &PILLAGER, DVec3::ZERO) else {
        return false;
    };
    if is_leader && let Some(patrolling) = pillager.as_patrolling_monster() {
        patrolling.set_patrol_leader(true);
        patrolling.find_patrol_target();
    }
    pillager.snap_to(position, 0.0, 0.0);
    if let Some(mob) = pillager.as_mob() {
        mob.finalize_spawn(world, EntitySpawnReason::Patrol, None);
    }
    if let Err(error) = world.try_add_fresh_entity_with_passengers(pillager) {
        log::debug!("Patrol member spawn failed: {error:?}");
    }
    true
}
