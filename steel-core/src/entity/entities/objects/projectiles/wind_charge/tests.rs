//! Wind charge tests.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{AttachFace, BlockStateProperties};
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{init_vanilla_registry, sound_events, vanilla_blocks, vanilla_entities};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{BreezeWindChargeEntity, WindChargeEntity, explode_wind_burst};
use crate::behavior::init_behaviors;
use crate::entity::entities::PigEntity;
use crate::entity::{
    AbstractHurtingProjectile, Entity, Projectile, ProjectileDeflection, RemovalReason,
    SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::{WindChargeExplosionDamageCalculator, World};

fn loaded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

fn spawn_wind_charge(world: &Arc<World>, position: DVec3, velocity: DVec3) -> SharedEntity {
    let entity: SharedEntity = Arc::new(WindChargeEntity::new(
        &vanilla_entities::WIND_CHARGE,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("wind charge should attach to the loaded chunk");
    entity.set_velocity(velocity);
    entity
}

fn spawn_pig(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let pig: SharedEntity = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&pig))
        .expect("pig should attach to the loaded chunk");
    pig
}

fn player_burst(world: &Arc<World>, center: DVec3) {
    let source = spawn_wind_charge(world, center, DVec3::ZERO);
    let charge = source
        .downcast_ref::<WindChargeEntity>()
        .expect("wind charge is concrete");
    charge.set_removed(RemovalReason::Discarded);
    explode_wind_burst(
        world,
        Arc::clone(&source),
        &WindChargeExplosionDamageCalculator,
        center,
        1.2,
        &sound_events::ENTITY_WIND_CHARGE_WIND_BURST,
    );
}

#[test]
fn wind_charges_fly_straight_without_speeding_up_or_burning() {
    let fixture = loaded_world("wind_charge_flight");
    let world = &fixture.world;
    let entity = spawn_wind_charge(
        world,
        DVec3::new(8.0, 100.0, 8.0),
        DVec3::new(0.5, 0.0, 0.0),
    );
    let charge = entity
        .downcast_ref::<WindChargeEntity>()
        .expect("wind charge is concrete");
    assert_eq!(charge.acceleration_power(), 0.0);
    let box_bottom = charge.bounding_box().min_y();
    assert!((box_bottom - (100.0 - f64::from(0.15_f32))).abs() < 1.0e-9);

    Arc::clone(&entity).tick();
    assert!((charge.velocity().x - 0.5).abs() < 1.0e-12, "inertia is 1");
    assert!((charge.position().x - 8.5).abs() < 1.0e-9);
    assert!(!charge.is_on_fire());
    assert!(
        (charge.bounding_box().min_y() - (100.0 - f64::from(0.15_f32))).abs() < 1.0e-9,
        "the box stays lowered after moving"
    );
}

#[test]
fn a_fresh_wind_charge_cannot_be_deflected() {
    let fixture = loaded_world("wind_charge_no_deflect");
    let world = &fixture.world;
    let entity = spawn_wind_charge(
        world,
        DVec3::new(8.0, 100.0, 8.0),
        DVec3::new(0.2, 0.0, 0.0),
    );
    let charge = entity
        .downcast_ref::<WindChargeEntity>()
        .expect("wind charge is concrete");
    assert!(!charge.deflect(ProjectileDeflection::Reverse, None, None, false));
    assert!(charge.velocity().x > 0.0);
    for _ in 0..5 {
        Arc::clone(&entity).tick();
    }
    assert!(charge.deflect(ProjectileDeflection::Reverse, None, None, false));
    assert!(charge.velocity().x < 0.0);
}

#[test]
fn a_wind_charge_bursts_on_the_block_it_hits_and_opens_a_wooden_trapdoor() {
    let fixture = loaded_world("wind_charge_block_hit");
    let world = &fixture.world;
    let wall = BlockPos::new(8, 64, 8);
    world.set_block(
        wall,
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let trapdoor = BlockPos::new(8, 64, 7);
    world.set_block(
        trapdoor,
        vanilla_blocks::OAK_TRAPDOOR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let iron_trapdoor = BlockPos::new(7, 64, 8);
    world.set_block(
        iron_trapdoor,
        vanilla_blocks::IRON_TRAPDOOR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );

    let entity = spawn_wind_charge(
        world,
        DVec3::new(8.5, 66.5, 8.5),
        DVec3::new(0.0, -1.0, 0.0),
    );
    for _ in 0..3 {
        if entity.is_removed() {
            break;
        }
        Arc::clone(&entity).tick();
    }

    assert!(entity.is_removed(), "the charge bursts on the stone");
    assert_eq!(
        world.get_block_state(wall).get_block(),
        &vanilla_blocks::STONE
    );
    assert!(
        world
            .get_block_state(trapdoor)
            .get_value(&BlockStateProperties::OPEN)
    );
    assert!(
        !world
            .get_block_state(iron_trapdoor)
            .get_value(&BlockStateProperties::OPEN)
    );
}

#[test]
fn a_wind_burst_pushes_mobs_without_hurting_them() {
    let fixture = loaded_world("wind_charge_push");
    let world = &fixture.world;
    let pig = spawn_pig(world, DVec3::new(9.0, 64.0, 8.5));
    let health = pig.as_living_entity().expect("pig is living").get_health();

    player_burst(world, DVec3::new(8.0, 64.0, 8.5));

    assert!(pig.velocity().x > 0.0, "pushed away from the burst");
    let living = pig.as_living_entity().expect("pig is living");
    assert!((living.get_health() - health).abs() < f32::EPSILON);
}

#[test]
fn wind_bursts_flip_levers_and_blow_out_candles() {
    let fixture = loaded_world("wind_charge_triggers");
    let world = &fixture.world;
    for x in 6..=10 {
        world.set_block(
            BlockPos::new(x, 63, 8),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
    }
    let lever = BlockPos::new(7, 64, 8);
    world.set_block(
        lever,
        vanilla_blocks::LEVER
            .default_state()
            .set_value(&BlockStateProperties::ATTACH_FACE, AttachFace::Floor),
        UpdateFlags::UPDATE_ALL,
    );
    let candle = BlockPos::new(9, 64, 8);
    world.set_block(
        candle,
        vanilla_blocks::CANDLE
            .default_state()
            .set_value(&BlockStateProperties::LIT, true),
        UpdateFlags::UPDATE_ALL,
    );

    player_burst(world, DVec3::new(8.5, 64.5, 8.5));

    assert!(
        world
            .get_block_state(lever)
            .get_value(&BlockStateProperties::POWERED)
    );
    assert!(
        !world
            .get_block_state(candle)
            .get_value(&BlockStateProperties::LIT)
    );
}

#[test]
fn a_breeze_charge_leaves_blocks_alone_without_mob_griefing() {
    let fixture = loaded_world("breeze_charge_no_grief");
    let world = &fixture.world;
    assert!(world.set_game_rule(&MOB_GRIEFING, false));
    let trapdoor = BlockPos::new(8, 64, 8);
    world.set_block(
        trapdoor,
        vanilla_blocks::OAK_TRAPDOOR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let entity: SharedEntity = Arc::new(BreezeWindChargeEntity::new(
        &vanilla_entities::BREEZE_WIND_CHARGE,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    ));
    explode_wind_burst(
        world,
        entity,
        &WindChargeExplosionDamageCalculator,
        DVec3::new(8.5, 65.0, 8.5),
        3.0,
        &sound_events::ENTITY_BREEZE_WIND_BURST,
    );
    assert!(
        !world
            .get_block_state(trapdoor)
            .get_value(&BlockStateProperties::OPEN)
    );
}
