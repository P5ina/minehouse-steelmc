use std::sync::Arc;

use super::{CraftingKind, crafting};
use crate::inventory::menu::Menu;
use crate::player::Player;
use crate::{
    behavior::init_behaviors,
    entity::{Entity as _, entities::ItemEntity},
    inventory::{
        click::{Click, MouseButton},
        container::Container as _,
    },
    test_support::{TestPlayerBuilder, fresh_test_world, insert_ready_full_chunk},
};
use glam::DVec3;
use steel_registry::{
    init_vanilla_registry, item_stack::ItemStack, vanilla_blocks, vanilla_entities, vanilla_items,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _, WorldAabb};

#[test]
fn partial_result_overflow_uses_the_default_drop_policy() {
    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world("crafting_menu_partial_result_overflow");
    let world = &world_fixture.world;
    let pos = BlockPos::new(0, 64, 0);
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
    assert!(world.set_block(
        pos,
        vanilla_blocks::CRAFTING_TABLE.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Crafter", 1).build();
    player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
    let mut menu = crafting(Arc::clone(&player.inventory), 1, pos);
    let Some(kind) = menu.kind().downcast_ref::<CraftingKind>() else {
        panic!("crafting builder should create a crafting menu");
    };
    let (crafting_container, result_id) =
        (kind.handler.crafting_container(), kind.handler.result_id());

    *menu.behavior_mut().carried_mut() = ItemStack::new(&vanilla_items::OAK_LOG);
    menu.clicked(
        Click::Pickup {
            slot: 1,
            button: MouseButton::Left,
        },
        &player,
    );
    let result = menu
        .behavior()
        .lock_all_containers()
        .get(result_id)
        .expect("result container is registered with the menu")
        .get_item(0)
        .clone();
    assert!(result.is(&vanilla_items::OAK_PLANKS));
    assert_eq!(result.count(), 4);

    let mut matching = result;
    matching.set_count(63);
    {
        let mut inventory = player.inventory.lock();
        for slot in 0..36 {
            inventory.set_item(slot, ItemStack::with_count(&vanilla_items::DIRT, 64));
        }
        inventory.set_item(8, matching);
    }

    menu.clicked(Click::QuickMove { slot: 0 }, &player);

    assert!(crafting_container.lock().get_item(0).is_empty());
    assert!(
        menu.behavior()
            .lock_all_containers()
            .get(result_id)
            .expect("result container is registered with the menu")
            .get_item(0)
            .is_empty()
    );
    assert_eq!(player.inventory.lock().get_item(8).count(), 64);
    let dropped = world.get_entities_in_aabb_matching(
        &WorldAabb::new(-2.0, 62.0, -2.0, 2.0, 68.0, 2.0),
        |entity| entity.entity_type() == &vanilla_entities::ITEM,
    );
    assert_eq!(dropped.len(), 1);
    let Some(item) = dropped[0].as_ref().downcast_ref::<ItemEntity>() else {
        panic!("dropped entity should retain its concrete item type");
    };
    assert!(item.get_item().is(&vanilla_items::OAK_PLANKS));
    assert_eq!(item.get_item().count(), 3);
    assert_eq!(item.get_thrower(), None);
}

/// Fills the grid with paper around `map` (the last paper is clicked in so the menu sees the
/// change) and returns what the result slot offers.
fn extend_map_in_grid(menu: &mut Menu, player: &Arc<Player>, map: ItemStack) -> ItemStack {
    let Some(kind) = menu.kind().downcast_ref::<CraftingKind>() else {
        panic!("crafting builder should create a crafting menu");
    };
    let (crafting_container, result_id) =
        (kind.handler.crafting_container(), kind.handler.result_id());
    {
        let mut grid = crafting_container.lock();
        for slot in 0..8 {
            grid.set_item(slot, ItemStack::new(&vanilla_items::PAPER));
        }
        grid.set_item(4, map);
    }
    *menu.behavior_mut().carried_mut() = ItemStack::new(&vanilla_items::PAPER);
    menu.clicked(
        Click::Pickup {
            slot: 9,
            button: MouseButton::Left,
        },
        player,
    );
    menu.behavior()
        .lock_all_containers()
        .get(result_id)
        .expect("result container is registered with the menu")
        .get_item(0)
        .clone()
}

#[test]
fn taking_an_extended_map_gives_it_a_new_zoomed_out_map() {
    use steel_registry::data_components::vanilla_components::{MAP_ID, MAP_POST_PROCESSING};

    use crate::behavior::items::create_map;

    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world("crafting_menu_map_extending");
    let world = &world_fixture.world;
    let pos = BlockPos::new(0, 64, 0);
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
    assert!(world.set_block(
        pos,
        vanilla_blocks::CRAFTING_TABLE.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Cartographer", 1).build();
    player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
    let mut menu = crafting(Arc::clone(&player.inventory), 1, pos);

    let map = create_map(world, 300, 40, 0, true, false);
    let source_id = *map.get(MAP_ID).expect("map id");
    let offered = extend_map_in_grid(&mut menu, &player, map);
    assert!(offered.is(&vanilla_items::FILLED_MAP));
    assert!(offered.has(MAP_POST_PROCESSING));
    assert_eq!(offered.get(MAP_ID), Some(&source_id));

    menu.clicked(
        Click::Pickup {
            slot: 0,
            button: MouseButton::Left,
        },
        &player,
    );
    let taken = menu.behavior().carried().clone();
    assert!(!taken.has(MAP_POST_PROCESSING));
    let new_id = *taken.get(MAP_ID).expect("the result keeps a map id");
    assert_ne!(new_id, source_id);
    {
        let data = world.map_data(new_id).expect("the zoomed out map exists");
        let data = data.lock();
        assert_eq!((data.scale, data.center_x, data.center_z), (1, 320, 64));
    }
    assert_eq!(
        world.map_data(source_id).expect("source map").lock().scale,
        0
    );

    // A shift-click takes the result through the same finishing step.
    *menu.behavior_mut().carried_mut() = ItemStack::empty();
    let zoomed_once = taken;
    let offered = extend_map_in_grid(&mut menu, &player, zoomed_once);
    assert!(offered.has(MAP_POST_PROCESSING));
    menu.clicked(Click::QuickMove { slot: 0 }, &player);
    let moved = {
        let inventory = player.inventory.lock();
        (0..36)
            .map(|slot| inventory.get_item(slot).clone())
            .find(|stack| stack.is(&vanilla_items::FILLED_MAP))
            .expect("the shift-clicked map lands in the inventory")
    };
    assert!(!moved.has(MAP_POST_PROCESSING));
    let zoomed_twice = *moved.get(MAP_ID).expect("map id");
    assert_ne!(zoomed_twice, new_id);
    assert_eq!(
        world
            .map_data(zoomed_twice)
            .expect("zoomed map")
            .lock()
            .scale,
        2
    );
}

#[test]
fn maps_at_the_widest_zoom_do_not_extend() {
    use crate::behavior::items::create_map;

    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world("crafting_menu_map_extending_max");
    let world = &world_fixture.world;
    let pos = BlockPos::new(0, 64, 0);
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
    assert!(world.set_block(
        pos,
        vanilla_blocks::CRAFTING_TABLE.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Cartographer", 1).build();
    player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
    let mut menu = crafting(Arc::clone(&player.inventory), 1, pos);

    let map = create_map(world, 0, 0, 4, true, false);
    assert!(extend_map_in_grid(&mut menu, &player, map).is_empty());
}
