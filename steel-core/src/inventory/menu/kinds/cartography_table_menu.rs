//! Cartography table menu.
use crate::player::player_inventory::Prediction;
use std::sync::Arc;

use steel_registry::{vanilla_blocks, vanilla_menu_types};
use steel_utils::{
    BlockPos,
    locks::{IntoShared, Shared},
};

use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        menu::builder::SectionKind,
        prelude::*,
        slots::{
            CARTOGRAPHY_ADDITIONAL_SLOT, CARTOGRAPHY_MAP_SLOT, CartographyResultHandler,
            cartography_result, is_cartography_additional, is_cartography_map,
        },
    },
    player::player_inventory::PlayerInventory,
    world::World,
};

/// Builds Vanilla's `CartographyTableMenu`.
#[must_use]
pub fn cartography_table(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    pos: BlockPos,
    world: &Arc<World>,
) -> Menu {
    let input_container = SimpleContainer::new(2).into_shared();
    let result_container = ResultContainer::new().into_shared();

    let mut builder = MenuBuilder::new(&vanilla_menu_types::CARTOGRAPHY_TABLE, container_id);

    let inputs = builder.section_all_with(
        &input_container,
        SectionKind::restricted(|index, stack| match index {
            CARTOGRAPHY_MAP_SLOT => is_cartography_map(stack),
            _ => is_cartography_additional(stack),
        }),
    );
    let result = builder.result_slot(CartographyResultHandler::new(
        Arc::clone(&input_container),
        Arc::clone(&result_container),
        pos,
        Arc::clone(world),
    ));
    let player = builder.player_inventory(&inventory);

    builder.route_with_remainder_policy(
        result,
        player.all(),
        FillDirection::Backward,
        FakeResultRemainderPolicy::Drop,
    );
    builder.route(inputs, player.all(), FillDirection::Forward);
    builder.drain(inputs);

    builder.build(CartographyTableKind {
        input_container,
        result_container,
        block_pos: pos,
        world: Arc::clone(world),
        inputs,
        result,
        player,
    })
}

/// Per-menu cartography table state: the map, the additional item and the virtual
/// result.
pub struct CartographyTableKind {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    block_pos: BlockPos,
    world: Arc<World>,
    inputs: Section,
    result: Section,
    player: PlayerInventorySections,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for CartographyTableKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/cartography_table");
}

impl MenuKind for CartographyTableKind {
    /// Vanilla `CartographyTableMenu.stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        still_valid_block(
            &self.world,
            self.block_pos,
            &vanilla_blocks::CARTOGRAPHY_TABLE,
            player,
        )
    }

    /// Vanilla `CartographyTableMenu.slotsChanged`.
    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        let Some((map, additional)) =
            guard
                .get(ContainerId::from_arc(&self.input_container))
                .map(|input| {
                    (
                        input.get_item(CARTOGRAPHY_MAP_SLOT).clone(),
                        input.get_item(CARTOGRAPHY_ADDITIONAL_SLOT).clone(),
                    )
                })
        else {
            return;
        };
        let Some(result_container) = guard.get_mut(ContainerId::from_arc(&self.result_container))
        else {
            return;
        };
        let current = result_container.get_item(0).clone();
        if let Some(result) = cartography_result(&self.world, &map, &additional, &current) {
            result_container.set_item(0, result);
        }
    }

    /// Vanilla `CartographyTableMenu.removed` clears the result; the inputs are
    /// drained by [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    /// Vanilla `CartographyTableMenu.canTakeItemForPickAll`.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        !self.result.contains(slot_index)
    }

    /// Vanilla `CartographyTableMenu.quickMoveStack` for the player's slots; the
    /// result and inputs use the route table.
    fn quick_move(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> Option<ItemStack> {
        if !self.player.all().contains(slot_index) {
            return None;
        }

        let clicked = behavior.slots()[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return Some(ItemStack::empty());
        }

        let input_slot = |slot: usize| self.inputs.start() + slot;
        let (start, end) = if is_cartography_map(&clicked) {
            (
                input_slot(CARTOGRAPHY_MAP_SLOT),
                input_slot(CARTOGRAPHY_MAP_SLOT) + 1,
            )
        } else if is_cartography_additional(&clicked) {
            (
                input_slot(CARTOGRAPHY_ADDITIONAL_SLOT),
                input_slot(CARTOGRAPHY_ADDITIONAL_SLOT) + 1,
            )
        } else if self.player.main().contains(slot_index) {
            (self.player.hotbar().start(), self.player.hotbar().end())
        } else {
            (self.player.main().start(), self.player.main().end())
        };

        let mut remaining = clicked.clone();
        let moved = behavior.move_item_stack_to(
            guard,
            slot_index,
            &mut remaining,
            start,
            end,
            FillDirection::Forward,
        );
        if !moved {
            return Some(ItemStack::empty());
        }

        behavior.update_quick_move_source(guard, slot_index, &remaining, &clicked);
        if remaining.count() == clicked.count() {
            return Some(ItemStack::empty());
        }
        if let Some(remainder) = behavior.slots()[slot_index].on_take(guard, &mut remaining, player)
        {
            player.add_item_or_drop_with_guard(guard, remainder, Prediction::Predicted);
        }
        Some(clicked)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::data_components::vanilla_components::{MAP_ID, MAP_POST_PROCESSING};
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, types::UpdateFlags};

    use super::{CartographyTableKind, cartography_table};
    use crate::{
        behavior::{init_behaviors, items::create_map},
        entity::Entity as _,
        inventory::{
            click::Click,
            container::{Container as _, ResultContainer},
            menu::Menu,
            prelude::*,
        },
        test_support::{TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk},
    };

    const RESULT_SLOT: usize = 2;
    /// Menu slot of the player's first main inventory slot (inventory slot 9).
    const FIRST_MAIN_SLOT: usize = 3;

    fn test_table(key: &'static str) -> (TestWorld, Arc<Player>, Menu) {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world(key);
        let world = &world_fixture.world;
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::CARTOGRAPHY_TABLE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "Cartographer", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
        let menu = cartography_table(Arc::clone(&player.inventory), 1, pos, world);
        (world_fixture, player, menu)
    }

    fn parts(menu: &Menu) -> (Shared<SimpleContainer>, Shared<ResultContainer>) {
        let Some(kind) = menu.kind().downcast_ref::<CartographyTableKind>() else {
            panic!("cartography builder should create a cartography menu");
        };
        (
            Arc::clone(&kind.input_container),
            Arc::clone(&kind.result_container),
        )
    }

    /// Shift-clicks `stack` in from the player's first main slot.
    fn shift_in(menu: &mut Menu, player: &Arc<Player>, stack: ItemStack) {
        player.inventory.lock().set_item(9, stack);
        menu.clicked(
            Click::QuickMove {
                slot: FIRST_MAIN_SLOT,
            },
            player,
        );
    }

    #[test]
    fn paper_zooms_a_map_out_and_glass_locks_it() {
        let (world_fixture, player, mut menu) = test_table("cartography_scale_lock");
        let world = &world_fixture.world;
        let (input, result) = parts(&menu);
        let map = create_map(world, 0, 0, 0, true, false);
        let source_id = *map.get(MAP_ID).expect("map id");

        shift_in(&mut menu, &player, map);
        assert!(input.lock().get_item(0).is(&vanilla_items::FILLED_MAP));
        shift_in(
            &mut menu,
            &player,
            ItemStack::with_count(&vanilla_items::PAPER, 2),
        );
        assert_eq!(input.lock().get_item(1).count(), 2);
        assert!(result.lock().get_item(0).has(MAP_POST_PROCESSING));

        menu.clicked(
            Click::Pickup {
                slot: RESULT_SLOT,
                button: MouseButton::Left,
            },
            &player,
        );
        let zoomed = menu.behavior().carried().clone();
        assert!(!zoomed.has(MAP_POST_PROCESSING));
        let zoomed_id = *zoomed.get(MAP_ID).expect("map id");
        assert_ne!(zoomed_id, source_id);
        assert_eq!(
            world.map_data(zoomed_id).expect("zoomed map").lock().scale,
            1
        );
        // The source map was used up; one paper is left and nothing is offered.
        assert!(input.lock().get_item(0).is_empty());
        assert_eq!(input.lock().get_item(1).count(), 1);
        assert!(result.lock().get_item(0).is_empty());

        // Put the zoomed map back, swap the paper for glass and shift-click the result.
        menu.clicked(
            Click::Pickup {
                slot: 0,
                button: MouseButton::Left,
            },
            &player,
        );
        menu.clicked(Click::QuickMove { slot: 1 }, &player);
        shift_in(
            &mut menu,
            &player,
            ItemStack::new(&vanilla_items::GLASS_PANE),
        );
        assert!(result.lock().get_item(0).has(MAP_POST_PROCESSING));
        menu.clicked(Click::QuickMove { slot: RESULT_SLOT }, &player);

        let locked = {
            let inventory = player.inventory.lock();
            (0..36)
                .map(|slot| inventory.get_item(slot).clone())
                .find(|stack| stack.is(&vanilla_items::FILLED_MAP))
                .expect("the locked map lands in the inventory")
        };
        assert!(!locked.has(MAP_POST_PROCESSING));
        let locked_id = *locked.get(MAP_ID).expect("map id");
        assert_ne!(locked_id, zoomed_id);
        let data = world.map_data(locked_id).expect("locked map");
        let data = data.lock();
        assert!(data.locked);
        assert_eq!(data.scale, 1);
    }

    #[test]
    fn an_empty_map_copies_and_locked_maps_cannot_change() {
        let (world_fixture, player, mut menu) = test_table("cartography_copy");
        let world = &world_fixture.world;
        let (input, result) = parts(&menu);
        let map = create_map(world, 0, 0, 0, true, false);
        let id = *map.get(MAP_ID).expect("map id");
        world.map_data(id).expect("map").lock().locked = true;

        shift_in(&mut menu, &player, map);
        shift_in(&mut menu, &player, ItemStack::new(&vanilla_items::PAPER));
        assert!(result.lock().get_item(0).is_empty());

        menu.clicked(Click::QuickMove { slot: 1 }, &player);
        assert!(input.lock().get_item(1).is_empty());
        shift_in(&mut menu, &player, ItemStack::new(&vanilla_items::MAP));
        let copies = result.lock().get_item(0).clone();
        assert_eq!(copies.count(), 2);
        assert_eq!(copies.get(MAP_ID), Some(&id));
        assert!(!copies.has(MAP_POST_PROCESSING));
    }
}
