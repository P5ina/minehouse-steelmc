use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_damage_types,
    vanilla_entities, vanilla_mob_effects,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::sensor::HoglinSpecificSensor;
use super::{HoglinEntity, ZoglinEntity, throw_target};
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::Sensor as _;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, AVOID_TARGET, NEAREST_REPELLENT, NEAREST_VISIBLE_ADULT_HOGLINS,
};
use crate::entity::ai::brain::sensors::NearestLivingEntitySensor;
use crate::entity::damage::DamageSource;
use crate::entity::entities::{CowEntity, CreeperEntity};
use crate::entity::{
    Entity, LivingEntity as _, Mob, PathfinderMob as _, SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn detached_zoglin() -> ZoglinEntity {
    ZoglinEntity::new(&vanilla_entities::ZOGLIN, 1, DVec3::ZERO, Weak::new())
}

fn loaded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world(name);
    insert_ready_full_chunk(&world_fixture.world, ChunkPos::new(0, 0));
    world_fixture
}

fn add(world: &Arc<World>, entity: SharedEntity) -> SharedEntity {
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("entity should attach to the loaded chunk");
    entity
}

#[test]
fn a_baby_is_smaller_and_hits_softer() {
    init_vanilla_registry();
    let zoglin = detached_zoglin();
    let adult_height = zoglin.dimensions_for_pose(zoglin.pose()).height;
    zoglin.set_baby(true);
    assert!(zoglin.is_baby());
    assert!(zoglin.dimensions_for_pose(zoglin.pose()).height < adult_height);
    let damage = zoglin
        .attributes()
        .lock()
        .required_value(vanilla_attributes::ATTACK_DAMAGE);
    assert!((damage - 0.5).abs() < 1.0e-9);
}

#[test]
fn baby_flag_round_trips() {
    init_vanilla_registry();
    let zoglin = detached_zoglin();
    zoglin.set_baby(true);
    let mut nbt = NbtCompound::new();
    zoglin.save_additional(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = detached_zoglin();
    loaded.load_additional((&borrowed).into());
    assert!(loaded.is_baby());
}

#[test]
fn throws_its_target_up_and_away() {
    let world_fixture = loaded_world("zoglin_throw");
    let world = &world_fixture.world;
    let zoglin = add(
        world,
        Arc::new(ZoglinEntity::new(
            &vanilla_entities::ZOGLIN,
            next_entity_id(),
            DVec3::new(4.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let cow = add(
        world,
        Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(6.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let (Some(body), Some(target)) = (zoglin.as_living_entity(), cow.as_living_entity()) else {
        panic!("both are living");
    };
    throw_target(body, target);
    let velocity = cow.velocity();
    assert!(velocity.y > 0.0, "tossed up: {velocity}");
    assert!(velocity.length() > 0.0);
}

#[test]
fn targets_the_nearest_mob_but_not_creepers() {
    let world_fixture = loaded_world("zoglin_targets");
    let world = &world_fixture.world;
    let zoglin = add(
        world,
        Arc::new(ZoglinEntity::new(
            &vanilla_entities::ZOGLIN,
            next_entity_id(),
            DVec3::new(4.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let _creeper = add(
        world,
        Arc::new(CreeperEntity::new(
            &vanilla_entities::CREEPER,
            next_entity_id(),
            DVec3::new(5.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let cow = add(
        world,
        Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(8.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let body = zoglin
        .downcast_ref::<ZoglinEntity>()
        .expect("zoglin is concrete");
    NearestLivingEntitySensor.do_tick(world, body);
    let found = body.find_nearest_valid_attack_target(world);
    assert_eq!(found.map(|target| target.id()), Some(cow.id()));
}

#[test]
fn getting_hurt_makes_it_angry_at_the_attacker() {
    let world_fixture = loaded_world("zoglin_hurt");
    let world = &world_fixture.world;
    let zoglin = add(
        world,
        Arc::new(ZoglinEntity::new(
            &vanilla_entities::ZOGLIN,
            next_entity_id(),
            DVec3::new(4.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let cow = add(
        world,
        Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(6.0, 64.0, 4.0),
            Arc::downgrade(world),
        )),
    );
    let body = zoglin
        .downcast_ref::<ZoglinEntity>()
        .expect("zoglin is concrete");
    let source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
        .with_causing_entity(Arc::clone(&cow))
        .with_direct_entity(Arc::clone(&cow));
    assert!(body.hurt_server(world, &source, 1.0));
    let target = body.memories().lock().get(&ATTACK_TARGET);
    assert_eq!(target.map(|target| target.id()), Some(cow.id()));
    assert_eq!(body.target().map(|target| target.id()), Some(cow.id()));
}

fn add_hoglin(world: &Arc<World>, position: DVec3) -> SharedEntity {
    add(
        world,
        Arc::new(HoglinEntity::new(
            &vanilla_entities::HOGLIN,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        )),
    )
}

fn as_hoglin(entity: &SharedEntity) -> &HoglinEntity {
    entity
        .downcast_ref::<HoglinEntity>()
        .expect("hoglin is concrete")
}

#[test]
fn a_baby_hoglin_hits_softer_until_it_grows_up() {
    init_vanilla_registry();
    let hoglin = HoglinEntity::new(&vanilla_entities::HOGLIN, 1, DVec3::ZERO, Weak::new());
    let damage = || {
        hoglin
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE)
    };
    assert!((damage() - 6.0).abs() < 1.0e-9);
    hoglin.set_baby(true);
    assert!(!hoglin.is_adult());
    assert!((damage() - 0.5).abs() < 1.0e-9);
    assert_eq!(hoglin.xp_reward(), 3);
    hoglin.set_baby(false);
    assert!((damage() - 6.0).abs() < 1.0e-9);
    assert_eq!(hoglin.xp_reward(), 5);
}

#[test]
fn hoglin_state_round_trips() {
    init_vanilla_registry();
    let hoglin = HoglinEntity::new(&vanilla_entities::HOGLIN, 1, DVec3::ZERO, Weak::new());
    hoglin.set_immune_to_zombification(true);
    hoglin.set_time_in_overworld(42);
    let mut nbt = NbtCompound::new();
    hoglin.save_additional(&mut nbt);
    assert_eq!(nbt.int("TimeInOverworld"), Some(42));
    assert_eq!(nbt.byte("CannotBeHunted"), Some(0));
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = HoglinEntity::new(&vanilla_entities::HOGLIN, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert!(!loaded.is_converting());
    assert!(loaded.can_be_hunted());
}

#[test]
fn senses_a_repellent_and_avoids_walking_near_it() {
    let world_fixture = loaded_world("hoglin_repellent");
    let world = &world_fixture.world;
    let anchor = BlockPos::new(8, 64, 4);
    world.set_block(
        anchor,
        vanilla_blocks::RESPAWN_ANCHOR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let entity = add_hoglin(world, DVec3::new(4.5, 64.0, 4.5));
    let hoglin = as_hoglin(&entity);
    HoglinSpecificSensor.do_tick(world, hoglin);
    assert_eq!(
        hoglin.memories().lock().get(&NEAREST_REPELLENT),
        Some(anchor)
    );
    let near = hoglin.get_walk_target_value_in(BlockPos::new(6, 64, 4), world.as_ref());
    assert!((near + 1.0).abs() < f32::EPSILON);
    let far = hoglin.get_walk_target_value_in(BlockPos::new(-6, 64, 4), world.as_ref());
    assert!(far.abs() < f32::EPSILON);
}

#[test]
fn zombifies_into_a_nauseous_zoglin_outside_the_nether() {
    let world_fixture = loaded_world("hoglin_zombify");
    let world = &world_fixture.world;
    let entity = add_hoglin(world, DVec3::new(4.5, 64.0, 4.5));
    let hoglin = as_hoglin(&entity);
    assert!(hoglin.is_converting());
    hoglin.set_time_in_overworld(300);
    hoglin.custom_server_ai_step();
    assert!(entity.is_removed());
    let zoglin = world
        .get_entities_in_aabb(&entity.bounding_box().inflate(1.0))
        .into_iter()
        .find(|candidate| candidate.downcast_ref::<ZoglinEntity>().is_some())
        .expect("the hoglin became a zoglin");
    let zoglin = zoglin
        .as_living_entity()
        .expect("zoglins are living entities");
    assert!(zoglin.has_mob_effect(vanilla_mob_effects::NAUSEA));
}

#[test]
fn an_immune_hoglin_stays_a_hoglin() {
    let world_fixture = loaded_world("hoglin_immune");
    let world = &world_fixture.world;
    let entity = add_hoglin(world, DVec3::new(4.5, 64.0, 4.5));
    let hoglin = as_hoglin(&entity);
    hoglin.set_immune_to_zombification(true);
    hoglin.set_time_in_overworld(300);
    hoglin.custom_server_ai_step();
    assert!(!entity.is_removed());
}

#[test]
fn a_hurt_adult_rallies_the_herd_against_the_attacker() {
    let world_fixture = loaded_world("hoglin_rally");
    let world = &world_fixture.world;
    let entity = add_hoglin(world, DVec3::new(4.5, 64.0, 4.5));
    let friend = add_hoglin(world, DVec3::new(6.5, 64.0, 4.5));
    let cow = add(
        world,
        Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(4.5, 64.0, 7.5),
            Arc::downgrade(world),
        )),
    );
    let hoglin = as_hoglin(&entity);
    hoglin.memories().lock().set(
        &NEAREST_VISIBLE_ADULT_HOGLINS,
        Some(vec![Arc::clone(&friend)]),
    );
    let source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
        .with_causing_entity(Arc::clone(&cow))
        .with_direct_entity(Arc::clone(&cow));
    assert!(hoglin.hurt_server(world, &source, 1.0));
    let target_of = |hoglin: &HoglinEntity| {
        hoglin
            .memories()
            .lock()
            .get(&ATTACK_TARGET)
            .map(|target| target.id())
    };
    assert_eq!(target_of(hoglin), Some(cow.id()));
    assert_eq!(target_of(as_hoglin(&friend)), Some(cow.id()));
}

#[test]
fn a_hurt_baby_runs_away() {
    let world_fixture = loaded_world("hoglin_baby_flee");
    let world = &world_fixture.world;
    let entity = add_hoglin(world, DVec3::new(4.5, 64.0, 4.5));
    let cow = add(
        world,
        Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(4.5, 64.0, 7.5),
            Arc::downgrade(world),
        )),
    );
    let hoglin = as_hoglin(&entity);
    hoglin.set_baby(true);
    let source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
        .with_causing_entity(Arc::clone(&cow))
        .with_direct_entity(Arc::clone(&cow));
    assert!(hoglin.hurt_server(world, &source, 1.0));
    let memories = hoglin.memories().lock();
    assert!(!memories.has_value(&ATTACK_TARGET));
    assert_eq!(
        memories.get(&AVOID_TARGET).map(|target| target.id()),
        Some(cow.id())
    );
}
