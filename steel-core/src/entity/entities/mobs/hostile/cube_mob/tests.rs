use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_entities};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use super::{AbstractCubeMob, MagmaCubeEntity, SlimeEntity};
use crate::entity::{Entity, LivingEntity, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

#[test]
fn size_scales_hitbox_health_damage_and_speed() {
    init_vanilla_registry();
    let slime = SlimeEntity::new(&vanilla_entities::SLIME, 1, DVec3::ZERO, Weak::new());
    slime.set_size(4, true);
    assert_eq!(slime.get_size(), 4);
    assert!((slime.get_max_health() - 16.0).abs() < f32::EPSILON);
    assert!((slime.get_health() - 16.0).abs() < f32::EPSILON);
    let width = vanilla_entities::SLIME.dimensions.width * 4.0;
    assert!(
        (slime.bounding_box().max_x() - slime.bounding_box().min_x() - f64::from(width)).abs()
            < 1.0e-6
    );
    let attributes = slime.attributes().lock();
    assert!((attributes.required_value(vanilla_attributes::ATTACK_DAMAGE) - 4.0).abs() < 1.0e-9);
    assert!(
        (attributes.required_value(vanilla_attributes::MOVEMENT_SPEED)
            - f64::from(0.2_f32 + 0.4_f32))
        .abs()
            < 1.0e-9
    );
    drop(attributes);

    slime.set_size(500, false);
    assert_eq!(slime.get_size(), 127, "sizes clamp to 127");
}

#[test]
fn magma_cube_armor_uses_the_unclamped_size() {
    init_vanilla_registry();
    let cube = MagmaCubeEntity::new(&vanilla_entities::MAGMA_CUBE, 1, DVec3::ZERO, Weak::new());
    cube.set_size(0, true);
    assert_eq!(cube.get_size(), 1);
    assert!(
        cube.attributes()
            .lock()
            .required_value(vanilla_attributes::ARMOR)
            .abs()
            < 1.0e-9
    );
    assert!(cube.can_deal_damage(), "even a tiny magma cube hurts");
    assert!(!cube.is_on_fire());
}

#[test]
fn size_and_landing_state_round_trip_through_nbt() {
    init_vanilla_registry();
    let slime = SlimeEntity::new(&vanilla_entities::SLIME, 1, DVec3::ZERO, Weak::new());
    slime.set_size(2, true);
    let mut nbt = NbtCompound::new();
    slime.save_additional(&mut nbt);
    assert_eq!(nbt.int("Size"), Some(1));
    assert!(
        nbt.int("Age").is_none(),
        "slimes have no baby form, so no age"
    );

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = SlimeEntity::new(&vanilla_entities::SLIME, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.get_size(), 2);
}

#[test]
fn a_dead_slime_splits_into_half_size_slimes() {
    init_vanilla_registry();
    init_entities();
    let world_fixture = fresh_test_world("slime_split");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity: SharedEntity = Arc::new(SlimeEntity::new(
        &vanilla_entities::SLIME,
        next_entity_id(),
        DVec3::new(8.0, 65.0, 8.0),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test slime should attach to the loaded chunk");
    let slime = entity
        .downcast_ref::<SlimeEntity>()
        .expect("slime is concrete");
    slime.set_size(4, true);
    slime.set_health(0.0);
    slime.tick_death_until_removed();

    let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 70.0, 16.0);
    let children: Vec<_> = world
        .get_entities_in_aabb(&area)
        .into_iter()
        .filter(|child| child.id() != entity.id() && !child.is_removed())
        .collect();
    assert!(
        (2..=4).contains(&children.len()),
        "split into {} slimes",
        children.len()
    );
    for child in children {
        let child = child
            .downcast_ref::<SlimeEntity>()
            .expect("split keeps the slime type");
        assert_eq!(child.get_size(), 2);
        assert!((child.get_health() - 4.0).abs() < f32::EPSILON);
    }
}

#[test]
fn a_tiny_slime_does_not_split() {
    init_vanilla_registry();
    let world_fixture = fresh_test_world("slime_tiny");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity: SharedEntity = Arc::new(SlimeEntity::new(
        &vanilla_entities::SLIME,
        next_entity_id(),
        DVec3::new(8.0, 65.0, 8.0),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test slime should attach to the loaded chunk");
    let slime = entity
        .downcast_ref::<SlimeEntity>()
        .expect("slime is concrete");
    slime.set_health(0.0);
    slime.tick_death_until_removed();

    let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 70.0, 16.0);
    assert!(
        world
            .get_entities_in_aabb(&area)
            .iter()
            .all(|child| child.id() == entity.id())
    );
}

trait TickDeath {
    fn tick_death_until_removed(&self);
}

impl<T: LivingEntity> TickDeath for T {
    fn tick_death_until_removed(&self) {
        for _ in 0..40 {
            if self.is_removed() {
                return;
            }
            self.tick_death();
        }
        panic!("death animation never removed the entity");
    }
}
