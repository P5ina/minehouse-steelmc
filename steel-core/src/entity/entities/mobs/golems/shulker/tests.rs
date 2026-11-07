use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::dye_color::DyeColor;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities};
use steel_utils::axis::Axis;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Direction};

use super::{ShulkerEntity, physical_peek, progress_aabb};
use crate::behavior::init_behaviors;
use crate::entity::entities::ShulkerBulletEntity;
use crate::entity::{Entity, LivingEntity as _, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};

fn detached_shulker(position: DVec3) -> ShulkerEntity {
    ShulkerEntity::new(&vanilla_entities::SHULKER, 1, position, Weak::new())
}

/// A world with one loaded chunk and a stone floor at y = 63.
fn floor_world(name: &'static str) -> TestWorld {
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
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    world_fixture
}

#[test]
fn snaps_to_the_block_bottom_center() {
    init_vanilla_registry();
    let shulker = detached_shulker(DVec3::new(3.2, 64.4, -1.7));
    assert_eq!(shulker.position(), DVec3::new(3.5, 64.0, -1.5));
    let shulker = detached_shulker(DVec3::new(3.2, 64.6, -1.7));
    assert_eq!(shulker.position(), DVec3::new(3.5, 65.0, -1.5));
}

#[test]
fn closing_adds_covered_armor() {
    init_vanilla_registry();
    let shulker = detached_shulker(DVec3::ZERO);
    let armor = || {
        shulker
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ARMOR)
    };
    assert!(shulker.is_closed());
    shulker.set_raw_peek_amount(100);
    assert!(!shulker.is_closed());
    let open_armor = armor();
    shulker.set_raw_peek_amount(0);
    assert!((armor() - open_armor - 20.0).abs() < 1.0e-9);
    shulker.set_raw_peek_amount(30);
    assert!((armor() - open_armor).abs() < 1.0e-9);
}

#[test]
fn the_lid_grows_the_box_away_from_the_attach_face() {
    assert!(physical_peek(0.0).abs() < 1.0e-6);
    assert!((physical_peek(1.0) - 1.0).abs() < 1.0e-6);

    let closed = progress_aabb(1.0, Direction::Up, 0.0, DVec3::new(0.5, 64.0, 0.5));
    assert!((closed.min_y() - 64.0).abs() < 1.0e-9);
    assert!((closed.max_y() - 65.0).abs() < 1.0e-9);
    let open = progress_aabb(1.0, Direction::Up, 1.0, DVec3::new(0.5, 64.0, 0.5));
    assert!((open.min_y() - 64.0).abs() < 1.0e-9);
    assert!((open.max_y() - 66.0).abs() < 1.0e-9);
    let ceiling = progress_aabb(1.0, Direction::Down, 1.0, DVec3::new(0.5, 64.0, 0.5));
    assert!((ceiling.min_y() - 63.0).abs() < 1.0e-9);
    assert!((ceiling.max_y() - 65.0).abs() < 1.0e-9);
}

#[test]
fn eases_the_lid_toward_the_synced_peek() {
    init_vanilla_registry();
    let shulker = detached_shulker(DVec3::new(0.5, 64.0, 0.5));
    shulker.set_raw_peek_amount(30);
    for _ in 0..6 {
        assert!(shulker.update_peek_amount());
    }
    assert!((shulker.current_peek_amount() - 0.3).abs() < 1.0e-6);
    assert!(!shulker.update_peek_amount());
}

#[test]
fn stays_on_a_floor_but_not_in_the_air() {
    let world_fixture = floor_world("shulker_attachment");
    let world = &world_fixture.world;
    let shulker = Arc::new(ShulkerEntity::new(
        &vanilla_entities::SHULKER,
        next_entity_id(),
        DVec3::new(4.5, 64.0, 4.5),
        Arc::downgrade(world),
    ));
    let floor = BlockPos::new(4, 64, 4);
    assert!(shulker.can_stay_at(floor, Direction::Down));
    assert!(!shulker.can_stay_at(floor, Direction::Up));
    assert!(!shulker.can_stay_at(BlockPos::new(4, 70, 4), Direction::Down));
    assert_eq!(
        shulker.find_attachable_surface(floor),
        Some(Direction::Down)
    );
}

#[test]
fn falls_back_to_a_wall_when_the_floor_is_gone() {
    let world_fixture = floor_world("shulker_find_wall");
    let world = &world_fixture.world;
    world.set_block(
        BlockPos::new(4, 63, 4),
        vanilla_blocks::AIR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    world.set_block(
        BlockPos::new(4, 64, 3),
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let shulker = Arc::new(ShulkerEntity::new(
        &vanilla_entities::SHULKER,
        next_entity_id(),
        DVec3::new(4.5, 64.0, 4.5),
        Arc::downgrade(world),
    ));
    shulker.find_new_attachment();
    assert_eq!(shulker.attach_face(), Direction::North);
}

#[test]
fn ignores_velocity() {
    init_vanilla_registry();
    let shulker = detached_shulker(DVec3::new(0.5, 64.0, 0.5));
    shulker.set_velocity(DVec3::new(1.0, 2.0, 3.0));
    assert_eq!(shulker.velocity(), DVec3::ZERO);
}

#[test]
fn attach_face_and_color_round_trip() {
    init_vanilla_registry();
    let shulker = detached_shulker(DVec3::ZERO);
    shulker.set_attach_face(Direction::East);
    shulker.set_color(Some(DyeColor::by_id(14)));
    let mut nbt = NbtCompound::new();
    shulker.save_additional(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = detached_shulker(DVec3::ZERO);
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.attach_face(), Direction::East);
    assert_eq!(loaded.color().map(DyeColor::id), Some(14));
}

#[test]
fn fires_a_bullet_that_picks_a_heading() {
    let world_fixture = floor_world("shulker_bullet_heading");
    let world = &world_fixture.world;
    let shulker: SharedEntity = Arc::new(ShulkerEntity::new(
        &vanilla_entities::SHULKER,
        next_entity_id(),
        DVec3::new(4.5, 64.0, 4.5),
        Arc::downgrade(world),
    ));
    let target: SharedEntity = Arc::new(ShulkerEntity::new(
        &vanilla_entities::SHULKER,
        next_entity_id(),
        DVec3::new(12.5, 64.0, 12.5),
        Arc::downgrade(world),
    ));
    let bullet = ShulkerBulletEntity::fired_by(world, &shulker, &target, Axis::Y);
    let heading = bullet.move_direction();
    assert!(heading.is_some_and(|direction| direction.axis() != Axis::Y));
}
