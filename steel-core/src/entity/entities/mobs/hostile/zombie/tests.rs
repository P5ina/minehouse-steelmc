use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_damage_types,
    vanilla_entities, vanilla_items, vanilla_mob_effects, vanilla_villager_professions,
    vanilla_villager_types,
};
use steel_utils::geometry::WorldAabb;
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{
    DrownedEntity, GiantEntity, HuskEntity, ZombieEntity, ZombieGroupData, ZombieVillagerEntity,
};
use crate::behavior::init_behaviors;
use crate::entity::RangedAttackMob as _;
use crate::entity::ai::navigation::NavigationKind;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entities::mobs::villagers::villager_data::villager_type_at;
use crate::entity::entities::{CowEntity, VillagerEntity};
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, Mob, SharedEntity, SpawnGroupData, init_entities,
    next_entity_id,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
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

fn flooded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    let water = vanilla_blocks::WATER.default_state();
    for y in 64..68 {
        fixture
            .world
            .set_block(BlockPos::new(8, y, 8), water, UpdateFlags::UPDATE_ALL);
    }
    fixture
}

#[test]
fn drowned_swims_with_amphibious_navigation() {
    init_vanilla_registry();
    let drowned = DrownedEntity::new(&vanilla_entities::DROWNED, 1, DVec3::ZERO, Weak::new());
    assert_eq!(
        drowned.mob_base().navigation().lock().kind(),
        NavigationKind::Amphibious
    );
    assert!(
        drowned
            .mob_base()
            .pathfinding_malus()
            .lock()
            .get(PathType::Water)
            .abs()
            < f32::EPSILON,
        "vanilla Drowned sets the water malus to 0"
    );
    assert!(
        (drowned.max_up_step() - 1.0).abs() < f32::EPSILON,
        "drowned step one block"
    );
}

#[test]
fn zombie_converts_into_a_drowned() {
    let fixture = flooded_world("zombie_to_drowned");
    let world = &fixture.world;
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
    zombie_ref.set_in_water_time(599);
    Arc::clone(&zombie).tick();
    zombie_ref.set_conversion_time(0);
    Arc::clone(&zombie).tick();

    assert!(zombie.is_removed(), "the zombie is replaced");
    let drowned = world
        .get_entities_in_aabb_matching(&zombie.bounding_box().inflate(1.0), |entity| {
            entity.entity_type() == &vanilla_entities::DROWNED
        })
        .into_iter()
        .next()
        .expect("the zombie becomes a drowned");
    assert!(drowned.downcast_ref::<DrownedEntity>().is_some());
}

#[test]
fn drowned_never_converts_underwater() {
    let fixture = flooded_world("drowned_no_conversion");
    let world = &fixture.world;
    let drowned = add(
        world,
        DrownedEntity::new(
            &vanilla_entities::DROWNED,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let drowned_ref = drowned
        .downcast_ref::<DrownedEntity>()
        .expect("drowned is concrete");
    drowned_ref.set_in_water_time(599);
    Arc::clone(&drowned).tick();
    assert!(!drowned_ref.is_under_water_converting());
    assert!(!drowned.is_removed());
}

#[test]
fn drowned_throws_a_trident_at_its_target() {
    let fixture = flooded_world("drowned_trident");
    let world = &fixture.world;
    let drowned = add(
        world,
        DrownedEntity::new(
            &vanilla_entities::DROWNED,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let target = add(
        world,
        CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 14.5),
            Arc::downgrade(world),
        ),
    );
    let drowned_ref = drowned
        .downcast_ref::<DrownedEntity>()
        .expect("drowned is concrete");
    drowned_ref.perform_ranged_attack(&drowned, &target, 1.0);

    let trident = world
        .get_entities_in_aabb_matching(&drowned.bounding_box().inflate(3.0), |entity| {
            entity.entity_type() == &vanilla_entities::TRIDENT
        })
        .into_iter()
        .next()
        .expect("the drowned throws a trident");
    assert!(
        trident.velocity().z > 0.0,
        "the trident flies at the target"
    );
}

#[test]
fn drowned_spawn_needs_water_below() {
    let fixture = flooded_world("drowned_spawn_rule");
    let world = &fixture.world;
    let mut random = rand::rng();
    assert!(
        !(DrownedEntity::SPAWN_RULE)(
            &vanilla_entities::DROWNED,
            world.as_ref(),
            EntitySpawnReason::Natural,
            BlockPos::new(2, 66, 2),
            &mut random,
        ),
        "natural spawns need water below"
    );
}

#[test]
fn drowned_ranged_uncertainty_override() {
    init_vanilla_registry();
    let fixture = fresh_test_world("drowned_uncertainty");
    let drowned = DrownedEntity::new(&vanilla_entities::DROWNED, 1, DVec3::ZERO, Weak::new());
    let default = drowned.ranged_attack_uncertainty(&fixture.world);
    drowned.set_ranged_attack_uncertainty(0.0);
    assert!(drowned.ranged_attack_uncertainty(&fixture.world).abs() < f32::EPSILON);
    drowned.set_ranged_attack_uncertainty(-1.0);
    assert!((drowned.ranged_attack_uncertainty(&fixture.world) - default).abs() < f32::EPSILON);
}

#[test]
fn zombie_villager_saves_and_loads_its_villager() {
    init_vanilla_registry();
    let zombie_villager = ZombieVillagerEntity::new(
        &vanilla_entities::ZOMBIE_VILLAGER,
        1,
        DVec3::ZERO,
        Weak::new(),
    );
    zombie_villager.set_villager_data(
        &vanilla_villager_types::SNOW,
        &vanilla_villager_professions::LIBRARIAN,
        3,
    );
    zombie_villager.set_villager_xp(42);

    let mut nbt = NbtCompound::new();
    zombie_villager.save_additional(&mut nbt);
    assert_eq!(nbt.int("ConversionTime"), Some(-1), "not converting");
    assert_eq!(nbt.int("Xp"), Some(42));
    let villager_data = nbt
        .compound("VillagerData")
        .expect("villager data is saved");
    assert_eq!(
        villager_data
            .string("type")
            .map(|s| s.to_string_lossy())
            .as_deref(),
        Some("minecraft:snow")
    );
    assert_eq!(
        villager_data
            .string("profession")
            .map(|s| s.to_string_lossy())
            .as_deref(),
        Some("minecraft:librarian")
    );
    assert_eq!(villager_data.int("level"), Some(3));

    let bytes = reborrow(&nbt);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    let loaded = ZombieVillagerEntity::new(
        &vanilla_entities::ZOMBIE_VILLAGER,
        2,
        DVec3::ZERO,
        Weak::new(),
    );
    loaded.load_additional((&borrowed).into());

    assert_eq!(loaded.villager_type().key, vanilla_villager_types::SNOW.key);
    assert_eq!(
        loaded.profession().key,
        vanilla_villager_professions::LIBRARIAN.key
    );
    assert_eq!(loaded.villager_level(), 3);
    assert_eq!(loaded.villager_xp(), 42);
    assert!(
        loaded.villager_data_finalized(),
        "loaded villager data is final"
    );
    assert!(!loaded.is_converting());
}

#[test]
fn zombie_villager_loads_a_conversion_in_progress() {
    init_vanilla_registry();
    let mut saved = NbtCompound::new();
    saved.insert("ConversionTime", 300_i32);
    let bytes = reborrow(&saved);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    let loaded = ZombieVillagerEntity::new(
        &vanilla_entities::ZOMBIE_VILLAGER,
        1,
        DVec3::ZERO,
        Weak::new(),
    );
    loaded.load_additional((&borrowed).into());

    assert!(loaded.is_converting());
    assert_eq!(loaded.villager_conversion_time(), 300);
    assert!(
        !loaded.remove_when_far_away(0.0),
        "a converting zombie villager stays"
    );
}

#[test]
fn zombie_villager_despawns_only_without_experience() {
    init_vanilla_registry();
    let zombie_villager = ZombieVillagerEntity::new(
        &vanilla_entities::ZOMBIE_VILLAGER,
        1,
        DVec3::ZERO,
        Weak::new(),
    );
    assert!(zombie_villager.remove_when_far_away(0.0));
    zombie_villager.set_villager_xp(1);
    assert!(
        !zombie_villager.remove_when_far_away(0.0),
        "a traded villager stays"
    );
}

#[test]
fn zombie_villager_finalizes_its_type_once() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("zombie_villager_type");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let zombie_villager = add(
        world,
        ZombieVillagerEntity::new(
            &vanilla_entities::ZOMBIE_VILLAGER,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let zombie_villager = zombie_villager
        .downcast_ref::<ZombieVillagerEntity>()
        .expect("zombie villager is concrete");
    assert!(!zombie_villager.villager_data_finalized());

    zombie_villager.finalize_spawn(world, EntitySpawnReason::Command, None);

    assert!(zombie_villager.villager_data_finalized());
    let biome_type = villager_type_at(world, zombie_villager.block_position());
    assert_eq!(zombie_villager.villager_type().key, biome_type.key);
}

/// The box around where [`zombie_kills_villager`] places its villager.
fn around_villager() -> WorldAabb {
    WorldAabb::from_min_max(DVec3::new(6.0, 62.0, 6.0), DVec3::new(12.0, 69.0, 12.0))
}

/// A zombie and a villager it has just killed on `difficulty`.
fn zombie_kills_villager(difficulty: Difficulty, name: &'static str) -> (TestWorld, bool) {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    world.set_difficulty(difficulty);
    let zombie = add(
        world,
        ZombieEntity::new(
            &vanilla_entities::ZOMBIE,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let villager = add(
        world,
        VillagerEntity::new(
            &vanilla_entities::VILLAGER,
            next_entity_id(),
            DVec3::new(9.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let villager_ref = villager
        .downcast_ref::<VillagerEntity>()
        .expect("villager is concrete");
    villager_ref.set_villager_data(
        &vanilla_villager_types::DESERT,
        &vanilla_villager_professions::FARMER,
        2,
    );
    villager_ref.set_villager_xp(17);
    let offers = villager_ref.with_offers(|offers| offers.iter().count());
    assert!(offers > 0, "a farmer rolls trades");
    let source = DamageSource::direct(&vanilla_damage_types::MOB_ATTACK, Arc::clone(&zombie));
    let perished = zombie.killed_entity(
        world,
        villager.as_living_entity().expect("villager is living"),
        &source,
    );
    (fixture, perished)
}

#[test]
fn zombie_infects_a_villager_on_hard() {
    let (fixture, perished) = zombie_kills_villager(Difficulty::Hard, "zombie_infects_hard");
    let world = &fixture.world;
    assert!(!perished, "the villager comes back instead of dying");

    let converted = world
        .get_entities_in_aabb_matching(&around_villager(), |entity| {
            entity.entity_type() == &vanilla_entities::ZOMBIE_VILLAGER
        })
        .into_iter()
        .next()
        .expect("the villager becomes a zombie villager");
    let zombie_villager = converted
        .downcast_ref::<ZombieVillagerEntity>()
        .expect("converted zombie villager is concrete");
    assert_eq!(
        zombie_villager.villager_type().key,
        vanilla_villager_types::DESERT.key
    );
    assert_eq!(
        zombie_villager.profession().key,
        vanilla_villager_professions::FARMER.key
    );
    assert_eq!(zombie_villager.villager_level(), 2);
    assert_eq!(zombie_villager.villager_xp(), 17);
    assert!(
        zombie_villager
            .trade_offers()
            .is_some_and(|offers| offers.iter().count() > 0),
        "the trades come along"
    );
    assert!(!zombie_villager.is_baby(), "an adult villager stays adult");
    let villagers = world.get_entities_in_aabb_matching(&around_villager(), |entity| {
        entity.entity_type() == &vanilla_entities::VILLAGER && !entity.is_removed()
    });
    assert!(villagers.is_empty(), "the villager is replaced");
}

#[test]
fn zombie_never_infects_on_easy() {
    let (fixture, perished) = zombie_kills_villager(Difficulty::Easy, "zombie_infects_easy");
    assert!(perished, "on Easy the villager just dies");
    let converted = fixture
        .world
        .get_entities_in_aabb_matching(&around_villager(), |entity| {
            entity.entity_type() == &vanilla_entities::ZOMBIE_VILLAGER
        });
    assert!(converted.is_empty());
}

#[test]
fn zombie_villager_does_not_drown_into_a_drowned() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = flooded_world("zombie_villager_water");
    let world = &fixture.world;
    let zombie_villager = add(
        world,
        ZombieVillagerEntity::new(
            &vanilla_entities::ZOMBIE_VILLAGER,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let zombie_villager_ref = zombie_villager
        .downcast_ref::<ZombieVillagerEntity>()
        .expect("zombie villager is concrete");
    zombie_villager_ref.set_in_water_time(599);
    Arc::clone(&zombie_villager).tick();
    assert!(
        !zombie_villager_ref.is_under_water_converting(),
        "zombie villagers never convert in water"
    );
}

/// Spawns `attempts` trident-holding drowned for `spawn_reason` and counts
/// the ones that end up riding a zombie nautilus.
fn drowned_nautilus_jockeys(spawn_reason: EntitySpawnReason, attempts: usize) -> usize {
    let fixture = flooded_world("drowned_nautilus_jockey");
    let world = &fixture.world;
    let mut jockeys = 0;
    for _ in 0..attempts {
        let drowned = add(
            world,
            DrownedEntity::new(
                &vanilla_entities::DROWNED,
                next_entity_id(),
                DVec3::new(8.5, 65.0, 8.5),
                Arc::downgrade(world),
            ),
        );
        let drowned_ref = drowned
            .downcast_ref::<DrownedEntity>()
            .expect("drowned is concrete");
        let _ = drowned_ref.living_base.equipment().lock().set(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::TRIDENT),
        );
        drowned_ref.finalize_spawn(world, spawn_reason, None);
        Arc::clone(&drowned).tick();
        if let Some(vehicle) = drowned.vehicle() {
            assert_eq!(vehicle.entity_type(), &vanilla_entities::ZOMBIE_NAUTILUS);
            assert!(!drowned_ref.is_baby(), "babies never ride a nautilus");
            jockeys += 1;
        }
    }
    jockeys
}

#[test]
fn natural_drowned_with_a_trident_may_ride_a_zombie_nautilus() {
    assert!(
        drowned_nautilus_jockeys(EntitySpawnReason::Natural, 64) > 0,
        "about half of the natural trident drowned ride a zombie nautilus"
    );
}

#[test]
fn spawner_drowned_never_rides_a_zombie_nautilus() {
    assert_eq!(drowned_nautilus_jockeys(EntitySpawnReason::Spawner, 32), 0);
}
