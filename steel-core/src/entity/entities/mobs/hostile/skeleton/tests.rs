use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::vanilla_components::POTION_CONTENTS;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_items,
    vanilla_mob_effects,
};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{BoggedEntity, SkeletonEntity, StrayEntity, WitherSkeletonEntity};
use crate::behavior::init_behaviors;
use crate::entity::entities::{ArrowEntity, CowEntity, SpiderEntity};
use crate::entity::{
    AbstractArrow, Entity, EntitySpawnReason, LivingEntity, Mob, MobEffectInstance,
    RangedAttackMob, SharedEntity, Shearable, init_entities, next_entity_id,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn world_with_floor(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    let stone = vanilla_blocks::STONE.default_state();
    for x in 0..16 {
        for z in 0..16 {
            fixture
                .world
                .set_block(BlockPos::new(x, 64, z), stone, UpdateFlags::UPDATE_ALL);
        }
    }
    fixture
}

fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> SharedEntity {
    let entity: SharedEntity = Arc::new(entity);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test entity should attach to the loaded chunk");
    entity
}

fn cow(world: &Arc<World>, position: DVec3) -> SharedEntity {
    add(
        world,
        CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        ),
    )
}

fn arrows_near(world: &Arc<World>, entity: &SharedEntity) -> Vec<SharedEntity> {
    world.get_entities_in_aabb_matching(&entity.bounding_box().inflate(16.0), |candidate| {
        candidate.entity_type() == &vanilla_entities::ARROW
    })
}

fn give(mob: &dyn LivingEntity, slot: EquipmentSlot, stack: ItemStack) {
    mob.with_equipment_slot_mut(slot, &mut |held| *held = stack.clone());
}

fn reborrow(nbt: &NbtCompound) -> Vec<u8> {
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    bytes
}

#[test]
fn skeleton_spawns_with_a_bow() {
    let fixture = world_with_floor("skeleton_spawn_bow");
    let world = &fixture.world;
    let skeleton = SkeletonEntity::new(
        &vanilla_entities::SKELETON,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    );

    let _ = skeleton.finalize_spawn(world, EntitySpawnReason::Natural, None);

    assert!(
        skeleton
            .equipment_in_slot(EquipmentSlot::MainHand)
            .is(&vanilla_items::BOW)
    );
}

#[test]
fn skeleton_draws_its_bow_and_shoots_its_target() {
    let fixture = world_with_floor("skeleton_shoots");
    let world = &fixture.world;
    let skeleton = add(
        world,
        SkeletonEntity::new(
            &vanilla_entities::SKELETON,
            next_entity_id(),
            DVec3::new(3.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let skeleton_ref = skeleton
        .downcast_ref::<SkeletonEntity>()
        .expect("skeleton is concrete");
    give(
        skeleton_ref,
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::BOW),
    );
    skeleton_ref.reassess_weapon_goal();
    let target = cow(world, DVec3::new(11.5, 65.0, 8.5));
    assert!(skeleton_ref.set_target(Some(&target)));

    let mut drew_bow = false;
    for _ in 0..80 {
        Arc::clone(&skeleton).tick();
        Arc::clone(&target).tick();
        drew_bow |= skeleton_ref.is_using_item();
        if !arrows_near(world, &skeleton).is_empty() {
            break;
        }
    }

    assert!(drew_bow, "the skeleton draws its bow before shooting");
    let arrows = arrows_near(world, &skeleton);
    assert_eq!(arrows.len(), 1, "one arrow is shot");
    let arrow = arrows[0]
        .downcast_ref::<ArrowEntity>()
        .expect("skeletons shoot arrows");
    assert!(arrow.velocity().x > 0.5, "the arrow flies at the target");
    assert!(!skeleton_ref.is_using_item(), "the bow is released");
}

#[test]
fn held_item_use_sets_the_living_flags_and_counts_ticks() {
    let fixture = world_with_floor("skeleton_item_use");
    let world = &fixture.world;
    let skeleton = add(
        world,
        SkeletonEntity::new(
            &vanilla_entities::SKELETON,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let skeleton_ref = skeleton
        .downcast_ref::<SkeletonEntity>()
        .expect("skeleton is concrete");
    give(
        skeleton_ref,
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::BOW),
    );
    skeleton_ref.set_no_ai(true);

    skeleton_ref.start_using_held_item(InteractionHand::MainHand);
    assert!(skeleton_ref.is_using_item());
    assert_eq!(
        *skeleton_ref
            .entity_data
            .lock()
            .living_entity()
            .living_entity_flags
            .get()
            & 1,
        1
    );
    for _ in 0..5 {
        Arc::clone(&skeleton).tick();
    }
    assert_eq!(skeleton_ref.ticks_using_item(), 5);

    skeleton_ref.stop_using_held_item();
    assert!(!skeleton_ref.is_using_item());
    assert_eq!(
        *skeleton_ref
            .entity_data
            .lock()
            .living_entity()
            .living_entity_flags
            .get()
            & 1,
        0
    );
}

#[test]
fn stray_arrows_slow_their_target() {
    let fixture = world_with_floor("stray_arrow");
    let world = &fixture.world;
    let stray = add(
        world,
        StrayEntity::new(
            &vanilla_entities::STRAY,
            next_entity_id(),
            DVec3::new(3.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let stray_ref = stray
        .downcast_ref::<StrayEntity>()
        .expect("stray is concrete");
    give(
        stray_ref,
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::BOW),
    );
    let target = cow(world, DVec3::new(11.5, 65.0, 8.5));

    stray_ref.perform_ranged_attack(&stray, &target, 1.0);

    let arrows = arrows_near(world, &stray);
    let arrow = arrows
        .first()
        .and_then(|arrow| arrow.downcast_ref::<ArrowEntity>())
        .expect("the stray shoots an arrow");
    let contents = arrow
        .pickup_item_stack()
        .get(POTION_CONTENTS)
        .cloned()
        .expect("the arrow carries an effect");
    assert_eq!(contents.custom_effects().len(), 1);
    assert_eq!(
        contents.custom_effects()[0].effect(),
        vanilla_mob_effects::SLOWNESS
    );
    assert_eq!(contents.custom_effects()[0].duration(), 600);
    assert_ne!(arrow.color(), -1, "a potion arrow is tinted");
}

#[test]
fn wither_skeleton_carries_a_sword_and_withers() {
    let fixture = world_with_floor("wither_skeleton");
    let world = &fixture.world;
    let wither_skeleton = add(
        world,
        WitherSkeletonEntity::new(
            &vanilla_entities::WITHER_SKELETON,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let mob = wither_skeleton
        .downcast_ref::<WitherSkeletonEntity>()
        .expect("wither skeleton is concrete");
    let _ = mob.finalize_spawn(world, EntitySpawnReason::Natural, None);

    assert!(
        mob.equipment_in_slot(EquipmentSlot::MainHand)
            .is(&vanilla_items::STONE_SWORD)
    );
    assert_eq!(
        mob.attributes()
            .lock()
            .get_base_value(vanilla_attributes::ATTACK_DAMAGE),
        Some(4.0)
    );
    assert!(!mob.can_be_affected(&MobEffectInstance::new(vanilla_mob_effects::WITHER, 0)));
    assert!(mob.can_be_affected(&MobEffectInstance::new(vanilla_mob_effects::SLOWNESS, 0)));

    let target = cow(world, DVec3::new(9.5, 65.0, 8.5));
    assert!(mob.do_hurt_target(&wither_skeleton, world, &target));
    let wither = target
        .as_living_entity()
        .expect("cow is living")
        .mob_effect(vanilla_mob_effects::WITHER)
        .expect("the hit withers");
    assert_eq!(wither.duration(), 200);

    // Its arrows (from a bow it picked up) are on fire.
    give(
        mob,
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::BOW),
    );
    mob.perform_ranged_attack(&wither_skeleton, &target, 1.0);
    let arrow = arrows_near(world, &wither_skeleton)
        .into_iter()
        .next()
        .expect("the wither skeleton shoots");
    assert!(arrow.is_on_fire());
}

#[test]
fn skeleton_freezes_into_a_stray_in_powder_snow() {
    let fixture = world_with_floor("skeleton_freezes");
    let world = &fixture.world;
    world.set_block(
        BlockPos::new(8, 65, 8),
        vanilla_blocks::POWDER_SNOW.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    // Walls keep the skeleton from wandering out of the snow.
    for (x, z) in [(7, 8), (9, 8), (8, 7), (8, 9)] {
        for y in 65..67 {
            world.set_block(
                BlockPos::new(x, y, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    let skeleton = add(
        world,
        SkeletonEntity::new(
            &vanilla_entities::SKELETON,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let skeleton_ref = skeleton
        .downcast_ref::<SkeletonEntity>()
        .expect("skeleton is concrete");

    for _ in 0..(140 + 2) {
        Arc::clone(&skeleton).tick();
    }
    assert!(
        skeleton_ref.is_freeze_converting(),
        "140 ticks in powder snow start the conversion"
    );

    let mut nbt = NbtCompound::new();
    skeleton_ref.save_additional(&mut nbt);
    assert!(nbt.int("StrayConversionTime").is_some_and(|time| time > 0));

    skeleton_ref.start_freeze_conversion(0);
    Arc::clone(&skeleton).tick();

    assert!(skeleton.is_removed(), "the skeleton is replaced");
    let stray = world
        .get_entities_in_aabb_matching(&skeleton.bounding_box().inflate(1.0), |entity| {
            entity.entity_type() == &vanilla_entities::STRAY
        });
    assert_eq!(stray.len(), 1, "the skeleton becomes a stray");
}

#[test]
fn skeleton_conversion_state_round_trips() {
    init_vanilla_registry();
    let skeleton = SkeletonEntity::new(&vanilla_entities::SKELETON, 1, DVec3::ZERO, Weak::new());
    skeleton.start_freeze_conversion(123);

    let mut nbt = NbtCompound::new();
    skeleton.save_additional(&mut nbt);
    assert_eq!(nbt.int("StrayConversionTime"), Some(123));
    assert_eq!(
        nbt.int("FreezingTime"),
        Some(-1),
        "not standing in powder snow"
    );

    let bytes = reborrow(&nbt);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    let loaded = SkeletonEntity::new(&vanilla_entities::SKELETON, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert!(loaded.is_freeze_converting());
}

#[test]
fn bogged_is_sheared_once() {
    let fixture = world_with_floor("bogged_shear");
    let world = &fixture.world;
    let bogged = add(
        world,
        BoggedEntity::new(
            &vanilla_entities::BOGGED,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let bogged_ref = bogged
        .downcast_ref::<BoggedEntity>()
        .expect("bogged is concrete");
    assert!(bogged_ref.ready_for_shearing());

    bogged_ref.shear(
        world,
        SoundSource::Players,
        &ItemStack::new(&vanilla_items::SHEARS),
    );

    assert!(bogged_ref.is_sheared());
    assert!(!bogged_ref.ready_for_shearing());
    let mushrooms = world
        .get_entities_in_aabb_matching(&bogged.bounding_box().inflate(2.0), |entity| {
            entity.entity_type() == &vanilla_entities::ITEM
        });
    assert!(!mushrooms.is_empty(), "shearing drops mushrooms");

    let mut nbt = NbtCompound::new();
    bogged_ref.save_additional(&mut nbt);
    assert_eq!(nbt.byte("sheared"), Some(1));
}

#[test]
fn spider_jockey_rider_mounts_its_spider() {
    let fixture = world_with_floor("spider_jockey");
    let world = &fixture.world;
    let spider = add(
        world,
        SpiderEntity::new(
            &vanilla_entities::SPIDER,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let spider_ref = spider
        .downcast_ref::<SpiderEntity>()
        .expect("spider is concrete");

    spider_ref.spawn_skeleton_jockey_rider(world, EntitySpawnReason::Natural);
    let rider = world
        .get_entities_in_aabb_matching(&spider.bounding_box().inflate(1.0), |entity| {
            entity.entity_type() == &vanilla_entities::SKELETON
        })
        .into_iter()
        .next()
        .expect("a skeleton is spawned on the spider");
    Arc::clone(&rider).tick();

    assert!(rider.is_passenger());
    assert_eq!(
        rider.vehicle().map(|vehicle| vehicle.id()),
        Some(spider.id())
    );
    let rider_ref = rider
        .downcast_ref::<SkeletonEntity>()
        .expect("the rider is a skeleton");
    assert!(
        rider_ref
            .equipment_in_slot(EquipmentSlot::MainHand)
            .is(&vanilla_items::BOW)
    );
}

#[test]
fn melee_skeleton_without_a_bow() {
    let fixture = world_with_floor("skeleton_melee");
    let world = &fixture.world;
    let skeleton = add(
        world,
        SkeletonEntity::new(
            &vanilla_entities::SKELETON,
            next_entity_id(),
            DVec3::new(3.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let skeleton_ref = skeleton
        .downcast_ref::<SkeletonEntity>()
        .expect("skeleton is concrete");
    // A helmet keeps it from burning (and fleeing the sun) in daylight.
    give(
        skeleton_ref,
        EquipmentSlot::Head,
        ItemStack::new(&vanilla_items::LEATHER_HELMET),
    );
    let target = cow(world, DVec3::new(11.5, 65.0, 8.5));
    assert!(skeleton_ref.set_target(Some(&target)));

    // MeleeAttackGoal checks canUse at most once a second of game time.
    for _ in 0..30 {
        world.level_data.write().advance_game_time();
        Arc::clone(&skeleton).tick();
    }

    assert!(!skeleton_ref.is_using_item(), "no bow to draw");
    assert!(arrows_near(world, &skeleton).is_empty());
    assert!(skeleton_ref.is_aggressive(), "it chases in melee instead");
}
