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

use super::spawn_list::{SpawnList, SpawnerEntry, fortress_enemies};
use super::spawn_state::{PotentialCalculator, SpawnState};
use super::{SPAWNING_CATEGORIES, filtered_spawning_categories};
use crate::behavior::init_behaviors;
use crate::entity::{Entity as _, init_entities};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, fresh_test_world_with_dimension_type, insert_ready_full_chunk,
};

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
