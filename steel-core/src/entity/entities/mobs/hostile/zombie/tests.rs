use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities,
    vanilla_mob_effects,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{GiantEntity, HuskEntity, ZombieEntity, ZombieGroupData};
use crate::behavior::init_behaviors;
use crate::entity::entities::CowEntity;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, Mob, SharedEntity, SpawnGroupData, init_entities,
    next_entity_id,
};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> SharedEntity {
    let entity: SharedEntity = Arc::new(entity);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test entity should attach to the loaded chunk");
    entity
}

fn reborrow(nbt: &NbtCompound) -> Vec<u8> {
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    bytes
}

#[test]
fn baby_zombie_is_small_and_fast() {
    init_vanilla_registry();
    let zombie = ZombieEntity::new(&vanilla_entities::ZOMBIE, 1, DVec3::ZERO, Weak::new());
    let adult_speed = zombie
        .attributes()
        .lock()
        .get_value(vanilla_attributes::MOVEMENT_SPEED)
        .expect("zombies have a movement speed");

    zombie.set_baby(true);

    assert!(zombie.is_baby());
    let dimensions = zombie.base().dimensions();
    assert_eq!(dimensions.width.to_bits(), 0.49_f32.to_bits());
    assert_eq!(dimensions.height.to_bits(), 0.98_f32.to_bits());
    assert_eq!(dimensions.eye_height.to_bits(), 0.775_f32.to_bits());
    let baby_speed = zombie
        .attributes()
        .lock()
        .get_value(vanilla_attributes::MOVEMENT_SPEED)
        .expect("zombies have a movement speed");
    assert!((baby_speed - adult_speed * 1.5).abs() < 1e-9);

    zombie.set_baby(false);
    assert!(!zombie.is_baby());
    assert_eq!(
        zombie.base().dimensions(),
        vanilla_entities::ZOMBIE.dimensions
    );
}

#[test]
fn husk_baby_has_its_own_eye_height() {
    init_vanilla_registry();
    let husk = HuskEntity::new(&vanilla_entities::HUSK, 1, DVec3::ZERO, Weak::new());
    husk.set_baby(true);
    assert_eq!(
        husk.base().dimensions().eye_height.to_bits(),
        0.825_f32.to_bits()
    );
}

#[test]
fn breaking_doors_toggles_the_goal_and_door_paths() {
    init_vanilla_registry();
    let zombie = ZombieEntity::new(&vanilla_entities::ZOMBIE, 1, DVec3::ZERO, Weak::new());
    let goals = |zombie: &ZombieEntity| {
        zombie
            .mob_base()
            .goal_selector()
            .lock()
            .available_goal_priorities()
    };
    let base_goals = goals(&zombie);
    assert!(!base_goals.contains(&1));

    zombie.set_can_break_doors(true);
    assert!(zombie.can_break_doors());
    assert!(zombie.mob_base().navigation().lock().can_open_doors());
    assert!(goals(&zombie).contains(&1));

    zombie.set_can_break_doors(false);
    assert!(!zombie.mob_base().navigation().lock().can_open_doors());
    assert_eq!(goals(&zombie), base_goals);
}

#[test]
fn zombie_saves_and_loads_vanilla_state() {
    init_vanilla_registry();
    let zombie = ZombieEntity::new(&vanilla_entities::ZOMBIE, 1, DVec3::ZERO, Weak::new());
    zombie.set_baby(true);
    zombie.set_can_break_doors(true);

    let mut nbt = NbtCompound::new();
    zombie.save_additional(&mut nbt);
    assert_eq!(nbt.byte("IsBaby"), Some(1));
    assert_eq!(nbt.byte("CanBreakDoors"), Some(1));
    assert_eq!(nbt.int("DrownedConversionTime"), Some(-1));
    assert_eq!(nbt.int("InWaterTime"), Some(-1));

    let mut saved = NbtCompound::new();
    saved.insert("IsBaby", 1_i8);
    saved.insert("CanBreakDoors", 1_i8);
    saved.insert("InWaterTime", 42_i32);
    saved.insert("DrownedConversionTime", 120_i32);
    let bytes = reborrow(&saved);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    let loaded = ZombieEntity::new(&vanilla_entities::ZOMBIE, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());

    assert!(loaded.is_baby());
    assert!(loaded.can_break_doors());
    assert!(loaded.is_under_water_converting());
}

#[test]
fn finalize_spawn_uses_the_zombie_group() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("zombie_group");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let zombie = add(
        world,
        ZombieEntity::new(
            &vanilla_entities::ZOMBIE,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let zombie_ref = zombie
        .downcast_ref::<ZombieEntity>()
        .expect("zombie is concrete");

    let group = Some(SpawnGroupData::Zombie(ZombieGroupData {
        is_baby: true,
        can_spawn_jockey: false,
    }));
    let returned = zombie_ref.finalize_spawn(world, EntitySpawnReason::Command, group);

    assert_eq!(returned, group);
    assert!(zombie_ref.is_baby());
}

#[test]
fn finalize_spawn_without_group_creates_one() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("zombie_new_group");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let zombie = add(
        world,
        ZombieEntity::new(
            &vanilla_entities::ZOMBIE,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let zombie_ref = zombie
        .downcast_ref::<ZombieEntity>()
        .expect("zombie is concrete");

    let returned = zombie_ref.finalize_spawn(world, EntitySpawnReason::Command, None);

    assert!(matches!(
        returned,
        Some(SpawnGroupData::Zombie(ZombieGroupData {
            can_spawn_jockey: true,
            ..
        }))
    ));
}

#[test]
fn husk_converts_to_a_zombie_after_drowning() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("husk_drowning");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let water = vanilla_blocks::WATER.default_state();
    for y in 65..68 {
        world.set_block(BlockPos::new(8, y, 8), water, UpdateFlags::UPDATE_ALL);
    }
    let husk = add(
        world,
        HuskEntity::new(
            &vanilla_entities::HUSK,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let husk_ref = husk.downcast_ref::<HuskEntity>().expect("husk is concrete");
    husk_ref.set_baby(true);

    husk_ref.set_in_water_time(599);
    Arc::clone(&husk).tick();
    assert!(
        husk_ref.is_under_water_converting(),
        "600 ticks underwater start the conversion"
    );

    husk_ref.set_conversion_time(0);
    Arc::clone(&husk).tick();

    assert!(husk.is_removed(), "the husk is replaced");
    let zombie = world
        .get_entities_in_aabb_matching(&husk.bounding_box().inflate(1.0), |entity| {
            entity.entity_type() == &vanilla_entities::ZOMBIE
        })
        .into_iter()
        .next()
        .expect("the husk becomes a zombie");
    let zombie_ref = zombie
        .downcast_ref::<ZombieEntity>()
        .expect("converted zombie is concrete");
    assert!(zombie_ref.is_baby(), "babies stay babies");
}

fn husk_bite_hunger_ticks(difficulty: Difficulty, name: &'static str) -> Option<i32> {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    world.set_difficulty(difficulty);
    let husk = add(
        world,
        HuskEntity::new(
            &vanilla_entities::HUSK,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let cow = add(
        world,
        CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(9.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let husk_ref = husk.downcast_ref::<HuskEntity>().expect("husk is concrete");

    assert!(husk_ref.do_hurt_target(&husk, world, &cow));
    cow.as_living_entity()
        .expect("cow is living")
        .mob_effect(vanilla_mob_effects::HUNGER)
        .map(|effect| effect.duration())
}

#[test]
fn husk_bite_starves_by_regional_difficulty() {
    // A fresh world and chunk have the base regional difficulty:
    // 1.5 on Normal and 2.25 on Hard, truncated to 1 and 2.
    assert_eq!(
        husk_bite_hunger_ticks(Difficulty::Normal, "husk_bite_normal"),
        Some(140)
    );
    assert_eq!(
        husk_bite_hunger_ticks(Difficulty::Hard, "husk_bite_hard"),
        Some(280)
    );
}

#[test]
fn giant_has_no_goals() {
    init_vanilla_registry();
    let giant = GiantEntity::new(&vanilla_entities::GIANT, 1, DVec3::ZERO, Weak::new());
    assert_eq!(
        giant
            .mob_base()
            .goal_selector()
            .lock()
            .available_goal_priorities(),
        Vec::<i32>::new()
    );
    assert_eq!(giant.get_max_health(), 100.0);
}
