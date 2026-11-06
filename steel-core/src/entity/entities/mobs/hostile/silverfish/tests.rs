use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_damage_types, vanilla_entities,
    vanilla_game_rules,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{SilverfishEntity, outward_offsets};
use crate::behavior::init_behaviors;
use crate::entity::damage::DamageSource;
use crate::entity::entities::CowEntity;
use crate::entity::{
    Entity, LivingEntity, PathfinderMob, SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> SharedEntity {
    let entity: SharedEntity = Arc::new(entity);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test entity should attach to the loaded chunk");
    entity
}

fn silverfish(world: &Arc<World>, position: DVec3) -> SharedEntity {
    add(
        world,
        SilverfishEntity::new(
            &vanilla_entities::SILVERFISH,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        ),
    )
}

fn setup(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

#[test]
fn offsets_walk_outward_from_the_center() {
    let offsets: Vec<i32> = outward_offsets(2).collect();
    assert_eq!(offsets, vec![0, 1, -1, 2, -2]);
}

#[test]
fn silverfish_prefer_walking_over_stone() {
    let fixture = setup("silverfish_walk_value");
    let world = &fixture.world;
    world.set_block(
        BlockPos::new(4, 64, 4),
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    world.set_block(
        BlockPos::new(6, 64, 4),
        vanilla_blocks::DIRT.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let mob = SilverfishEntity::new(
        &vanilla_entities::SILVERFISH,
        1,
        DVec3::new(5.0, 65.0, 4.0),
        Arc::downgrade(world),
    );

    assert!((mob.get_walk_target_value(BlockPos::new(4, 65, 4)) - 10.0).abs() < f32::EPSILON);
    assert!(mob.get_walk_target_value(BlockPos::new(6, 65, 4)) < 10.0);
}

#[test]
fn hurting_a_silverfish_wakes_its_friends_in_infested_stone() {
    let fixture = setup("silverfish_wake_friends");
    let world = &fixture.world;
    // Dirt, so the silverfish cannot hide in the floor before waking friends.
    let floor = vanilla_blocks::DIRT.default_state();
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(BlockPos::new(x, 63, z), floor, UpdateFlags::UPDATE_ALL);
        }
    }
    let infested = BlockPos::new(10, 64, 8);
    world.set_block(
        infested,
        vanilla_blocks::INFESTED_STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let mob = silverfish(world, DVec3::new(8.5, 64.0, 8.5));
    let silverfish_ref = mob
        .downcast_ref::<SilverfishEntity>()
        .expect("silverfish is concrete");
    let attacker = add(
        world,
        CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(6.5, 64.0, 8.5),
            Arc::downgrade(world),
        ),
    );

    let source = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, Arc::clone(&attacker));
    assert!(silverfish_ref.hurt_server(world, &source, 1.0));
    assert_eq!(
        silverfish_ref.look_for_friends(),
        10,
        "adjustedTickDelay(20)"
    );

    for _ in 0..30 {
        Arc::clone(&mob).tick();
    }

    let state = world.get_block_state(infested);
    assert_ne!(
        state,
        vanilla_blocks::INFESTED_STONE.default_state(),
        "the infested stone is broken open"
    );
}

#[test]
fn friends_stay_as_stone_without_mob_griefing() {
    let fixture = setup("silverfish_wake_no_griefing");
    let world = &fixture.world;
    assert!(world.set_game_rule(&vanilla_game_rules::MOB_GRIEFING, false));
    let infested = BlockPos::new(9, 64, 8);
    world.set_block(
        infested,
        vanilla_blocks::INFESTED_COBBLESTONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let mob = silverfish(world, DVec3::new(8.5, 64.0, 8.5));
    let silverfish_ref = mob
        .downcast_ref::<SilverfishEntity>()
        .expect("silverfish is concrete");
    silverfish_ref.notify_hurt();

    for _ in 0..30 {
        Arc::clone(&mob).tick();
    }

    assert_eq!(
        world.get_block_state(infested),
        vanilla_blocks::COBBLESTONE.default_state(),
        "without mob griefing the block turns back into its host"
    );
}

#[test]
fn silverfish_merges_into_neighboring_stone() {
    let fixture = setup("silverfish_merge");
    let world = &fixture.world;
    let stone = vanilla_blocks::STONE.default_state();
    // Box the silverfish in stone on every side.
    for x in 7..10 {
        for y in 63..66 {
            for z in 7..10 {
                world.set_block(BlockPos::new(x, y, z), stone, UpdateFlags::UPDATE_ALL);
            }
        }
    }
    world.set_block(
        BlockPos::new(8, 64, 8),
        vanilla_blocks::AIR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let mob = silverfish(world, DVec3::new(8.5, 64.0, 8.5));

    for _ in 0..400 {
        world.level_data.write().advance_game_time();
        Arc::clone(&mob).tick();
        if mob.is_removed() {
            break;
        }
    }

    assert!(mob.is_removed(), "the silverfish hides in the stone");
    let infested_neighbors = [
        BlockPos::new(7, 64, 8),
        BlockPos::new(9, 64, 8),
        BlockPos::new(8, 63, 8),
        BlockPos::new(8, 65, 8),
        BlockPos::new(8, 64, 7),
        BlockPos::new(8, 64, 9),
    ]
    .into_iter()
    .filter(|pos| world.get_block_state(*pos) == vanilla_blocks::INFESTED_STONE.default_state())
    .count();
    assert_eq!(infested_neighbors, 1, "one neighbor becomes infested stone");
}

#[test]
fn silverfish_body_follows_its_head() {
    init_vanilla_registry();
    let mob = SilverfishEntity::new(&vanilla_entities::SILVERFISH, 1, DVec3::ZERO, Weak::new());
    mob.set_y_body_rot(90.0);
    assert!((mob.rotation().0 - 90.0).abs() < f32::EPSILON);
}
