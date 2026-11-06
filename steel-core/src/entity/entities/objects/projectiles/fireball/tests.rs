use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{LargeFireballEntity, SmallFireballEntity};
use crate::behavior::init_behaviors;
use crate::entity::{
    AbstractHurtingProjectile, Entity, INITIAL_ACCELERATION_POWER, Projectile,
    ProjectileDeflection, SharedEntity, next_entity_id,
};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn spawn_small_fireball(world: &Arc<World>, position: DVec3, direction: DVec3) -> SharedEntity {
    let entity: SharedEntity = Arc::new(SmallFireballEntity::new(
        &vanilla_entities::SMALL_FIREBALL,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("fireball should attach to the loaded chunk");
    entity
        .downcast_ref::<SmallFireballEntity>()
        .expect("small fireball is concrete")
        .assign_directional_movement(direction);
    entity
}

#[test]
fn fireballs_accelerate_along_their_heading_and_burn() {
    init_vanilla_registry();
    let world_fixture = fresh_test_world("fireball_flight");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity = spawn_small_fireball(
        world,
        DVec3::new(8.0, 100.0, 8.0),
        DVec3::new(3.0, 0.0, 0.0),
    );
    let fireball = entity
        .downcast_ref::<SmallFireballEntity>()
        .expect("small fireball is concrete");
    assert!((fireball.velocity().x - INITIAL_ACCELERATION_POWER).abs() < 1.0e-12);

    let start = fireball.position();
    Arc::clone(&entity).tick();
    let expected_speed = (0.1 + 0.1) * f64::from(0.95_f32);
    assert!((fireball.velocity().x - expected_speed).abs() < 1.0e-12);
    assert!((fireball.position().x - (start.x + expected_speed)).abs() < 1.0e-9);
    assert!(fireball.velocity().y.abs() < 1.0e-12, "no gravity");
    assert!(fireball.is_on_fire());
    // Flying along +X faces yaw 90 (atan2(0, x) + 90), lerped 20% from 0.
    assert!((fireball.rotation().0 - 0.2 * 90.0_f32).abs() < 1.0e-4);
}

#[test]
fn a_small_fireball_sets_the_block_face_it_hits_on_fire() {
    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world("fireball_ignite");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let floor = BlockPos::new(8, 64, 8);
    world.set_block(
        floor,
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );

    let entity = spawn_small_fireball(world, DVec3::new(8.5, 65.05, 8.5), DVec3::NEG_Y);
    Arc::clone(&entity).tick();
    assert!(entity.is_removed(), "the impact discards the fireball");
    let above = world.get_block_state(floor.above());
    assert_eq!(above.get_block(), &vanilla_blocks::FIRE);
}

#[test]
fn deflection_by_attack_resets_acceleration_and_other_deflections_halve_it() {
    init_vanilla_registry();
    let fireball =
        LargeFireballEntity::new(&vanilla_entities::FIREBALL, 1, DVec3::ZERO, Weak::new());
    fireball.set_acceleration_power(0.4);
    fireball.deflect(ProjectileDeflection::Reverse, None, None, false);
    assert!((fireball.acceleration_power() - 0.2).abs() < 1.0e-12);
    fireball.deflect(ProjectileDeflection::Reverse, None, None, true);
    assert!((fireball.acceleration_power() - INITIAL_ACCELERATION_POWER).abs() < 1.0e-12);
}

#[test]
fn fireballs_can_be_hit_but_take_no_damage() {
    init_vanilla_registry();
    let fireball =
        LargeFireballEntity::new(&vanilla_entities::FIREBALL, 1, DVec3::ZERO, Weak::new());
    assert!(
        fireball.is_pickable(),
        "fireballs are redirectable projectiles"
    );
    let small = SmallFireballEntity::new(
        &vanilla_entities::SMALL_FIREBALL,
        2,
        DVec3::ZERO,
        Weak::new(),
    );
    assert!(
        !small.is_pickable(),
        "small fireballs are not in the redirectable tag"
    );
}

#[test]
fn large_fireball_round_trips_power_acceleration_and_item() {
    init_vanilla_registry();
    let fireball =
        LargeFireballEntity::new(&vanilla_entities::FIREBALL, 1, DVec3::ZERO, Weak::new());
    fireball.set_explosion_power(3);
    fireball.set_acceleration_power(0.25);
    let mut nbt = NbtCompound::new();
    fireball.save_additional(&mut nbt);
    assert_eq!(nbt.byte("ExplosionPower"), Some(3));
    assert!(nbt.compound("Item").is_some(), "the fire charge is saved");

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = LargeFireballEntity::new(&vanilla_entities::FIREBALL, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.explosion_power(), 3);
    assert!((loaded.acceleration_power() - 0.25).abs() < 1.0e-12);

    let defaults =
        LargeFireballEntity::new(&vanilla_entities::FIREBALL, 3, DVec3::ZERO, Weak::new());
    let empty = NbtCompound::new();
    let mut bytes = Vec::new();
    empty.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    defaults.load_additional((&borrowed).into());
    assert_eq!(defaults.explosion_power(), 1);
    assert!((defaults.acceleration_power() - INITIAL_ACCELERATION_POWER).abs() < 1.0e-12);
}
