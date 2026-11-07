//! Creaking tests.

use std::io::Cursor;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, CreakingHeartState};
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_damage_types, vanilla_entities,
};
use steel_utils::axis::Axis;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Downcast as _};

use super::CreakingEntity;
use crate::behavior::init_behaviors;
use crate::block_entity::entities::{CreakingHeartBlockEntity, creaking_heart_has_required_logs};
use crate::block_entity::{SharedBlockEntity, init_block_entities};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ZombieEntity;
use crate::entity::{Entity, LivingEntity, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

const HEART_POS: BlockPos = BlockPos::new(4, 3, 4);

fn loaded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    init_block_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

fn spawn_creaking(world: &Arc<World>, position: DVec3) -> Arc<CreakingEntity> {
    let creaking = Arc::new(CreakingEntity::new(
        &vanilla_entities::CREAKING,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&creaking) as SharedEntity)
        .expect("creaking should attach to the loaded chunk");
    creaking
}

fn spawn_zombie(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let zombie: SharedEntity = Arc::new(ZombieEntity::new(
        &vanilla_entities::ZOMBIE,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&zombie))
        .expect("zombie should attach to the loaded chunk");
    zombie
}

fn awake_heart() -> BlockStateId {
    vanilla_blocks::CREAKING_HEART.default_state().set_value(
        &BlockStateProperties::CREAKING_HEART_STATE,
        CreakingHeartState::Awake,
    )
}

/// A vertical pale oak trunk with an awake heart in the middle.
fn place_heart(world: &Arc<World>) -> SharedBlockEntity {
    let log = vanilla_blocks::PALE_OAK_LOG.default_state();
    world.set_block(HEART_POS.below(), log, UpdateFlags::UPDATE_ALL);
    world.set_block(HEART_POS.above(), log, UpdateFlags::UPDATE_ALL);
    world.set_block(HEART_POS, awake_heart(), UpdateFlags::UPDATE_ALL);
    world
        .get_block_entity(HEART_POS)
        .expect("the heart block should create its block entity")
}

fn heart(block_entity: &SharedBlockEntity) -> &CreakingHeartBlockEntity {
    block_entity
        .downcast_ref::<CreakingHeartBlockEntity>()
        .expect("block entity should be a creaking heart")
}

fn bind(world: &Arc<World>, heart: &CreakingHeartBlockEntity) -> Arc<CreakingEntity> {
    let creaking = spawn_creaking(world, DVec3::new(6.5, 1.0, 6.5));
    creaking.set_transient(HEART_POS);
    heart.set_creaking(Arc::clone(&creaking) as SharedEntity);
    creaking
}

#[test]
fn heart_needs_pale_oak_logs_on_its_axis() {
    let fixture = loaded_world("creaking_heart_logs");
    let world = &fixture.world;
    let heart = awake_heart();
    assert!(!creaking_heart_has_required_logs(
        heart,
        world.as_ref(),
        HEART_POS
    ));

    let log = vanilla_blocks::PALE_OAK_LOG.default_state();
    world.set_block(HEART_POS.below(), log, UpdateFlags::UPDATE_ALL);
    assert!(!creaking_heart_has_required_logs(
        heart,
        world.as_ref(),
        HEART_POS
    ));
    world.set_block(HEART_POS.above(), log, UpdateFlags::UPDATE_ALL);
    assert!(creaking_heart_has_required_logs(
        heart,
        world.as_ref(),
        HEART_POS
    ));

    let sideways = log.set_value(&BlockStateProperties::AXIS, Axis::X);
    world.set_block(HEART_POS.above(), sideways, UpdateFlags::UPDATE_ALL);
    assert!(!creaking_heart_has_required_logs(
        heart,
        world.as_ref(),
        HEART_POS
    ));

    let oak = vanilla_blocks::OAK_LOG.default_state();
    world.set_block(HEART_POS.above(), oak, UpdateFlags::UPDATE_ALL);
    assert!(!creaking_heart_has_required_logs(
        heart,
        world.as_ref(),
        HEART_POS
    ));
}

#[test]
fn heart_bound_creaking_takes_no_damage() {
    let fixture = loaded_world("creaking_heart_bound_hurt");
    let world = &fixture.world;
    let block_entity = place_heart(world);
    let heart = heart(&block_entity);
    let creaking = bind(world, heart);
    assert!(heart.is_protector(world, creaking.id()));
    let zombie = spawn_zombie(world, DVec3::new(7.5, 1.0, 6.5));
    let health = creaking.get_health();

    let source = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, zombie);
    assert!(creaking.hurt_server(world, &source, 5.0));
    assert!((creaking.get_health() - health).abs() < f32::EPSILON);
    // The flinch animation makes it ignore hits for a while.
    assert!(!creaking.hurt_server(world, &source, 5.0));
}

#[test]
fn free_creaking_takes_damage() {
    let fixture = loaded_world("creaking_free_hurt");
    let world = &fixture.world;
    let creaking = spawn_creaking(world, DVec3::new(6.5, 1.0, 6.5));
    let zombie = spawn_zombie(world, DVec3::new(7.5, 1.0, 6.5));
    let health = creaking.get_health();

    let source = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, zombie);
    assert!(creaking.hurt_server(world, &source, 5.0));
    assert!(creaking.get_health() < health);
}

#[test]
fn environment_damage_does_not_flinch_a_bound_creaking() {
    let fixture = loaded_world("creaking_heart_bound_environment");
    let world = &fixture.world;
    let block_entity = place_heart(world);
    let heart = heart(&block_entity);
    let creaking = bind(world, heart);
    let health = creaking.get_health();

    let source = DamageSource::environment(&vanilla_damage_types::CACTUS);
    assert!(!creaking.hurt_server(world, &source, 5.0));
    assert!((creaking.get_health() - health).abs() < f32::EPSILON);
}

#[test]
fn breaking_the_heart_kills_its_creaking() {
    let fixture = loaded_world("creaking_heart_removed");
    let world = &fixture.world;
    let block_entity = place_heart(world);
    let heart = heart(&block_entity);
    let creaking = bind(world, heart);

    let source = DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK);
    heart.remove_protector(world, Some(&source));
    assert!(creaking.is_tearing_down());
    assert!(creaking.get_health() <= 0.0);
    assert!(!heart.is_protector(world, creaking.id()));
}

#[test]
fn creaking_dies_once_its_heart_is_gone() {
    let fixture = loaded_world("creaking_heart_missing");
    let world = &fixture.world;
    let creaking = spawn_creaking(world, DVec3::new(6.5, 1.0, 6.5));
    creaking.set_transient(HEART_POS);
    Arc::clone(&creaking).tick();
    assert!(creaking.get_health() <= 0.0);
}

#[test]
fn creaking_is_frozen_until_it_can_move() {
    let fixture = loaded_world("creaking_frozen");
    let world = &fixture.world;
    let creaking = spawn_creaking(world, DVec3::new(6.5, 1.0, 6.5));
    // No player is around, so nothing stops it.
    assert!(creaking.check_can_move());
    assert!(!creaking.is_active());
    assert!(creaking.is_pushable());
}

#[test]
fn home_pos_round_trips() {
    let fixture = loaded_world("creaking_home_save");
    let world = &fixture.world;
    let creaking = spawn_creaking(world, DVec3::new(6.5, 1.0, 6.5));
    creaking.set_transient(BlockPos::new(3, -4, 12));
    let mut saved = NbtCompound::new();
    creaking.save_additional(&mut saved);

    let loaded = spawn_creaking(world, DVec3::new(2.5, 1.0, 2.5));
    let bytes = {
        let mut out = Vec::new();
        saved.write(&mut out);
        out
    };
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("saved creaking nbt should reborrow: {error}"));
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.home_pos(), Some(BlockPos::new(3, -4, 12)));
    assert!(loaded.is_heart_bound());
}
