//! Vanilla `NaturalSpawner.spawnMobsForChunkGeneration`: the creatures placed
//! once while a chunk generates.

use std::sync::Arc;

use glam::DVec3;
use steel_math::DEGREE_360;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityTypeRef, MobCategory};
use steel_registry::vanilla_game_rules::SPAWN_MOBS;
use steel_utils::random::Random;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{BlockPos, ChunkPos};

use super::spawn_list::SpawnerEntry;
use crate::entity::{
    EntitySpawnReason, SharedEntity, SpawnPlacements, create_entity_instance, spawn_aabb,
};
use crate::physics::collision::level_has_block_collision;
use crate::world::{LevelReader, ServerLevelAccessor, World};
use crate::worldgen::region::WorldGenRegion;

/// Attempts per mob to find a free column near the group origin.
const ATTEMPTS_PER_MOB: i32 = 4;

/// Vanilla `WorldgenRandom.setDecorationSeed` over a `LegacyRandomSource`.
pub(crate) fn set_decoration_seed(
    random: &mut LegacyRandom,
    seed: i64,
    block_x: i32,
    block_z: i32,
) {
    random.set_seed(seed);
    let x_scale = random.next_i64() | 1;
    let z_scale = random.next_i64() | 1;
    random.set_seed(
        i64::from(block_x)
            .wrapping_mul(x_scale)
            .wrapping_add(i64::from(block_z).wrapping_mul(z_scale))
            ^ seed,
    );
}

/// Vanilla `NaturalSpawner.spawnMobsForChunkGeneration`.
///
/// Spawn predicates read the region; `finalizeSpawn` runs against the live
/// world, whose biome lookup falls back to the generator for chunks that are
/// not loaded yet.
pub(crate) fn spawn_mobs_for_chunk_generation(
    region: &WorldGenRegion<'_>,
    world: &Arc<World>,
    source_pos: BlockPos,
    chunk_pos: ChunkPos,
    random: &mut LegacyRandom,
) {
    if !world.get_game_rule(&SPAWN_MOBS) {
        return;
    }
    let mobs = world.biome_mobs_at(MobCategory::Creature, source_pos);
    if mobs.is_empty() {
        return;
    }
    let creature_probability = world
        .biome_at(source_pos)
        .map_or(0.0, |biome| biome.creature_spawn_probability);
    let xo = chunk_pos.0.x * 16;
    let zo = chunk_pos.0.y * 16;

    while random.next_f32() < creature_probability {
        let Some(spawner_data) = mobs.get_random_java(random) else {
            continue;
        };
        let count = spawner_data.sample_count_java(random);
        let mut group_data = None;
        let start_x = xo + random.next_i32_bounded(16);
        let start_z = zo + random.next_i32_bounded(16);
        let (mut x, mut z) = (start_x, start_z);

        for _ in 0..count {
            let mut success = false;
            let mut attempts = 0;
            while !success && attempts < ATTEMPTS_PER_MOB {
                attempts += 1;
                if let Some(entity) =
                    try_spawn_at(region, world, &spawner_data, (xo, zo), (x, z), random)
                    && let Some(mob) = entity.as_mob()
                {
                    group_data = mob.finalize_spawn(
                        world,
                        EntitySpawnReason::ChunkGeneration,
                        group_data.take(),
                    );
                    add_fresh_entity_with_passengers(region, &entity);
                    success = true;
                }

                x += random.next_i32_bounded(5) - random.next_i32_bounded(5);
                z += random.next_i32_bounded(5) - random.next_i32_bounded(5);
                while x < xo || x >= xo + 16 || z < zo || z >= zo + 16 {
                    x = start_x + random.next_i32_bounded(5) - random.next_i32_bounded(5);
                    z = start_z + random.next_i32_bounded(5) - random.next_i32_bounded(5);
                }
            }
        }
    }
}

/// One placement attempt of vanilla's inner loop: returns the created mob when
/// the column, the type's rules and the mob's own checks all accept it.
fn try_spawn_at(
    region: &WorldGenRegion<'_>,
    world: &Arc<World>,
    spawner_data: &SpawnerEntry,
    (xo, zo): (i32, i32),
    (x, z): (i32, i32),
    random: &mut LegacyRandom,
) -> Option<SharedEntity> {
    let entity_type = spawner_data.entity_type;
    let pos = top_non_colliding_pos(region, entity_type, x, z);
    if !entity_type.summonable || !SpawnPlacements::is_spawn_position_ok(entity_type, region, pos) {
        return None;
    }
    let width = f64::from(entity_type.dimensions.width);
    let fx = clamp(
        f64::from(x),
        f64::from(xo) + width,
        f64::from(xo) + 16.0 - width,
    );
    let fz = clamp(
        f64::from(z),
        f64::from(zo) + width,
        f64::from(zo) + 16.0 - width,
    );
    let y = f64::from(pos.y());
    // Vanilla passes the region's own random here, which the creature spawn
    // rules never draw from; it stays separate from the seeded group random.
    if level_has_block_collision(region, spawn_aabb(entity_type, fx, y, fz))
        || !SpawnPlacements::check_spawn_rules(
            entity_type,
            region,
            EntitySpawnReason::ChunkGeneration,
            BlockPos::new(fx.floor() as i32, pos.y(), fz.floor() as i32),
            &mut rand::rng(),
        )
    {
        return None;
    }

    let position = DVec3::new(fx, y, fz);
    let entity = match create_entity_instance(world, entity_type, position) {
        Ok(entity) => entity,
        Err(error) => {
            log::warn!("Failed to create mob {}: {error:?}", entity_type.key);
            return None;
        }
    };
    entity.snap_to(position, random.next_f32() * DEGREE_360, 0.0);
    let mob = entity.as_mob()?;
    (mob.check_spawn_rules(region, EntitySpawnReason::ChunkGeneration)
        && mob.check_spawn_obstruction(region))
    .then_some(entity)
}

/// Vanilla `Level.addFreshEntityWithPassengers` on a worldgen region.
fn add_fresh_entity_with_passengers(region: &WorldGenRegion<'_>, entity: &SharedEntity) {
    let passengers = entity.passengers();
    let _ = region.add_fresh_entity(Arc::clone(entity));
    for passenger in passengers {
        add_fresh_entity_with_passengers(region, &passenger);
    }
}

/// Vanilla `NaturalSpawner.getTopNonCollidingPos`.
fn top_non_colliding_pos(
    region: &WorldGenRegion<'_>,
    entity_type: EntityTypeRef,
    x: i32,
    z: i32,
) -> BlockPos {
    let level_height =
        LevelReader::height_at(region, SpawnPlacements::heightmap_type(entity_type), x, z);
    let mut pos = BlockPos::new(x, level_height, z);
    if ServerLevelAccessor::dimension_type(region).has_ceiling {
        loop {
            pos = pos.below();
            if region.get_block_state(pos).is_air() {
                break;
            }
        }
        loop {
            pos = pos.below();
            if !(region.get_block_state(pos).is_air() && pos.y() > LevelReader::min_y(region)) {
                break;
            }
        }
    }
    SpawnPlacements::placement_type(entity_type).adjust_spawn_position(region, pos)
}

/// Vanilla `Mth.clamp(double, double, double)`.
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value < min { min } else { value.min(max) }
}
