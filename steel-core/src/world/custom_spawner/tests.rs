//! Custom spawner tests.

use std::sync::Arc;

use glam::DVec3;
use rand::{SeedableRng as _, rngs::StdRng};
use steel_registry::vanilla_custom_stats::TIME_SINCE_REST;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_dimension_types, vanilla_entities,
    vanilla_world_clocks,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, WorldAabb};

use super::custom_spawners_for;
use super::patrol::{patrol_spawn_pos, spawn_patrol};
use super::phantom::phantom_spawn_pos;
use crate::behavior::init_behaviors;
use crate::entity::{Entity as _, init_entities};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, fresh_test_world, fresh_test_world_with_dimension_type,
    insert_ready_full_chunk,
};

#[test]
fn only_the_overworld_gets_custom_spawners() {
    assert_eq!(
        custom_spawners_for(&vanilla_dimension_types::OVERWORLD).len(),
        2
    );
    assert!(custom_spawners_for(&vanilla_dimension_types::THE_NETHER).is_empty());
    assert!(custom_spawners_for(&vanilla_dimension_types::THE_END).is_empty());
}

#[test]
fn phantoms_come_only_for_players_who_have_not_rested() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    // Without sky light every player position qualifies, as in vanilla.
    let world_fixture = fresh_test_world_with_dimension_type(
        "custom_spawner",
        "phantoms",
        &vanilla_dimension_types::THE_NETHER,
    );
    let world = &world_fixture.world;
    world.set_difficulty(Difficulty::Hard);
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Insomniac", 1).build();
    player.snap_to(DVec3::new(8.0, 40.0, 8.0), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));

    player.reset_custom_stat(&TIME_SINCE_REST);
    for seed in 0..64 {
        let mut random = StdRng::seed_from_u64(seed);
        assert_eq!(phantom_spawn_pos(world, &player, &mut random), None);
    }

    player.award_custom_stat_with_count(&TIME_SINCE_REST, 1_000_000);
    let player_pos = player.block_position();
    let mut spawned = 0;
    for seed in 0..64 {
        let mut random = StdRng::seed_from_u64(seed);
        if let Some((pos, difficulty)) = phantom_spawn_pos(world, &player, &mut random) {
            spawned += 1;
            assert_eq!(difficulty.difficulty(), Difficulty::Hard);
            assert!((20..35).contains(&(pos.y() - player_pos.y())), "{pos:?}");
            assert!((-10..=10).contains(&(pos.x() - player_pos.x())), "{pos:?}");
            assert!((-10..=10).contains(&(pos.z() - player_pos.z())), "{pos:?}");
        }
    }
    assert!(spawned > 0, "no phantom position chosen");
}

/// Vanilla `early_game` keeps patrols off for the first five days.
const EARLY_GAME_TICKS: i64 = 120_000;

#[test]
fn patrols_skip_the_early_game_and_spawn_away_from_the_player() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("patrol_spawn_pos");
    let world = &world_fixture.world;
    // The patrol lands up to 47 blocks off, and 10 more around it must load.
    for x in -4..=4 {
        for z in -4..=4 {
            insert_ready_full_chunk(world, ChunkPos::new(x, z));
        }
    }
    let player = TestPlayerBuilder::new(Arc::clone(world), "Wanderer", 1).build();
    player.snap_to(DVec3::new(8.0, 65.0, 8.0), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    let player_pos = player.block_position();

    assert_eq!(
        world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, EARLY_GAME_TICKS - 1),
        Some(())
    );
    for seed in 0..32 {
        let mut random = StdRng::seed_from_u64(seed);
        assert_eq!(patrol_spawn_pos(world, &mut random), None);
    }

    assert_eq!(
        world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, EARLY_GAME_TICKS),
        Some(())
    );
    for seed in 0..32 {
        let mut random = StdRng::seed_from_u64(seed);
        let pos = patrol_spawn_pos(world, &mut random).expect("a patrol position");
        for offset in [pos.x() - player_pos.x(), pos.z() - player_pos.z()] {
            assert!((24..48).contains(&offset.abs()), "{pos:?}");
        }
        assert_eq!(pos.y(), player_pos.y());
    }
}

#[test]
fn patrols_spawn_a_leader_with_a_banner_and_followers() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("patrol_group");
    let world = &world_fixture.world;
    world.set_difficulty(Difficulty::Hard);
    for x in -1..=1 {
        for z in -1..=1 {
            insert_ready_full_chunk(world, ChunkPos::new(x, z));
        }
    }
    for x in -16..32 {
        for z in -16..32 {
            assert!(world.set_block(
                BlockPos::new(x, 64, z),
                vanilla_blocks::GRASS_BLOCK.default_state(),
                UpdateFlags::UPDATE_ALL,
            ));
        }
    }
    let spawn_pos = BlockPos::new(8, 0, 8);
    let effective = world
        .current_difficulty_at(spawn_pos)
        .effective_difficulty();
    let expected = effective.ceil() as usize + 1;

    let mut random = StdRng::seed_from_u64(7);
    spawn_patrol(world, spawn_pos, &mut random);

    let pillagers = world.get_entities_in_aabb_matching(
        &WorldAabb::new(-16.0, 60.0, -16.0, 32.0, 70.0, 32.0),
        |entity| entity.entity_type() == &vanilla_entities::PILLAGER,
    );
    assert_eq!(pillagers.len(), expected);
    let mut leaders = 0;
    for pillager in &pillagers {
        assert!(
            (pillager.position().y - 65.0).abs() < 1.0e-9,
            "on the ground"
        );
        let patrolling = pillager.as_patrolling_monster().expect("pillagers patrol");
        assert!(patrolling.is_patrolling());
        if patrolling.is_patrol_leader() {
            leaders += 1;
            let target = patrolling.patrol_target().expect("the leader has a target");
            assert!(
                target.x().abs() <= 500 && target.z().abs() <= 500,
                "{target:?}"
            );
        }
    }
    assert_eq!(leaders, 1);
}

#[test]
fn a_patrol_stops_when_its_leader_cannot_spawn() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("patrol_blocked_leader");
    let world = &world_fixture.world;
    world.set_difficulty(Difficulty::Hard);
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    // No floor: the heightmap lands the leader on the world bottom with
    // nothing to stand on.
    let mut random = StdRng::seed_from_u64(7);
    spawn_patrol(world, BlockPos::new(8, 0, 8), &mut random);
    assert!(
        world
            .get_entities_in_aabb_matching(
                &WorldAabb::new(-16.0, -128.0, -16.0, 32.0, 320.0, 32.0),
                |entity| entity.entity_type() == &vanilla_entities::PILLAGER,
            )
            .is_empty()
    );
}
