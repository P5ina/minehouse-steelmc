use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::data_components::vanilla_components::{INTANGIBLE_PROJECTILE, POTION_CONTENTS};
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_enchantments, vanilla_entities, vanilla_items,
    vanilla_mob_effects,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{ArrowEntity, SpectralArrowEntity};
use crate::behavior::items::projectile_weapon::{AmmoSlot, find_player_arrow};
use crate::behavior::{ITEM_BEHAVIORS, init_behaviors};
use crate::entity::entities::CowEntity;
use crate::entity::{AbstractArrow, ArrowPickup, Entity, SharedEntity, next_entity_id};
use crate::inventory::container::Container as _;
use crate::player::Player;
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, enchanted_item, fresh_test_world, insert_ready_full_chunk,
};
use crate::world::World;

fn world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

fn spawn_arrow(world: &Arc<World>, position: DVec3, velocity: DVec3) -> Arc<ArrowEntity> {
    let arrow = Arc::new(ArrowEntity::new(
        &vanilla_entities::ARROW,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    arrow.set_velocity(velocity);
    world
        .try_add_entity(Arc::clone(&arrow) as SharedEntity)
        .expect("arrow should enter the loaded chunk");
    arrow
}

fn spawn_cow(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let cow: SharedEntity = Arc::new(CowEntity::new(
        &vanilla_entities::COW,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&cow))
        .expect("cow should enter the loaded chunk");
    cow
}

fn tick_until(arrow: &Arc<ArrowEntity>, ticks: usize, done: impl Fn(&ArrowEntity) -> bool) {
    for _ in 0..ticks {
        if done(arrow) || arrow.is_removed() {
            return;
        }
        Arc::clone(arrow).tick();
    }
}

#[test]
fn arrow_sticks_in_the_block_it_hits() {
    let fixture = world("arrow_sticks");
    let world = &fixture.world;
    world.set_block(
        BlockPos::new(10, 65, 8),
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let arrow = spawn_arrow(world, DVec3::new(5.5, 65.5, 8.5), DVec3::new(1.5, 0.0, 0.0));
    arrow.set_crit_arrow(true);

    tick_until(&arrow, 20, AbstractArrow::is_in_ground);

    assert!(arrow.is_in_ground(), "the arrow stops in the stone");
    assert_eq!(arrow.velocity(), DVec3::ZERO);
    assert!(
        !arrow.is_crit_arrow(),
        "hitting a block clears the crit flag"
    );
    assert_eq!(arrow.shake_time(), 7);
    assert!(
        (arrow.position().x - 9.95).abs() < 1e-6,
        "the arrow sits 0.05 outside the face, at {}",
        arrow.position().x
    );

    // Stuck arrows stay put and despawn after 1200 ticks in the ground.
    for _ in 0..1199 {
        Arc::clone(&arrow).tick();
    }
    assert!(!arrow.is_removed());
    Arc::clone(&arrow).tick();
    assert!(
        arrow.is_removed(),
        "1200 ticks in the ground despawn the arrow"
    );
}

#[test]
fn stuck_arrow_falls_when_its_block_is_removed() {
    let fixture = world("arrow_falls");
    let world = &fixture.world;
    let wall = BlockPos::new(10, 65, 8);
    world.set_block(
        wall,
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let arrow = spawn_arrow(world, DVec3::new(5.5, 65.5, 8.5), DVec3::new(1.5, 0.0, 0.0));
    tick_until(&arrow, 20, AbstractArrow::is_in_ground);
    assert!(arrow.is_in_ground());

    world.set_block(
        wall,
        vanilla_blocks::AIR.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    Arc::clone(&arrow).tick();

    assert!(
        !arrow.is_in_ground(),
        "with nothing to hold it the arrow falls"
    );
}

#[test]
fn arrow_damage_scales_with_speed() {
    let fixture = world("arrow_damage");
    let world = &fixture.world;
    let cow = spawn_cow(world, DVec3::new(8.5, 65.0, 8.5));
    let arrow = spawn_arrow(world, DVec3::new(5.5, 65.5, 8.5), DVec3::new(2.0, 0.0, 0.0));

    tick_until(&arrow, 5, |_| false);

    let cow_living = cow.as_living_entity().expect("cow is living");
    // Second tick: speed |(1.98, -0.05, 0)| * 2 base damage, rounded up = 4.
    assert_eq!(cow_living.get_health(), 6.0);
    assert!(arrow.is_removed(), "a non-piercing arrow is used up");
    assert_eq!(cow_living.arrow_count(), 1);
}

#[test]
fn piercing_arrow_passes_through_its_targets() {
    let fixture = world("arrow_piercing");
    let world = &fixture.world;
    let first = spawn_cow(world, DVec3::new(8.5, 65.0, 8.5));
    let second = spawn_cow(world, DVec3::new(10.5, 65.0, 8.5));
    let arrow = spawn_arrow(world, DVec3::new(5.5, 65.5, 8.5), DVec3::new(3.0, 0.0, 0.0));
    arrow.set_pierce_level(1);

    tick_until(&arrow, 4, |_| false);

    for cow in [&first, &second] {
        let health = cow.as_living_entity().expect("cow is living").get_health();
        assert!(health < 10.0, "both cows are hit, got {health}");
        assert_eq!(
            cow.as_living_entity().expect("cow is living").arrow_count(),
            0,
            "piercing arrows do not stick in their targets"
        );
    }
}

#[test]
fn tipped_arrow_applies_its_potion_at_an_eighth_of_the_duration() {
    let fixture = world("arrow_tipped");
    let world = &fixture.world;
    let cow = spawn_cow(world, DVec3::new(8.5, 65.0, 8.5));
    let tipped = ITEM_BEHAVIORS.default_instance(&vanilla_items::TIPPED_ARROW);
    let contents = tipped
        .get(POTION_CONTENTS)
        .cloned()
        .expect("the default tipped arrow is poisoned");
    let arrow = Arc::new(ArrowEntity::with_items(
        &vanilla_entities::ARROW,
        next_entity_id(),
        DVec3::new(5.5, 65.5, 8.5),
        Arc::downgrade(world),
        &tipped,
        None,
    ));
    assert_eq!(arrow.color(), contents.get_color());
    arrow.set_velocity(DVec3::new(2.0, 0.0, 0.0));
    world
        .try_add_entity(Arc::clone(&arrow) as SharedEntity)
        .expect("arrow should enter the loaded chunk");

    tick_until(&arrow, 5, |_| false);

    let poison = cow
        .as_living_entity()
        .expect("cow is living")
        .mob_effect(vanilla_mob_effects::POISON)
        .expect("the tipped arrow poisons its target");
    // Poison lasts 900 ticks; the tipped arrow scales it by 1/8.
    assert!(poison.duration() <= 112 && poison.duration() > 100);
}

#[test]
fn spectral_arrow_makes_its_target_glow() {
    let fixture = world("arrow_spectral");
    let world = &fixture.world;
    let cow = spawn_cow(world, DVec3::new(8.5, 65.0, 8.5));
    let arrow = Arc::new(SpectralArrowEntity::new(
        &vanilla_entities::SPECTRAL_ARROW,
        next_entity_id(),
        DVec3::new(5.5, 65.5, 8.5),
        Arc::downgrade(world),
    ));
    arrow.set_velocity(DVec3::new(2.0, 0.0, 0.0));
    world
        .try_add_entity(Arc::clone(&arrow) as SharedEntity)
        .expect("arrow should enter the loaded chunk");

    for _ in 0..5 {
        if arrow.is_removed() {
            break;
        }
        Arc::clone(&arrow).tick();
    }

    let glowing = cow
        .as_living_entity()
        .expect("cow is living")
        .mob_effect(vanilla_mob_effects::GLOWING)
        .expect("the spectral arrow makes the cow glow");
    assert!(glowing.duration() <= 200 && glowing.duration() > 190);
}

#[test]
fn arrow_saves_and_loads_vanilla_fields() {
    init_vanilla_registry();
    let arrow = ArrowEntity::new(&vanilla_entities::ARROW, 1, DVec3::ZERO, Weak::new());
    arrow.set_crit_arrow(true);
    arrow.set_pierce_level(3);
    arrow.set_base_damage(4.5);
    arrow.set_pickup(ArrowPickup::CreativeOnly);

    let mut nbt = NbtCompound::new();
    arrow.save_additional(&mut nbt);
    assert_eq!(nbt.byte("crit"), Some(1));
    assert_eq!(nbt.byte("PierceLevel"), Some(3));
    assert_eq!(nbt.double("damage"), Some(4.5));
    assert_eq!(nbt.byte("pickup"), Some(2));
    assert_eq!(nbt.short("life"), Some(0));

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    let loaded = ArrowEntity::new(&vanilla_entities::ARROW, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());

    assert!(loaded.is_crit_arrow());
    assert_eq!(loaded.pierce_level(), 3);
    assert_eq!(loaded.base_damage(), 4.5);
    assert_eq!(loaded.pickup(), ArrowPickup::CreativeOnly);
    assert!(loaded.get_pickup_item().is(&vanilla_items::ARROW));
}

fn joined_player(world: &Arc<World>) -> Arc<Player> {
    let player = TestPlayerBuilder::new(Arc::clone(world), "Archer", next_entity_id()).build();
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    player
}

#[test]
fn player_picks_up_a_stuck_arrow() {
    let fixture = world("arrow_pickup");
    let world = &fixture.world;
    let player = joined_player(world);
    let arrow = spawn_arrow(world, DVec3::new(0.5, 65.0, 0.5), DVec3::ZERO);
    arrow.set_in_ground(true);

    arrow.set_pickup(ArrowPickup::Disallowed);
    Arc::clone(&arrow).player_touch(&player);
    assert!(!arrow.is_removed(), "a disallowed arrow stays");

    arrow.set_pickup(ArrowPickup::Allowed);
    Arc::clone(&arrow).player_touch(&player);
    assert!(arrow.is_removed());
    let found = find_player_arrow(&player);
    assert!(
        matches!(found, Some((AmmoSlot::Hand(_) | AmmoSlot::Inventory(_), ref stack)) if stack.is(&vanilla_items::ARROW)),
        "the arrow went into the inventory"
    );
}

fn shoot_full_bow(world: &Arc<World>, player: &Arc<Player>, bow: ItemStack) -> Vec<SharedEntity> {
    let behavior = ITEM_BEHAVIORS.get_behavior(&vanilla_items::BOW);
    let mut bow = bow;
    // 20 ticks of drawing is full power.
    let shot = behavior.release_using(&mut bow, world, player.as_ref(), 72000 - 20);
    assert!(shot, "a fully drawn bow fires");
    world.get_entities_in_aabb_matching(&player.bounding_box().inflate(10.0), |entity| {
        entity.entity_type() == &vanilla_entities::ARROW
    })
}

#[test]
fn bow_fires_a_critical_arrow_and_uses_ammo() {
    let fixture = world("bow_shoot");
    let world = &fixture.world;
    let player = joined_player(world);
    player
        .inventory
        .lock()
        .set_item(5, ItemStack::with_count(&vanilla_items::ARROW, 3));

    let arrows = shoot_full_bow(world, &player, ItemStack::new(&vanilla_items::BOW));

    assert_eq!(arrows.len(), 1);
    let arrow = arrows[0]
        .downcast_ref::<ArrowEntity>()
        .expect("the bow shoots an arrow");
    assert!(arrow.is_crit_arrow(), "full draw is a critical shot");
    assert_eq!(arrow.pickup(), ArrowPickup::Allowed);
    assert!((arrow.velocity().length() - 3.0).abs() < 0.2);
    assert_eq!(player.inventory.lock().get_item(5).count(), 2);
}

#[test]
fn infinity_bow_keeps_its_arrow() {
    let fixture = world("bow_infinity");
    let world = &fixture.world;
    let player = joined_player(world);
    player
        .inventory
        .lock()
        .set_item(5, ItemStack::with_count(&vanilla_items::ARROW, 1));
    let bow = enchanted_item(
        &vanilla_items::BOW,
        vanilla_enchantments::INFINITY.key.clone(),
        1,
    );

    let arrows = shoot_full_bow(world, &player, bow);

    assert_eq!(arrows.len(), 1);
    let arrow = arrows[0]
        .downcast_ref::<ArrowEntity>()
        .expect("the bow shoots an arrow");
    assert_eq!(
        arrow.pickup(),
        ArrowPickup::CreativeOnly,
        "an Infinity arrow cannot be picked back up"
    );
    assert!(arrow.pickup_item_stack().has(INTANGIBLE_PROJECTILE));
    assert_eq!(player.inventory.lock().get_item(5).count(), 1);
}

#[test]
fn bow_power_follows_the_vanilla_curve() {
    use crate::behavior::items::bow::get_power_for_time;
    assert_eq!(get_power_for_time(0), 0.0);
    assert!((get_power_for_time(10) - (0.25 + 1.0) / 3.0).abs() < 1e-6);
    assert_eq!(get_power_for_time(20), 1.0);
    assert_eq!(get_power_for_time(100), 1.0);
}
