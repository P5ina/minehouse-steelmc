//! Natural spawner tests.

use std::convert::Infallible;
use std::ptr;
use std::sync::Arc;

use glam::DVec3;
use rand::{SeedableRng as _, TryRng, rngs::StdRng};
use steel_registry::entity_type::MobCategory;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_dimension_types, vanilla_entities,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Identifier};

use rustc_hash::FxHashMap;
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;

use super::spawn_list::{SpawnList, SpawnerEntry, fortress_enemies};
use super::spawn_state::{PotentialCalculator, SpawnState};
use super::{SPAWNING_CATEGORIES, filtered_spawning_categories};
use super::{set_decoration_seed, spawn_mobs_for_chunk_generation};
use crate::behavior::init_behaviors;
use crate::chunk::Chunk;
use crate::chunk::chunk_generation_task::StaticCache2D;
use crate::chunk::chunk_holder::ChunkHolder;
use crate::chunk::chunk_pyramid::GENERATION_PYRAMID;
use crate::chunk::chunk_ticket_manager::ChunkTicketLevel;
use crate::chunk::section::{ChunkSection, Sections};
use crate::chunk::status::ChunkStatus;
use crate::entity::{Entity as _, init_entities};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, fresh_test_world_in_domain, fresh_test_world_with_dimension_type,
    insert_ready_full_chunk,
};
use crate::world::World;
use crate::worldgen::generator::ChunkGenerator as _;
use crate::worldgen::region::WorldGenRegion;

/// Returns the same raw value for every draw.
struct FixedRng(u64);

impl TryRng for FixedRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.0 as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0)
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        bytes.fill(self.0 as u8);
        Ok(())
    }
}

fn entry(weight: i32, min_count: i32, max_count: i32) -> SpawnerEntry {
    SpawnerEntry {
        entity_type: &vanilla_entities::ZOMBIE,
        weight,
        min_count,
        max_count,
    }
}

#[test]
fn mob_category_caps_match_vanilla() {
    assert_eq!(MobCategory::Monster.max_instances_per_chunk(), 70);
    assert_eq!(MobCategory::Creature.max_instances_per_chunk(), 10);
    assert_eq!(MobCategory::Ambient.max_instances_per_chunk(), 15);
    assert_eq!(MobCategory::WaterAmbient.max_instances_per_chunk(), 20);
    assert!(!MobCategory::Monster.is_friendly());
    assert!(MobCategory::Creature.is_persistent());
    assert!(!MobCategory::Ambient.is_persistent());
    assert!(!SPAWNING_CATEGORIES.contains(&MobCategory::Misc));
}

#[test]
fn spawn_list_picks_by_cumulative_weight() {
    let first = SpawnerEntry {
        entity_type: &vanilla_entities::CREEPER,
        weight: 3,
        min_count: 1,
        max_count: 1,
    };
    let second = SpawnerEntry {
        entity_type: &vanilla_entities::SPIDER,
        weight: 1,
        min_count: 1,
        max_count: 1,
    };
    let list = SpawnList::new(vec![first, second]);

    let low = list
        .get_random(&mut FixedRng(0))
        .map(|entry| entry.entity_type);
    let high = list
        .get_random(&mut FixedRng(u64::MAX))
        .map(|entry| entry.entity_type);

    assert!(
        low.is_some_and(|entity_type| ptr::eq(entity_type, &raw const vanilla_entities::CREEPER))
    );
    assert!(
        high.is_some_and(|entity_type| ptr::eq(entity_type, &raw const vanilla_entities::SPIDER))
    );
    assert_eq!(SpawnList::default().get_random(&mut FixedRng(0)), None);
}

#[test]
fn spawn_list_contains_ignores_weight() {
    let list = SpawnList::new(vec![entry(100, 4, 4)]);

    assert!(list.contains(&entry(1, 4, 4)));
    assert!(!list.contains(&entry(100, 1, 4)));
}

#[test]
fn fortress_enemies_match_vanilla() {
    init_vanilla_registry();
    let blaze = SpawnerEntry {
        entity_type: &vanilla_entities::BLAZE,
        weight: 10,
        min_count: 2,
        max_count: 3,
    };

    assert!(fortress_enemies().contains(&blaze));
}

#[test]
fn potential_energy_falls_off_with_distance() {
    let mut potential = PotentialCalculator::default();
    potential.add_charge(BlockPos::new(0, 0, 0), 0.7);

    let near = potential.potential_energy_change(BlockPos::new(1, 0, 0), 0.7);
    let far = potential.potential_energy_change(BlockPos::new(4, 0, 0), 0.7);

    assert!((near - 0.49).abs() < 1e-9);
    assert!((far - 0.1225).abs() < 1e-9);
    assert!(
        potential
            .potential_energy_change(BlockPos::new(0, 0, 0), 0.7)
            .is_infinite()
    );
    assert_eq!(
        potential.potential_energy_change(BlockPos::new(1, 0, 0), 0.0),
        0.0
    );
}

#[test]
fn global_cap_scales_with_spawnable_chunks() {
    let state = SpawnState::empty(289);
    assert!(state.can_spawn_for_category_global(MobCategory::Monster));
    assert!(filtered_spawning_categories(&state, true, true).contains(&MobCategory::Creature));
    assert!(!filtered_spawning_categories(&state, true, false).contains(&MobCategory::Creature));
    assert!(!filtered_spawning_categories(&state, false, true).contains(&MobCategory::Monster));

    // 70 * 16 / 289 rounds down to 3 monsters for a lone 4×4 chunk area.
    let small = SpawnState::empty(16);
    assert!(small.can_spawn_for_category_global(MobCategory::Monster));
    assert!(!SpawnState::empty(0).can_spawn_for_category_global(MobCategory::Monster));
}

#[test]
fn natural_spawn_places_monsters_away_from_players() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    // Without a lighting engine, test chunks report full sky light; a dimension
    // without sky light keeps the monster darkness check deterministic.
    let world_fixture = fresh_test_world_with_dimension_type(
        "natural_spawner",
        "monsters",
        &vanilla_dimension_types::THE_NETHER,
    );
    let world = &world_fixture.world;
    let chunk_pos = ChunkPos::new(0, 0);
    insert_ready_full_chunk(world, chunk_pos);
    for x in 0..16 {
        for z in 0..16 {
            assert!(world.set_block(
                BlockPos::new(x, 64, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            ));
        }
    }
    let player = TestPlayerBuilder::new(Arc::clone(world), "SpawnPlayer", 1).build();
    player.snap_to(DVec3::new(8.0, 65.0, 48.0), 0.0, 0.0);
    // Keep the shared respawn point out of the 24 block exclusion.
    world
        .level_data
        .write()
        .data_mut()
        .set_spawn_pos(BlockPos::new(1000, 64, 1000));
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    let biome = world
        .biome_at(BlockPos::new(8, 65, 8))
        .map(|biome| biome.key.clone());
    assert!(
        biome
            .as_ref()
            .is_some_and(|key| *key != Identifier::vanilla_static("the_void")),
        "unexpected test biome {biome:?}"
    );

    let mut spawned = Vec::new();
    for seed in 0..64 {
        let mut state = SpawnState::empty(289);
        let mut random = StdRng::seed_from_u64(seed);
        world.spawn_category_for_position(
            MobCategory::Monster,
            chunk_pos,
            BlockPos::new(8, 65, 8),
            &mut state,
            &mut random,
        );
        spawned.extend(
            world
                .entity_manager
                .get_accessible_entities()
                .into_iter()
                .filter(|entity| entity.as_player().is_none()),
        );
        if !spawned.is_empty() {
            break;
        }
    }

    assert!(!spawned.is_empty(), "no monster spawned in {biome:?}");
    for entity in &spawned {
        assert_eq!(entity.entity_type().mob_category, MobCategory::Monster);
        assert_eq!(entity.block_position().y(), 65);
        assert!(entity.position().distance_squared(player.position()) > 24.0 * 24.0);
    }
}

/// Builds a 3×3 area of generation chunks at `Light`, floored with grass at
/// y = 63, and returns their holders keyed by position.
fn grass_generation_area(world: &Arc<World>) -> FxHashMap<(i32, i32), Arc<ChunkHolder>> {
    let min_y = world.get_min_y();
    let height = world.get_height();
    let grass_y = usize::try_from(63 - min_y).unwrap_or(0);
    let mut holders = FxHashMap::default();
    for chunk_x in -1..=1 {
        for chunk_z in -1..=1 {
            let pos = ChunkPos::new(chunk_x, chunk_z);
            let sections = (0..height / 16)
                .map(|_| ChunkSection::new_empty())
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let chunk = Chunk::new(
                Sections::from_owned(sections),
                pos,
                min_y,
                height,
                Arc::downgrade(world),
            );
            for x in 0..16 {
                for z in 0..16 {
                    chunk.set_relative_block_for_generation(
                        ChunkStatus::Light,
                        x,
                        grass_y,
                        z,
                        vanilla_blocks::GRASS_BLOCK.default_state(),
                    );
                }
            }
            chunk.prime_final_heightmaps();
            let holder = Arc::new(ChunkHolder::new(
                pos,
                ChunkTicketLevel::FULL_CHUNK,
                Some(ChunkTicketLevel::FULL_CHUNK),
                min_y,
                height,
            ));
            holder.insert_chunk(chunk, ChunkStatus::Light);
            holders.insert((chunk_x, chunk_z), holder);
        }
    }
    holders
}

#[test]
fn chunk_generation_places_creatures_on_grass() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world_in_domain("natural_spawner", "chunk_generation");
    let world = &world_fixture.world;
    let holders = Arc::new(grass_generation_area(world));
    let cache_holders = Arc::clone(&holders);
    let cache = StaticCache2D::create(0, 0, 1, move |x, z| {
        let Some(holder) = cache_holders.get(&(x, z)) else {
            panic!("missing test chunk ({x}, {z})");
        };
        Arc::clone(holder)
    });
    let context = Arc::clone(&world.chunk_map.world_gen_context);
    let step = GENERATION_PYRAMID.get_step_to(ChunkStatus::Spawn);
    let center = ChunkPos::new(0, 0);
    let region_random = context
        .generator
        .create_worldgen_region_random(world.seed(), center);
    let region = WorldGenRegion::new(context.as_ref(), step, &cache, center, region_random);

    let mut spawned = Vec::new();
    for seed in 0_i64..64 {
        let mut random = LegacyRandom::from_seed(0);
        // Java random draws barely differ for adjacent small seeds.
        set_decoration_seed(&mut random, seed.wrapping_mul(0x5851_F42D_4C95_7F2D), 0, 0);
        spawn_mobs_for_chunk_generation(
            &region,
            world,
            BlockPos::new(0, 319, 0),
            center,
            &mut random,
        );
        if let Some(chunk) = holders[&(0, 0)].try_chunk(ChunkStatus::Light) {
            spawned = chunk.get_entities();
        }
        if !spawned.is_empty() {
            break;
        }
    }

    assert!(!spawned.is_empty(), "no creature spawned on grass");
    for entity in &spawned {
        assert_eq!(entity.entity_type().mob_category, MobCategory::Creature);
        let pos = entity.block_position();
        assert_eq!(
            pos.y(),
            64,
            "{} spawned at {pos:?}",
            entity.entity_type().key
        );
        assert!((0..16).contains(&pos.x()) && (0..16).contains(&pos.z()));
    }
}

#[test]
fn decoration_seed_is_deterministic_per_chunk() {
    let mut first = LegacyRandom::from_seed(1);
    let mut second = LegacyRandom::from_seed(2);
    set_decoration_seed(&mut first, 42, 16, -32);
    set_decoration_seed(&mut second, 42, 16, -32);
    assert_eq!(first.next_i64(), second.next_i64());

    let mut other_chunk = LegacyRandom::from_seed(1);
    set_decoration_seed(&mut other_chunk, 42, 32, -32);
    assert_ne!(first.next_i64(), other_chunk.next_i64());
}
