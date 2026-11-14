use steel_registry::{init_vanilla_registry, vanilla_blocks};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use super::*;
use crate::{
    behavior::{BLOCK_BEHAVIORS, init_behaviors},
    entity::entities::ItemEntity,
    test_support::{TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk},
};

#[test]
fn registered_composter_outputs_its_full_state_level() {
    init_vanilla_registry();
    init_behaviors();
    let world = fresh_test_world("composter_analog_output");
    let state = vanilla_blocks::COMPOSTER
        .default_state()
        .set_value(LEVEL_COMPOSTER, LEVEL_COMPOSTER.max);
    let behavior = BLOCK_BEHAVIORS.get_behavior(state.get_block());

    assert!(behavior.has_analog_output_signal(state));
    assert_eq!(
        behavior.get_analog_output_signal(
            state,
            world.world.as_ref(),
            BlockPos::ZERO,
            Direction::North,
        ),
        i32::from(LEVEL_COMPOSTER.max),
    );
}

#[test]
fn compostable_layer_chances_follow_the_datapack() {
    init_vanilla_registry();
    let empty = vanilla_blocks::COMPOSTER.default_state();
    let filling = empty.set_value(LEVEL_COMPOSTER, 3);
    let mut rng = rand::rng();
    let low = vanilla_context_int_providers::get(&steel_utils::Identifier::vanilla_static(
        "compostable/low",
    ))
    .expect("the datapack has compostable/low");
    // The first layer of an empty composter always lands.
    assert!((0..200).all(|_| low.get_int(Some(empty), &mut rng) == 1));
    let added: i32 = (0..10_000)
        .map(|_| low.get_int(Some(filling), &mut rng))
        .sum();
    assert!(
        (2_500..3_500).contains(&added),
        "a low compostable adds a layer 30% of the time, got {added}/10000"
    );
    let always = vanilla_context_int_providers::get(&steel_utils::Identifier::vanilla_static(
        "compostable/always_add_one",
    ))
    .expect("the datapack has compostable/always_add_one");
    assert_eq!(always.get_int(Some(filling), &mut rng), 1);
}

const POS: BlockPos = BlockPos::new(8, 64, 8);

fn hit() -> BlockHitResult {
    BlockHitResult {
        location: DVec3::new(8.5, 65.0, 8.5),
        direction: Direction::Up,
        block_pos: POS,
        miss: false,
        inside: false,
        world_border_hit: false,
    }
}

fn composter_world(name: &'static str, level: u8) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::from_block_pos(POS));
    let state = vanilla_blocks::COMPOSTER
        .default_state()
        .set_value(LEVEL_COMPOSTER, level);
    assert!(fixture.world.set_block(POS, state, UpdateFlags::UPDATE_ALL));
    fixture
}

#[test]
fn players_fill_an_empty_composter_with_one_seed() {
    let fixture = composter_world("composter_fill", 0);
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Composter", 31).build();
    player
        .inventory
        .lock()
        .set_selected_item(ItemStack::with_count(&vanilla_items::WHEAT_SEEDS, 4));
    let mut inventory =
        InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
    let behavior = ComposterBlock::new(&vanilla_blocks::COMPOSTER);

    let result = behavior.use_item_on(
        world.get_block_state(POS),
        world,
        POS,
        &player,
        InteractionHand::MainHand,
        &hit(),
        &mut inventory,
    );

    assert_eq!(result, InteractionResult::Success);
    assert_eq!(composter_level(world.get_block_state(POS)), 1);
    let left = player.inventory.lock().get_selected_item().count();
    assert_eq!(
        left,
        if player.has_infinite_materials() {
            4
        } else {
            3
        }
    );
}

#[test]
fn non_compostable_items_fall_through_to_the_empty_hand() {
    let fixture = composter_world("composter_reject", 0);
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Composter", 32).build();
    player
        .inventory
        .lock()
        .set_selected_item(ItemStack::new(&vanilla_items::STONE));
    let mut inventory =
        InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
    let behavior = ComposterBlock::new(&vanilla_blocks::COMPOSTER);

    let result = behavior.use_item_on(
        world.get_block_state(POS),
        world,
        POS,
        &player,
        InteractionHand::MainHand,
        &hit(),
        &mut inventory,
    );
    assert_eq!(result, InteractionResult::TryEmptyHandInteraction);
    assert_eq!(
        behavior.use_without_item(
            world.get_block_state(POS),
            world,
            POS,
            &player,
            &hit(),
            &mut inventory,
        ),
        InteractionResult::Pass
    );
}

#[test]
fn a_full_composter_turns_ready_and_gives_bone_meal() {
    let fixture = composter_world("composter_ready", MAX_LEVEL);
    let world = &fixture.world;
    let behavior = ComposterBlock::new(&vanilla_blocks::COMPOSTER);

    behavior.tick(world.get_block_state(POS), world, POS);
    assert_eq!(composter_level(world.get_block_state(POS)), READY);

    let player = TestPlayerBuilder::new(Arc::clone(world), "Composter", 33).build();
    let mut inventory =
        InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
    let result = behavior.use_without_item(
        world.get_block_state(POS),
        world,
        POS,
        &player,
        &hit(),
        &mut inventory,
    );
    assert_eq!(result, InteractionResult::Success);
    assert_eq!(composter_level(world.get_block_state(POS)), 0);
    let around = WorldAabb::new(7.0, 64.0, 7.0, 10.0, 67.0, 10.0);
    let bone_meal = world
        .get_entities_in_aabb(&around)
        .into_iter()
        .any(|entity| {
            entity
                .downcast_ref::<ItemEntity>()
                .is_some_and(|item| item.get_item().is(&vanilla_items::BONE_MEAL))
        });
    assert!(bone_meal, "a bone meal pops out above the composter");
}

/// Hoppers and droppers reaching the composter's containers.
mod hopper_containers {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::item_stack::ItemStack;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::locks::shared;
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos, Direction};

    use super::super::LEVEL_COMPOSTER;
    use crate::behavior::init_behaviors;
    use crate::block_entity::entities::{
        HopperBlockEntity, add_item, hopper_suck_in_items as suck_in_items,
    };
    use crate::inventory::container::{Container as _, SimpleContainer};
    use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
    use crate::world::World;

    const COMPOSTER: BlockPos = BlockPos::new(1, 64, 1);

    fn composter_at_level(world: &Arc<World>, level: u8) {
        world.set_block(
            COMPOSTER,
            vanilla_blocks::COMPOSTER
                .default_state()
                .set_value(LEVEL_COMPOSTER, level),
            UpdateFlags::UPDATE_ALL,
        );
    }

    fn level(world: &World) -> u8 {
        world.get_block_state(COMPOSTER).get_value(LEVEL_COMPOSTER)
    }

    /// Inserts `stack` into the composter as a hopper on side `from` would.
    fn insert(world: &Arc<World>, stack: ItemStack, from: Direction) -> ItemStack {
        let targets = HopperBlockEntity::get_container_at(world, COMPOSTER)
            .expect("composters are containers");
        let ids: Vec<_> = targets.iter().map(ContainerRef::container_id).collect();
        let mut guard = ContainerLockGuard::lock_all(&targets);
        add_item(&mut guard, None, &ids, stack, Some(from))
    }

    #[test]
    fn hoppers_fill_a_composter_from_above_only() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("composter_hopper_fill");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        composter_at_level(world, 0);

        let rest = insert(
            world,
            ItemStack::with_count(&vanilla_items::PUMPKIN_PIE, 2),
            Direction::North,
        );
        assert_eq!(rest.count(), 2, "only the top face takes items");
        assert_eq!(level(world), 0);

        let rest = insert(world, ItemStack::new(&vanilla_items::STONE), Direction::Up);
        assert_eq!(rest.count(), 1, "stone is not compostable");

        // Pumpkin pie always adds a layer.
        let rest = insert(
            world,
            ItemStack::new(&vanilla_items::PUMPKIN_PIE),
            Direction::Up,
        );
        assert!(rest.is_empty());
        assert_eq!(level(world), 1);
    }

    #[test]
    fn a_full_composter_takes_nothing_until_ready() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("composter_hopper_full");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        composter_at_level(world, 7);
        let rest = insert(
            world,
            ItemStack::new(&vanilla_items::PUMPKIN_PIE),
            Direction::Up,
        );
        assert_eq!(rest.count(), 1);
        assert_eq!(level(world), 7);
    }

    #[test]
    fn hoppers_take_bone_meal_from_a_ready_composter_below_it() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("composter_hopper_take");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        composter_at_level(world, 8);
        let hopper = shared(SimpleContainer::new(5));
        let hopper_ref = ContainerRef::from(&hopper);
        // A hopper right under the composter.
        let below = DVec3::new(1.5, 63.5, 1.5);

        assert!(suck_in_items(world, &hopper_ref, below, true));
        assert_eq!(level(world), 0, "taking the bone meal empties it");
        assert_eq!(hopper.lock().count_item(&vanilla_items::BONE_MEAL), 1);

        composter_at_level(world, 3);
        assert!(
            !suck_in_items(world, &hopper_ref, below, true),
            "a composter that is not ready gives nothing"
        );
        assert_eq!(level(world), 3);
    }
}
