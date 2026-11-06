use std::convert::Infallible;
use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::TryRng;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_entities};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use steel_registry::vanilla_blocks;
use steel_utils::BlockPos;
use steel_utils::types::{Difficulty, UpdateFlags};

use super::slime::is_slime_chunk;
use super::{AbstractCubeMob, MagmaCubeEntity, SlimeEntity};
use crate::behavior::init_behaviors;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, SharedEntity, SpawnPlacements, init_entities,
    next_entity_id,
};
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

/// Returns the same raw value for every draw, so bounded rolls land on zero.
struct ZeroRng;

impl TryRng for ZeroRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(0)
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(0)
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        bytes.fill(0);
        Ok(())
    }
}

#[test]
fn slime_chunks_match_vanilla_java_random() {
    // Reference values from vanilla `SlimeChunk` on the JVM.
    assert!(is_slime_chunk(1, -3, 0));
    assert!(is_slime_chunk(100, -200, 0));
    assert!(!is_slime_chunk(0, 0, 0));
    assert!(!is_slime_chunk(-7, 12, 0));
    assert!(!is_slime_chunk(-30_000, 29_999, 0));
    assert!(!is_slime_chunk(9, 9, 0));
    assert!(is_slime_chunk(9, 9, -4_172_144_997_902_289_642));
    assert!(is_slime_chunk(0, 7, 12_345));
    assert!(is_slime_chunk(19, 11, 12_345));
    assert!(!is_slime_chunk(1, -3, 12_345));
}

#[test]
fn natural_slimes_stay_off_the_surface_outside_swamps() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("slime_surface_spawn_rule");
    let world = &fixture.world;
    world.set_difficulty(Difficulty::Normal);
    let seed = world.seed();
    let slime_chunk = (0..64)
        .flat_map(|x| (0..64).map(move |z| (x, z)))
        .find(|&(x, z)| is_slime_chunk(x, z, seed))
        .expect("a slime chunk near the origin");
    let plain_chunk = (0..64)
        .flat_map(|x| (0..64).map(move |z| (x, z)))
        .find(|&(x, z)| !is_slime_chunk(x, z, seed))
        .expect("a plain chunk near the origin");

    for (x, z) in [slime_chunk, plain_chunk] {
        insert_ready_full_chunk(world, ChunkPos::new(x, z));
    }
    let place_floor = |pos: BlockPos| {
        assert!(world.set_block(
            pos.below(),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        pos
    };
    let surface = place_floor(BlockPos::new(
        plain_chunk.0 * 16 + 8,
        65,
        plain_chunk.1 * 16 + 8,
    ));
    let underground_plain = place_floor(BlockPos::new(
        plain_chunk.0 * 16 + 4,
        20,
        plain_chunk.1 * 16 + 4,
    ));
    let underground_slime = place_floor(BlockPos::new(
        slime_chunk.0 * 16 + 8,
        20,
        slime_chunk.1 * 16 + 8,
    ));
    let high_slime = place_floor(BlockPos::new(
        slime_chunk.0 * 16 + 4,
        45,
        slime_chunk.1 * 16 + 4,
    ));
    let check = |reason, pos| {
        SpawnPlacements::check_spawn_rules(
            &vanilla_entities::SLIME,
            world,
            reason,
            pos,
            &mut ZeroRng,
        )
    };

    assert!(
        !check(EntitySpawnReason::Natural, surface),
        "no surface slimes outside swamps"
    );
    assert!(
        !check(EntitySpawnReason::Natural, underground_plain),
        "only slime chunks"
    );
    assert!(
        !check(EntitySpawnReason::Natural, high_slime),
        "slime chunks stop at y 40"
    );
    assert!(check(EntitySpawnReason::Natural, underground_slime));
    assert!(
        check(EntitySpawnReason::Spawner, surface),
        "spawners skip the biome rules"
    );

    world.set_difficulty(Difficulty::Peaceful);
    assert!(!check(EntitySpawnReason::Spawner, surface));
}

#[test]
fn natural_zombies_need_darkness() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("zombie_spawn_rule_light");
    let world = &fixture.world;
    world.set_difficulty(Difficulty::Normal);
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let pos = BlockPos::new(8, 65, 8);
    assert!(world.set_block(
        pos.below(),
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    for entity_type in [
        &vanilla_entities::ZOMBIE,
        &vanilla_entities::SPIDER,
        &vanilla_entities::HUSK,
    ] {
        assert!(
            !SpawnPlacements::check_spawn_rules(
                entity_type,
                world,
                EntitySpawnReason::Natural,
                pos,
                &mut ZeroRng,
            ),
            "{} must not spawn under the open daytime sky",
            entity_type.key
        );
    }
}
