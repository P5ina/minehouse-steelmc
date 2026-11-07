//! Custom spawner tests.

use std::sync::Arc;

use glam::DVec3;
use rand::{SeedableRng as _, rngs::StdRng};
use steel_registry::vanilla_custom_stats::TIME_SINCE_REST;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_dimension_types,
    vanilla_entities, vanilla_poi_types, vanilla_world_clocks,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Identifier, WorldAabb};

use super::cat::{spawn_in_hut, spawn_in_village};
use super::custom_spawners_for;
use super::patrol::{patrol_spawn_pos, spawn_patrol};
use super::phantom::phantom_spawn_pos;
use super::village_siege::{SiegeState, VillageSiege};
use crate::behavior::init_behaviors;
use crate::entity::{Entity as _, SharedEntity, init_entities};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, fresh_test_world, fresh_test_world_with_dimension_type,
    insert_ready_full_chunk,
};
use crate::world::World;

#[test]
fn only_the_overworld_gets_custom_spawners() {
    assert_eq!(
        custom_spawners_for(&vanilla_dimension_types::OVERWORLD).len(),
        4
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

fn cats_near(world: &World) -> Vec<SharedEntity> {
    world.get_entities_in_aabb_matching(
        &WorldAabb::new(-64.0, 0.0, -64.0, 64.0, 128.0, 64.0),
        |entity| entity.entity_type() == &vanilla_entities::CAT,
    )
}

#[test]
fn a_witch_hut_keeps_one_persistent_cat() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("cat_hut");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let spawn_pos = BlockPos::new(8, 65, 8);

    spawn_in_hut(world, spawn_pos);
    let cats = cats_near(world);
    assert_eq!(cats.len(), 1);
    let cat = cats[0].as_mob().expect("cats are mobs");
    assert!(cat.is_persistence_required(), "hut cats never despawn");
    assert!((cats[0].position() - DVec3::new(8.5, 65.0, 8.5)).length() < 1.0e-9);

    spawn_in_hut(world, spawn_pos);
    assert_eq!(cats_near(world).len(), 1, "one cat per hut");
}

#[test]
fn villages_need_five_claimed_beds_and_hold_up_to_five_cats() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("cat_village");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let spawn_pos = BlockPos::new(8, 65, 8);
    let home = vanilla_poi_types::HOME
        .try_id()
        .expect("vanilla home POI type should be registered");
    let claim_bed = |x: i32| {
        let bed = BlockPos::new(x, 65, 0);
        let mut storage = world.poi_storage.lock();
        storage.add(bed, home, vanilla_poi_types::HOME.ticket_count);
        assert!(storage.reserve_ticket(bed));
    };

    for x in 0..4 {
        claim_bed(x);
    }
    // An unclaimed bed does not count.
    world.poi_storage.lock().add(
        BlockPos::new(10, 65, 0),
        home,
        vanilla_poi_types::HOME.ticket_count,
    );
    spawn_in_village(world, spawn_pos);
    assert!(
        cats_near(world).is_empty(),
        "four claimed beds are not enough"
    );

    claim_bed(4);
    for _ in 0..7 {
        spawn_in_village(world, spawn_pos);
    }
    let cats = cats_near(world);
    assert_eq!(cats.len(), 5, "villages stop at five cats");
    assert!(cats.iter().all(|cat| {
        cat.as_mob()
            .is_some_and(|mob| !mob.is_persistence_required())
    }));
}

/// Vanilla `roll_village_siege`: tick 18000 of the overworld day.
const ROLL_VILLAGE_SIEGE_TICK: i64 = 18_000;
const DAY_TICKS: i64 = 24_000;

#[test]
fn the_siege_roll_happens_exactly_at_midnight() {
    init_vanilla_registry();
    let world_fixture = fresh_test_world("siege_marker");
    let world = &world_fixture.world;
    let clock = &vanilla_world_clocks::OVERWORLD;
    let marker = Identifier::vanilla_static("roll_village_siege");
    for (ticks, expected) in [
        (ROLL_VILLAGE_SIEGE_TICK, true),
        (ROLL_VILLAGE_SIEGE_TICK + DAY_TICKS, true),
        (ROLL_VILLAGE_SIEGE_TICK - 1, false),
        (ROLL_VILLAGE_SIEGE_TICK + 1, false),
    ] {
        assert_eq!(world.set_clock_total_ticks(clock, ticks), Some(()));
        assert_eq!(
            world.is_clock_at_time_marker(clock, &marker),
            expected,
            "{ticks}"
        );
    }
    assert!(!world.is_clock_at_time_marker(clock, &Identifier::vanilla_static("no_such_marker")));
}

#[test]
fn sieges_end_by_day_and_need_a_player_in_a_village() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world("siege_roll");
    let world = &world_fixture.world;
    world.set_difficulty(Difficulty::Normal);
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Villager", 1).build();
    player.snap_to(DVec3::new(8.0, 65.0, 8.0), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));

    let mut siege = VillageSiege::default();
    siege.siege_state = SiegeState::SiegeTonight;
    assert_eq!(
        world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, 6_000),
        Some(())
    );
    siege.tick_with(world, true, &mut StdRng::seed_from_u64(1));
    assert_eq!(
        siege.siege_state,
        SiegeState::SiegeDone,
        "daylight ends a siege"
    );

    assert_eq!(
        world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, ROLL_VILLAGE_SIEGE_TICK),
        Some(())
    );
    let mut tonight = 0;
    for seed in 0..200 {
        let mut siege = VillageSiege::default();
        siege.tick_with(world, true, &mut StdRng::seed_from_u64(seed));
        if siege.siege_state == SiegeState::SiegeTonight {
            tonight += 1;
            // No village around the player: nothing to besiege yet.
            assert_eq!(siege.zombies_to_spawn, 0);
        }
    }
    assert!(
        (5..=40).contains(&tonight),
        "about one night in ten: {tonight}"
    );
}

#[test]
fn a_siege_sends_twenty_zombies_into_the_village() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    // Without sky light the test chunks are dark enough for monsters.
    let world_fixture = fresh_test_world_with_dimension_type(
        "siege",
        "zombies",
        &vanilla_dimension_types::THE_NETHER,
    );
    let world = &world_fixture.world;
    world.set_difficulty(Difficulty::Normal);
    for x in -4..=3 {
        for z in -4..=3 {
            insert_ready_full_chunk(world, ChunkPos::new(x, z));
        }
    }
    let home = vanilla_poi_types::HOME
        .try_id()
        .expect("vanilla home POI type should be registered");
    for x in (-56..=56).step_by(16) {
        for z in (-56..=56).step_by(16) {
            let bed = BlockPos::new(x, 64, z);
            let mut storage = world.poi_storage.lock();
            storage.add(bed, home, vanilla_poi_types::HOME.ticket_count);
            assert!(storage.reserve_ticket(bed));
        }
    }
    for x in -60..60 {
        for z in -60..60 {
            assert!(world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            ));
        }
    }
    let player = TestPlayerBuilder::new(Arc::clone(world), "Defender", 1).build();
    player.snap_to(DVec3::new(0.5, 64.0, 0.5), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));

    let mut siege = VillageSiege::default();
    siege.siege_state = SiegeState::SiegeTonight;
    let mut random = StdRng::seed_from_u64(3);
    for _ in 0..100 {
        siege.tick_with(world, true, &mut random);
    }
    assert_eq!(siege.siege_state, SiegeState::SiegeDone);
    let origin = siege.spawn_pos;
    let horizontal = (f64::from(origin.x()).powi(2) + f64::from(origin.z()).powi(2)).sqrt();
    assert!((30.0..=34.0).contains(&horizontal), "{origin:?}");

    let zombies = world.get_entities_in_aabb_matching(
        &WorldAabb::new(-64.0, 0.0, -64.0, 64.0, 128.0, 64.0),
        |entity| entity.entity_type() == &vanilla_entities::ZOMBIE,
    );
    assert_eq!(zombies.len(), 20);
    for zombie in &zombies {
        let position = zombie.position();
        assert!((position.y - 64.0).abs() < 1.0e-9);
        assert!(
            (position.x - f64::from(origin.x())).abs() <= 8.5,
            "{position:?}"
        );
        assert!(
            (position.z - f64::from(origin.z())).abs() <= 8.5,
            "{position:?}"
        );
    }
}
