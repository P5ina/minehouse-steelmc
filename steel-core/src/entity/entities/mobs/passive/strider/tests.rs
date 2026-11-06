use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos};

use super::StriderEntity;
use crate::behavior::init_behaviors;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity as _, PathfinderMob as _, init_entities, next_entity_id,
};
use crate::physics::MoverType;
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};

/// A world with a 16×16 lava pool at y = 63.
fn lava_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world(name);
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::LAVA.default_state(),
                UpdateFlags::UPDATE_NONE,
            );
        }
    }
    world_fixture
}

#[test]
fn walks_on_the_lava_surface() {
    let world_fixture = lava_world("strider_lava_floor");
    let world = &world_fixture.world;
    let strider = Arc::new(StriderEntity::new(
        &vanilla_entities::STRIDER,
        next_entity_id(),
        DVec3::new(8.5, 63.75, 8.5),
        Arc::downgrade(world),
    ));
    let _ = Arc::clone(&strider).move_entity(MoverType::SelfMovement, DVec3::new(0.0, -0.5, 0.0));
    let y = strider.position().y;
    assert!((y - 63.5).abs() < 1.0e-6, "rests on the lava shape: {y}");
    assert!(strider.on_ground());
}

#[test]
fn suffocating_slows_it_down() {
    init_vanilla_registry();
    let strider = StriderEntity::new(&vanilla_entities::STRIDER, 1, DVec3::ZERO, Weak::new());
    let speed = || {
        strider
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED)
    };
    let base_speed = speed();
    strider.set_suffocating(true);
    assert!(strider.is_suffocating());
    assert!((speed() - base_speed * 0.66).abs() < 1.0e-9);
    strider.set_suffocating(false);
    assert!(!strider.is_suffocating());
    assert!((speed() - base_speed).abs() < 1.0e-9);
}

#[test]
fn spawns_only_below_air() {
    let world_fixture = lava_world("strider_spawn_rule");
    let world = &world_fixture.world;
    let rule = StriderEntity::SPAWN_RULE;
    let pos = BlockPos::new(4, 63, 4);
    assert!(rule(
        &vanilla_entities::STRIDER,
        world.as_ref(),
        EntitySpawnReason::Natural,
        pos,
        &mut rand::rng(),
    ));
    world.set_block(
        pos.above(),
        vanilla_blocks::NETHERRACK.default_state(),
        UpdateFlags::UPDATE_NONE,
    );
    assert!(!rule(
        &vanilla_entities::STRIDER,
        world.as_ref(),
        EntitySpawnReason::Natural,
        pos,
        &mut rand::rng(),
    ));
}

#[test]
fn prefers_lava_when_wandering() {
    let world_fixture = lava_world("strider_walk_target");
    let world = &world_fixture.world;
    world.set_block(
        BlockPos::new(2, 63, 2),
        vanilla_blocks::NETHERRACK.default_state(),
        UpdateFlags::UPDATE_NONE,
    );
    let strider = StriderEntity::new(
        &vanilla_entities::STRIDER,
        next_entity_id(),
        DVec3::new(8.5, 70.0, 8.5),
        Arc::downgrade(world),
    );
    assert_eq!(
        strider.get_walk_target_value_in(BlockPos::new(4, 63, 4), world.as_ref()),
        10.0
    );
    assert_eq!(
        strider.get_walk_target_value_in(BlockPos::new(2, 63, 2), world.as_ref()),
        0.0
    );
    assert!(strider.is_stable_destination(BlockPos::new(4, 63, 4)));
}
