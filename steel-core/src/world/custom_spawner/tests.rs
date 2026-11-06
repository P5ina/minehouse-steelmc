//! Custom spawner tests.

use std::sync::Arc;

use glam::DVec3;
use rand::{SeedableRng as _, rngs::StdRng};
use steel_registry::vanilla_custom_stats::TIME_SINCE_REST;
use steel_registry::{init_vanilla_registry, vanilla_dimension_types};
use steel_utils::ChunkPos;
use steel_utils::types::Difficulty;

use super::custom_spawners_for;
use super::phantom::phantom_spawn_pos;
use crate::behavior::init_behaviors;
use crate::entity::{Entity as _, init_entities};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, fresh_test_world_with_dimension_type, insert_ready_full_chunk,
};

#[test]
fn only_the_overworld_gets_custom_spawners() {
    assert_eq!(
        custom_spawners_for(&vanilla_dimension_types::OVERWORLD).len(),
        1
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
