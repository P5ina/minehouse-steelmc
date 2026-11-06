use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_entities};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::goals::PhantomCircleAroundAnchorGoal;
use super::{AttackPhase, PhantomEntity};
use crate::entity::ai::goal::Goal;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity as _, Mob, SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

fn phantom_at(position: DVec3) -> PhantomEntity {
    PhantomEntity::new(&vanilla_entities::PHANTOM, 1, position, Weak::new())
}

#[test]
fn size_grows_the_box_and_the_bite() {
    init_vanilla_registry();
    let phantom = phantom_at(DVec3::ZERO);
    let base_width = vanilla_entities::PHANTOM.dimensions.width;
    phantom.set_phantom_size(4);
    assert_eq!(phantom.phantom_size(), 4);
    let bounding_box = phantom.bounding_box();
    let width = bounding_box.max_x() - bounding_box.min_x();
    assert!((width - f64::from(base_width * 1.6)).abs() < 1.0e-5);
    assert!(
        (phantom
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE)
            - 10.0)
            .abs()
            < 1.0e-9
    );
    phantom.set_phantom_size(100);
    assert_eq!(phantom.phantom_size(), 64, "the size is clamped to 64");
}

#[test]
fn anchor_and_size_round_trip() {
    init_vanilla_registry();
    let phantom = phantom_at(DVec3::ZERO);
    phantom.state().lock().anchor_point = Some(BlockPos::new(3, 90, -7));
    phantom.set_phantom_size(2);
    let mut nbt = NbtCompound::new();
    phantom.save_additional(&mut nbt);
    assert_eq!(nbt.int("size"), Some(2));

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = phantom_at(DVec3::ZERO);
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.phantom_size(), 2);
    assert_eq!(loaded.anchor_point(), Some(BlockPos::new(3, 90, -7)));
}

#[test]
fn a_fresh_spawn_anchors_five_blocks_up() {
    init_vanilla_registry();
    let world_fixture = fresh_test_world("phantom_finalize");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let phantom = PhantomEntity::new(
        &vanilla_entities::PHANTOM,
        next_entity_id(),
        DVec3::new(4.5, 80.0, 4.5),
        Arc::downgrade(world),
    );
    phantom.set_phantom_size(3);
    let _ = phantom.finalize_spawn(world, EntitySpawnReason::Natural, None);
    assert_eq!(phantom.anchor_point(), Some(BlockPos::new(4, 85, 4)));
    assert_eq!(phantom.phantom_size(), 0);
}

#[test]
fn the_move_control_turns_four_degrees_and_eases_its_velocity() {
    init_vanilla_registry();
    let phantom = phantom_at(DVec3::new(0.0, 100.0, 0.0));
    phantom.set_rotation((0.0, 0.0));
    phantom.state().lock().move_target_point = DVec3::new(10.0, 100.0, 0.0);
    phantom.tick_move_control();
    // Facing +Z (yaw 0), the target lies at yaw -90: a 4 degree turn.
    assert!((phantom.rotation().0 + 4.0).abs() < 1.0e-4);
    assert!((phantom.y_body_rot() + 4.0).abs() < 1.0e-4);
    let velocity = phantom.velocity();
    assert!(velocity.x > 0.0, "flies toward the target");
    assert!(velocity.y.abs() < 1.0e-9, "level flight");
}

#[test]
fn circling_picks_points_around_the_anchor() {
    init_vanilla_registry();
    init_entities();
    let world_fixture = fresh_test_world("phantom_circle");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let entity: SharedEntity = Arc::new(PhantomEntity::new(
        &vanilla_entities::PHANTOM,
        next_entity_id(),
        DVec3::new(8.0, 120.0, 8.0),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("phantom should attach to the loaded chunk");
    let phantom = entity
        .downcast_ref::<PhantomEntity>()
        .expect("phantom is concrete");
    phantom.state().lock().anchor_point = Some(BlockPos::new(8, 120, 8));

    let mut goal = PhantomCircleAroundAnchorGoal::new();
    assert!(goal.can_use(phantom));
    goal.start(phantom);
    let target = phantom.move_target_point();
    let horizontal = (target.x - 8.0).hypot(target.z - 8.0);
    assert!((5.0..=15.0).contains(&horizontal), "distance {horizontal}");
    assert!((112.0..=121.0).contains(&target.y), "height {}", target.y);
    assert_eq!(phantom.state().lock().attack_phase, AttackPhase::Circle);
}

#[test]
fn flaps_once_every_twenty_five_ticks() {
    init_vanilla_registry();
    let phantom = phantom_at(DVec3::ZERO);
    let flaps = (0..100)
        .filter(|&tick| {
            phantom.base().set_tick_count(tick);
            phantom.is_flapping()
        })
        .count();
    assert_eq!(flaps, 4);
}
