use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_registry::{init_vanilla_registry, vanilla_entities};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use super::{BlazeAttackGoal, BlazeEntity};
use crate::entity::ai::goal::Goal;
use crate::entity::entities::{PigEntity, SmallFireballEntity};
use crate::entity::{
    Entity, Mob, Monster, Projectile, SharedEntity, init_entities, next_entity_id,
};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

#[test]
fn a_charged_blaze_counts_as_burning_and_is_always_lit() {
    init_vanilla_registry();
    let blaze = BlazeEntity::new(&vanilla_entities::BLAZE, 1, DVec3::ZERO, Weak::new());
    assert!(!blaze.is_on_fire());
    blaze.set_charged(true);
    assert!(blaze.is_charged());
    assert!(blaze.is_on_fire());
    blaze.set_charged(false);
    assert!(!blaze.is_on_fire());
    assert!((blaze.light_level_dependent_magic_value() - 1.0).abs() < f32::EPSILON);
}

#[test]
fn the_attack_goal_charges_then_fires_three_fireballs() {
    init_vanilla_registry();
    init_entities();
    let world_fixture = fresh_test_world("blaze_volley");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity: SharedEntity = Arc::new(BlazeEntity::new(
        &vanilla_entities::BLAZE,
        next_entity_id(),
        DVec3::new(8.0, 100.0, 2.0),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("blaze should attach to the loaded chunk");
    let target: SharedEntity = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        DVec3::new(8.0, 100.0, 12.0),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&target))
        .expect("pig should attach to the loaded chunk");
    let blaze = entity
        .downcast_ref::<BlazeEntity>()
        .expect("blaze is concrete");
    assert!(blaze.set_target(Some(&target)));

    let mut goal = BlazeAttackGoal::new();
    assert!(goal.can_use(blaze));
    goal.start(blaze);
    goal.tick(blaze, &entity);
    assert!(blaze.is_charged(), "the first step charges up");

    let fireballs = || {
        world
            .get_entities_in_aabb(&WorldAabb::new(0.0, 90.0, 0.0, 16.0, 110.0, 16.0))
            .into_iter()
            .filter(|candidate| candidate.downcast_ref::<SmallFireballEntity>().is_some())
            .count()
    };
    for _ in 0..59 {
        goal.tick(blaze, &entity);
    }
    assert_eq!(fireballs(), 0, "nothing is fired while charging");
    goal.tick(blaze, &entity);
    assert_eq!(fireballs(), 1);
    for _ in 0..12 {
        goal.tick(blaze, &entity);
    }
    assert_eq!(fireballs(), 3);
    assert!(blaze.is_charged());
    for _ in 0..6 {
        goal.tick(blaze, &entity);
    }
    assert_eq!(fireballs(), 3, "the volley is three fireballs");
    assert!(!blaze.is_charged(), "the blaze cools down after the volley");

    let shot = world
        .get_entities_in_aabb(&WorldAabb::new(0.0, 90.0, 0.0, 16.0, 110.0, 16.0))
        .into_iter()
        .find(|candidate| candidate.downcast_ref::<SmallFireballEntity>().is_some())
        .expect("a fireball was fired");
    assert!(shot.velocity().z > 0.0, "fireballs fly toward the target");
    let owner = shot
        .downcast_ref::<SmallFireballEntity>()
        .and_then(Projectile::get_owner)
        .expect("fireballs belong to the blaze");
    assert_eq!(owner.id(), entity.id());
}
