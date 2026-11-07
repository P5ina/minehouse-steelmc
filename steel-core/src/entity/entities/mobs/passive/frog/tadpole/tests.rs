use std::io::Cursor;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use steel_registry::{init_vanilla_registry, vanilla_items};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use super::*;
use crate::behavior::init_behaviors;
use crate::entity::{init_entities, next_entity_id};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

fn tadpole(id: i32) -> TadpoleEntity {
    TadpoleEntity::new(&vanilla_entities::TADPOLE, id, DVec3::ZERO, Weak::new())
}

#[test]
fn tadpoles_never_despawn_and_eat_slime_balls() {
    init_vanilla_registry();
    let body = tadpole(1);
    assert!(body.is_from_bucket());
    assert!(body.requires_custom_persistence());
    assert!(!body.remove_when_far_away(f64::MAX));
    assert!(!body.should_drop_experience());
    assert!((body.get_max_health() - 6.0).abs() < f32::EPSILON);
    assert!(TadpoleEntity::is_food(&ItemStack::new(
        &vanilla_items::SLIME_BALL
    )));
    assert!(!TadpoleEntity::is_food(&ItemStack::new(
        &vanilla_items::WHEAT
    )));
}

#[test]
fn feeding_speeds_growth_by_a_tenth_of_what_is_left() {
    init_vanilla_registry();
    let body = tadpole(1);
    body.age_up_by_seconds(AgeableMobBase::get_speed_up_seconds_when_feeding(
        body.ticks_left_until_adult(),
    ));
    // 24000 ticks left is 1200 s, a tenth of that is 120 s or 2400 ticks.
    assert_eq!(body.age(), 2400);
}

#[test]
fn age_and_lock_survive_save_and_load() {
    init_vanilla_registry();
    let saved = tadpole(1);
    saved.set_age(1234);
    saved.set_age_locked(true);
    let mut nbt = NbtCompound::new();
    saved.save_additional(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));

    let loaded = tadpole(2);
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.age(), 1234);
    assert!(loaded.is_age_locked());
}

#[test]
fn a_grown_tadpole_becomes_a_persistent_frog() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("tadpole_grow_up");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity: SharedEntity = Arc::new(TadpoleEntity::new(
        &vanilla_entities::TADPOLE,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .unwrap_or_else(|error| panic!("test tadpole should attach: {error}"));
    let body = entity
        .downcast_ref::<TadpoleEntity>()
        .unwrap_or_else(|| panic!("tadpole is concrete"));

    body.set_age(TICKS_TO_BE_FROG - 1);
    assert!(!entity.is_removed());
    body.set_age(TICKS_TO_BE_FROG);
    assert!(entity.is_removed());

    let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 70.0, 16.0);
    let frogs: Vec<_> = world
        .get_entities_in_aabb(&area)
        .into_iter()
        .filter(|other| other.entity_type() == &vanilla_entities::FROG && !other.is_removed())
        .collect();
    assert_eq!(frogs.len(), 1);
    assert!(frogs[0].as_mob().is_some_and(Mob::is_persistence_required));
}
