use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_items,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Identifier};

use super::{
    run_location_changed_effects, run_location_changed_effects_in_slot,
    stop_location_based_effects_in_slot,
};
use crate::behavior::init_behaviors;
use crate::entity::{LivingEntity, SharedEntity, create_entity_instance, init_entities};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

/// A world whose chunk at the origin has a 16x16 layer of `floor` at y = 63.
fn world_with_floor(name: &'static str, floor: BlockRef) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(BlockPos::new(8, 64, 8)));
    set_floor(world, floor);
    fixture
}

fn set_floor(world: &Arc<World>, floor: BlockRef) {
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(
                BlockPos::new(x, 63, z),
                floor.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
}

fn zombie_with_boots(world: &Arc<World>, enchantment: &str, level: u32) -> SharedEntity {
    let zombie =
        create_entity_instance(world, &vanilla_entities::ZOMBIE, DVec3::new(8.5, 64.0, 8.5))
            .expect("zombie factory");
    world
        .try_add_entity(Arc::clone(&zombie))
        .expect("zombie added");
    let mut boots = ItemStack::new(&vanilla_items::DIAMOND_BOOTS);
    boots.set_enchantments(
        &[(Identifier::vanilla(enchantment.to_string()), level)],
        false,
    );
    let living = zombie.as_living_entity().expect("zombie is living");
    living.with_equipment_slot_mut(EquipmentSlot::Feet, &mut |stack| *stack = boots.clone());
    zombie.set_on_ground(true);
    zombie
}

fn block_at(world: &Arc<World>, x: i32, z: i32) -> BlockRef {
    world.get_block_state(BlockPos::new(x, 63, z)).get_block()
}

fn movement_efficiency(living: &dyn LivingEntity) -> f64 {
    living
        .living_base()
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_EFFICIENCY)
}

#[test]
fn frost_walker_freezes_water_within_its_radius() {
    let fixture = world_with_floor("frost_walker_freezes", &vanilla_blocks::WATER);
    let world = &fixture.world;
    let zombie = zombie_with_boots(world, "frost_walker", 2);
    let living = zombie.as_living_entity().expect("zombie is living");

    zombie.set_on_ground(false);
    run_location_changed_effects(world, living);
    assert_eq!(
        block_at(world, 8, 8),
        &vanilla_blocks::WATER,
        "needs ground"
    );

    zombie.set_on_ground(true);
    run_location_changed_effects(world, living);
    // Level 2: radius 3 + 1 = 4, measured from the wearer at x = 8.5.
    assert_eq!(block_at(world, 8, 8), &vanilla_blocks::FROSTED_ICE);
    assert_eq!(block_at(world, 11, 8), &vanilla_blocks::FROSTED_ICE);
    assert_eq!(block_at(world, 12, 8), &vanilla_blocks::WATER);
    assert_eq!(block_at(world, 11, 11), &vanilla_blocks::WATER);
}

#[test]
fn frost_walker_skips_water_under_a_block() {
    let fixture = world_with_floor("frost_walker_covered", &vanilla_blocks::WATER);
    let world = &fixture.world;
    world.set_block(
        BlockPos::new(10, 64, 8),
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let zombie = zombie_with_boots(world, "frost_walker", 1);
    let living = zombie.as_living_entity().expect("zombie is living");

    run_location_changed_effects(world, living);

    assert_eq!(block_at(world, 9, 8), &vanilla_blocks::FROSTED_ICE);
    assert_eq!(block_at(world, 10, 8), &vanilla_blocks::WATER);
}

#[test]
fn soul_speed_is_active_only_on_soul_blocks() {
    let fixture = world_with_floor("soul_speed_toggles", &vanilla_blocks::SOUL_SAND);
    let world = &fixture.world;
    let zombie = zombie_with_boots(world, "soul_speed", 3);
    let living = zombie.as_living_entity().expect("zombie is living");
    let base_speed = living
        .living_base()
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_SPEED);

    run_location_changed_effects(world, living);
    assert!((movement_efficiency(living) - 1.0).abs() < 1e-9);
    let boosted = living
        .living_base()
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_SPEED);
    // Level 3: 0.0405 + 0.0105 * 2.
    assert!((boosted - base_speed - 0.0615).abs() < 1e-6, "{boosted}");

    // Still active while airborne over anything.
    set_floor(world, &vanilla_blocks::STONE);
    zombie.set_on_ground(false);
    run_location_changed_effects(world, living);
    assert!((movement_efficiency(living) - 1.0).abs() < 1e-9);

    zombie.set_on_ground(true);
    run_location_changed_effects(world, living);
    assert!(movement_efficiency(living).abs() < 1e-9);
    assert!(
        living
            .living_base()
            .active_location_enchantments()
            .lock()
            .is_empty()
    );
}

#[test]
fn removing_the_boots_stops_soul_speed() {
    let fixture = world_with_floor("soul_speed_unequip", &vanilla_blocks::SOUL_SOIL);
    let world = &fixture.world;
    let zombie = zombie_with_boots(world, "soul_speed", 1);
    let living = zombie.as_living_entity().expect("zombie is living");
    run_location_changed_effects_in_slot(world, living, EquipmentSlot::Feet);
    assert!((movement_efficiency(living) - 1.0).abs() < 1e-9);

    stop_location_based_effects_in_slot(living, EquipmentSlot::Feet);

    assert!(movement_efficiency(living).abs() < 1e-9);
    assert!(
        living
            .living_base()
            .active_location_enchantments()
            .lock()
            .is_empty()
    );
}

#[test]
fn soul_speed_lerps_the_block_speed_factor() {
    let fixture = world_with_floor("soul_speed_block_factor", &vanilla_blocks::SOUL_SAND);
    let world = &fixture.world;
    let zombie = zombie_with_boots(world, "soul_speed", 1);
    let living = zombie.as_living_entity().expect("zombie is living");
    assert!(zombie.block_speed_factor() < 1.0, "soul sand slows");

    run_location_changed_effects(world, living);

    assert!((zombie.block_speed_factor() - 1.0).abs() < 1e-6);
}
