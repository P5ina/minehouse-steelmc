//! Breeze tests.

use std::io::Cursor;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_data::EntityPose;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_damage_types, vanilla_entities,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _, WorldAabb};

use super::BreezeEntity;
use super::ai::{LongJump, Shoot};
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, BREEZE_JUMP_TARGET, BREEZE_SHOOT, BREEZE_SHOOT_CHARGING,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::{
    ArrowEntity, BreezeWindChargeEntity, IronGolemEntity, PigEntity, WindChargeEntity,
};
use crate::entity::{
    Entity, LivingEntity, Mob, ProjectileDeflection, SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn loaded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

fn spawn_breeze(world: &Arc<World>, position: DVec3) -> Arc<BreezeEntity> {
    let breeze = Arc::new(BreezeEntity::new(
        &vanilla_entities::BREEZE,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&breeze) as SharedEntity)
        .expect("breeze should attach to the loaded chunk");
    breeze
}

fn spawn_golem(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let golem: SharedEntity = Arc::new(IronGolemEntity::new(
        &vanilla_entities::IRON_GOLEM,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&golem))
        .expect("iron golem should attach to the loaded chunk");
    golem
}

#[test]
fn breezes_only_attack_players_and_iron_golems() {
    let fixture = loaded_world("breeze_targets");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(8.5, 1.0, 8.5));
    let golem = spawn_golem(world, DVec3::new(4.5, 1.0, 4.5));
    let pig: SharedEntity = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        DVec3::new(2.5, 1.0, 2.5),
        Arc::downgrade(world),
    ));

    let golem_living = golem.as_living_entity().expect("golem is living");
    let pig_living = pig.as_living_entity().expect("pig is living");
    assert!(Mob::can_attack(breeze.as_ref(), golem_living));
    assert!(!Mob::can_attack(breeze.as_ref(), pig_living));
}

#[test]
fn breezes_ignore_damage_from_other_breezes() {
    let fixture = loaded_world("breeze_friendly_fire");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(8.5, 1.0, 8.5));
    let other: SharedEntity = spawn_breeze(world, DVec3::new(10.5, 1.0, 8.5));
    let golem = spawn_golem(world, DVec3::new(4.5, 1.0, 4.5));

    let from_breeze = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, other);
    let from_golem = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, golem);
    assert!(breeze.is_invulnerable_to(world, &from_breeze));
    assert!(!breeze.is_invulnerable_to(world, &from_golem));
}

#[test]
fn breezes_reverse_projectiles_but_not_wind_charges() {
    let fixture = loaded_world("breeze_deflection");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(8.5, 1.0, 8.5));
    let arrow = ArrowEntity::new(
        &vanilla_entities::ARROW,
        next_entity_id(),
        DVec3::new(4.5, 2.0, 8.5),
        Arc::downgrade(world),
    );
    let wind_charge = WindChargeEntity::new(
        &vanilla_entities::WIND_CHARGE,
        next_entity_id(),
        DVec3::new(4.5, 2.0, 8.5),
        Arc::downgrade(world),
    );
    let breeze_charge = BreezeWindChargeEntity::new(
        &vanilla_entities::BREEZE_WIND_CHARGE,
        next_entity_id(),
        DVec3::new(4.5, 2.0, 8.5),
        Arc::downgrade(world),
    );

    assert_eq!(
        breeze.deflection(&arrow),
        ProjectileDeflection::BreezeReverse
    );
    assert_eq!(breeze.deflection(&wind_charge), ProjectileDeflection::None);
    assert_eq!(
        breeze.deflection(&breeze_charge),
        ProjectileDeflection::None
    );
}

#[test]
fn inner_circle_is_four_blocks_wide_and_ten_high() {
    let fixture = loaded_world("breeze_inner_circle");
    let breeze = spawn_breeze(&fixture.world, DVec3::new(8.2, 1.0, 8.7));
    assert!(breeze.within_inner_circle_range(DVec3::new(11.0, 1.0, 8.5)));
    assert!(!breeze.within_inner_circle_range(DVec3::new(12.6, 1.0, 8.5)));
    assert!(breeze.within_inner_circle_range(DVec3::new(8.5, 10.0, 8.5)));
    assert!(!breeze.within_inner_circle_range(DVec3::new(8.5, 11.6, 8.5)));
}

#[test]
fn shooting_inhales_then_fires_a_breeze_wind_charge() {
    let fixture = loaded_world("breeze_shoot");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(8.5, 1.0, 8.5));
    let golem = spawn_golem(world, DVec3::new(2.5, 1.0, 8.5));
    {
        let mut memories = breeze.memories().lock();
        memories.set(&ATTACK_TARGET, Some(golem));
        memories.set(&BREEZE_SHOOT, Some(()));
    }

    let mut shoot = Shoot;
    assert!(shoot.check_extra_start_conditions(world, &breeze));
    shoot.start(world, &breeze, 0);
    assert_eq!(breeze.pose(), EntityPose::Shooting);
    shoot.tick(world, &breeze, 0);
    let search = WorldAabb::new(0.0, -2.0, 0.0, 16.0, 8.0, 16.0);
    let charges = || {
        world.get_entities_in_aabb_matching(&search, |entity| {
            entity.entity_type() == &vanilla_entities::BREEZE_WIND_CHARGE
        })
    };
    assert!(charges().is_empty(), "the breeze is still inhaling");

    breeze.memories().lock().erase(&BREEZE_SHOOT_CHARGING);
    shoot.tick(world, &breeze, 1);
    let fired = charges();
    assert_eq!(fired.len(), 1);
    let charge = fired[0]
        .downcast_ref::<BreezeWindChargeEntity>()
        .expect("breeze wind charge is concrete");
    assert!((charge.position().y - breeze.firing_y_position()).abs() < 1.0e-9);
    assert!(
        charge.velocity().x < 0.0,
        "the charge flies toward the golem"
    );

    shoot.stop(world, &breeze, 2);
    assert_eq!(breeze.pose(), EntityPose::Standing);
    assert!(!breeze.memories().lock().has_value(&BREEZE_SHOOT));
}

#[test]
fn out_of_range_targets_cancel_the_shot() {
    let fixture = loaded_world("breeze_shoot_range");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(0.5, 1.0, 0.5));
    let golem = spawn_golem(world, DVec3::new(15.5, 1.0, 15.5));
    {
        let mut memories = breeze.memories().lock();
        memories.set(&ATTACK_TARGET, Some(golem));
        memories.set(&BREEZE_SHOOT, Some(()));
    }
    assert!(!Shoot.check_extra_start_conditions(world, &breeze));
    assert!(!breeze.memories().lock().has_value(&BREEZE_SHOOT));
}

#[test]
fn breezes_need_open_air_to_long_jump() {
    let fixture = loaded_world("breeze_jump_headroom");
    let world = &fixture.world;
    let breeze = spawn_breeze(world, DVec3::new(8.5, 1.0, 8.5));
    assert!(LongJump::can_jump_from_current_position(world, &breeze));

    let stone = vanilla_blocks::STONE.default_state();
    world.set_block(BlockPos::new(8, 4, 8), stone, UpdateFlags::UPDATE_ALL);
    assert!(!LongJump::can_jump_from_current_position(world, &breeze));

    world.set_block(
        BlockPos::new(8, 4, 8),
        vanilla_blocks::WATER.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    assert!(LongJump::can_jump_from_current_position(world, &breeze));

    world.set_block(
        BlockPos::new(8, 1, 8),
        vanilla_blocks::HONEY_BLOCK.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    assert!(!LongJump::can_jump_from_current_position(world, &breeze));
}

#[test]
fn jump_target_memory_round_trips() {
    let fixture = loaded_world("breeze_jump_target_save");
    let breeze = spawn_breeze(&fixture.world, DVec3::new(8.5, 1.0, 8.5));
    breeze
        .memories()
        .lock()
        .set(&BREEZE_JUMP_TARGET, Some(BlockPos::new(3, -4, 12)));
    let mut saved = NbtCompound::new();
    breeze.save_additional(&mut saved);

    let loaded = spawn_breeze(&fixture.world, DVec3::new(4.5, 1.0, 4.5));
    let bytes = {
        let mut out = Vec::new();
        saved.write(&mut out);
        out
    };
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("saved breeze nbt should reborrow: {error}"));
    loaded.load_additional((&borrowed).into());
    assert_eq!(
        loaded.memories().lock().get(&BREEZE_JUMP_TARGET),
        Some(BlockPos::new(3, -4, 12))
    );
}
