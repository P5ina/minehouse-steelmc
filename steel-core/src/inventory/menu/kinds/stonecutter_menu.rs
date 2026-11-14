//! Stonecutter menu.
use crate::player::player_inventory::Prediction;
use std::sync::Arc;

use steel_registry::{vanilla_blocks, vanilla_menu_types};
use steel_utils::{
    BlockPos,
    locks::{IntoShared, Shared, SyncMutex},
};

use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        prelude::*,
        recipe_access::{RECIPE_ACCESS, StonecutterRecipeSet},
        slots::{StonecutterResultHandler, StonecutterState},
    },
    player::player_inventory::PlayerInventory,
    world::World,
};

/// Vanilla `StonecutterMenu.INPUT_SLOT`.
const INPUT_SLOT: usize = 0;

/// Builds the stonecutter menu.
#[must_use]
pub fn stonecutter(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    pos: BlockPos,
    world: &Arc<World>,
) -> Menu {
    let input_container = SimpleContainer::new(1).into_shared();
    let result_container = ResultContainer::new().into_shared();
    let state = Arc::new(SyncMutex::new(StonecutterState::default()));

    let mut builder = MenuBuilder::new(&vanilla_menu_types::STONECUTTER, container_id);

    let input = builder.section_all(&input_container);
    let result = builder.result_slot(StonecutterResultHandler::new(
        Arc::clone(&input_container),
        Arc::clone(&result_container),
        Arc::clone(&state),
        pos,
        Arc::clone(world),
    ));
    let player = builder.player_inventory(&inventory);
    let selected_recipe_index = builder.data_slot(0);

    builder.route_with_remainder_policy(
        result,
        player.all(),
        FillDirection::Backward,
        FakeResultRemainderPolicy::Drop,
    );
    builder.route(input, player.all(), FillDirection::Forward);
    builder.drain(input);

    builder.build(StonecutterKind {
        input_container,
        result_container,
        state,
        block_pos: pos,
        world: Arc::clone(world),
        input,
        result,
        player,
        selected_recipe_index,
    })
}

/// Per-menu stonecutter state: the input, the virtual result and the selected
/// recipe.
pub struct StonecutterKind {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    state: Arc<SyncMutex<StonecutterState>>,
    block_pos: BlockPos,
    world: Arc<World>,
    input: Section,
    result: Section,
    player: PlayerInventorySections,
    /// Vanilla `StonecutterMenu.selectedRecipeIndex` data slot.
    selected_recipe_index: DataSlot,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for StonecutterKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/stonecutter");
}

impl StonecutterKind {
    /// Sets the selected recipe on the data slot and the shared state.
    fn set_selected_recipe_index(&self, behavior: &mut MenuBehavior, index: i32) {
        self.state.lock().selected_recipe_index = index;
        // Recipe indices stay far below `i16::MAX`; vanilla sends the low 16 bits.
        self.selected_recipe_index
            .set(behavior, i16::try_from(index).unwrap_or(-1));
    }

    fn set_result(&self, guard: &mut ContainerLockGuard, result: ItemStack) {
        if let Some(container) = guard.get_mut(ContainerId::from_arc(&self.result_container)) {
            container.set_item(0, result);
        }
    }

    /// Vanilla `StonecutterMenu.setupRecipeList`.
    fn setup_recipe_list(
        &self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        item: &ItemStack,
    ) {
        self.set_selected_recipe_index(behavior, -1);
        self.set_result(guard, ItemStack::empty());
        self.state.lock().recipes_for_input = if item.is_empty() {
            StonecutterRecipeSet::empty()
        } else {
            RECIPE_ACCESS.stonecutter_recipes().select_by_input(item)
        };
    }

    /// Vanilla `StonecutterMenu.setupResultSlot`.
    fn setup_result_slot(&self, guard: &mut ContainerLockGuard, index: i32) {
        let result = self.state.lock().result_for(index);
        self.set_result(guard, result);
    }
}

impl MenuKind for StonecutterKind {
    /// Vanilla `StonecutterMenu.stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        still_valid_block(
            &self.world,
            self.block_pos,
            &vanilla_blocks::STONECUTTER,
            player,
        )
    }

    /// Vanilla `StonecutterMenu.slotsChanged`: rebuilds the recipe list when the
    /// input's item type changes.
    fn slots_changed(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        let Some(input) = guard
            .get(ContainerId::from_arc(&self.input_container))
            .map(|container| container.get_item(INPUT_SLOT).clone())
        else {
            return;
        };
        {
            let mut state = self.state.lock();
            if input.is(state.input.item) {
                return;
            }
            state.input = input.clone();
        }
        self.setup_recipe_list(behavior, guard, &input);
    }

    /// Vanilla `StonecutterMenu.clickMenuButton`.
    fn click_menu_button(
        &mut self,
        behavior: &mut MenuBehavior,
        button_id: i32,
        _player: &Player,
    ) -> bool {
        let (selected, valid) = {
            let state = self.state.lock();
            (
                state.selected_recipe_index,
                state.is_valid_recipe_index(button_id),
            )
        };
        if selected == button_id {
            return false;
        }
        if valid {
            self.set_selected_recipe_index(behavior, button_id);
            let mut guard = behavior.lock_all_containers();
            self.setup_result_slot(&mut guard, button_id);
        }
        true
    }

    /// Vanilla `StonecutterMenu.removed` clears the result; the input is drained
    /// by [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    /// Vanilla `StonecutterMenu.canTakeItemForPickAll`.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        !self.result.contains(slot_index)
    }

    /// Vanilla `StonecutterMenu.quickMoveStack` for the player's slots; the
    /// result and input use the route table.
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

        let destination = if RECIPE_ACCESS.stonecutter_recipes().accepts_input(&clicked) {
            self.input
        } else if self.player.main().contains(slot_index) {
            self.player.hotbar()
        } else {
            self.player.main()
        };

        let mut remaining = clicked.clone();
        let moved = behavior.move_item_stack_to(
            guard,
            slot_index,
            &mut remaining,
            destination.start(),
            destination.end(),
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
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, types::UpdateFlags};

    use super::{StonecutterKind, stonecutter};
    use crate::{
        behavior::init_behaviors,
        entity::Entity as _,
        inventory::{
            click::Click,
            container::{Container as _, ResultContainer},
            menu::Menu,
            prelude::*,
            recipe_access::RECIPE_ACCESS,
        },
        test_support::{TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk},
    };

    /// Menu slot of the player's first main inventory slot (inventory slot 9).
    const FIRST_MAIN_SLOT: usize = 2;
    const RESULT_SLOT: usize = 1;

    fn test_stonecutter(key: &'static str) -> (TestWorld, Arc<Player>, Menu) {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world(key);
        let world = &world_fixture.world;
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::STONECUTTER.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "CutterTester", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
        let menu = stonecutter(Arc::clone(&player.inventory), 1, pos, world);
        (world_fixture, player, menu)
    }

    fn slab_button() -> i32 {
        let recipes = RECIPE_ACCESS
            .stonecutter_recipes()
            .select_by_input(&ItemStack::new(&vanilla_items::STONE));
        let index = (0..recipes.len())
            .find(|&index| {
                recipes.get(index).is_some_and(|recipe| {
                    recipe.data().result.item() == &*vanilla_items::STONE_SLAB
                })
            })
            .expect("stone should cut into slabs");
        i32::try_from(index).expect("recipe index fits a button id")
    }

    fn parts(menu: &Menu) -> (Shared<SimpleContainer>, Shared<ResultContainer>, DataSlot) {
        let Some(kind) = menu.kind().downcast_ref::<StonecutterKind>() else {
            panic!("stonecutter builder should create a stonecutter menu");
        };
        (
            Arc::clone(&kind.input_container),
            Arc::clone(&kind.result_container),
            kind.selected_recipe_index,
        )
    }

    #[test]
    fn selecting_a_recipe_shows_its_result_and_new_input_resets_it() {
        let (_world, player, mut menu) = test_stonecutter("stonecutter_menu_select");
        let (input, result, selected) = parts(&menu);
        player
            .inventory
            .lock()
            .set_item(9, ItemStack::with_count(&vanilla_items::STONE, 3));

        menu.clicked(
            Click::QuickMove {
                slot: FIRST_MAIN_SLOT,
            },
            &player,
        );
        assert_eq!(input.lock().get_item(0).count(), 3);
        assert_eq!(selected.get(menu.behavior()), -1);
        assert!(result.lock().get_item(0).is_empty());

        let button = slab_button();
        assert!(menu.click_menu_button(button, &player));
        assert!(!menu.click_menu_button(button, &player));
        assert_eq!(i32::from(selected.get(menu.behavior())), button);
        let slabs = result.lock().get_item(0).clone();
        assert!(slabs.is(&vanilla_items::STONE_SLAB));
        assert_eq!(slabs.count(), 2);

        // Taking one result consumes one input and keeps the selection.
        menu.clicked(
            Click::Pickup {
                slot: RESULT_SLOT,
                button: MouseButton::Left,
            },
            &player,
        );
        assert_eq!(menu.behavior().carried().count(), 2);
        assert_eq!(input.lock().get_item(0).count(), 2);
        assert!(result.lock().get_item(0).is(&vanilla_items::STONE_SLAB));
        assert_eq!(i32::from(selected.get(menu.behavior())), button);
    }

    #[test]
    fn shift_clicking_the_result_cuts_every_input() {
        let (_world, player, mut menu) = test_stonecutter("stonecutter_menu_quick_craft");
        let (input, result, selected) = parts(&menu);
        input
            .lock()
            .set_item(0, ItemStack::with_count(&vanilla_items::STONE, 3));
        menu.clicked(
            Click::Pickup {
                slot: FIRST_MAIN_SLOT,
                button: MouseButton::Left,
            },
            &player,
        );
        assert!(menu.click_menu_button(slab_button(), &player));

        menu.clicked(Click::QuickMove { slot: RESULT_SLOT }, &player);

        assert!(input.lock().get_item(0).is_empty());
        assert!(result.lock().get_item(0).is_empty());
        assert_eq!(selected.get(menu.behavior()), -1);
        let inventory = player.inventory.lock();
        let slabs: i32 = (0..36)
            .map(|slot| inventory.get_item(slot))
            .filter(|stack| stack.is(&vanilla_items::STONE_SLAB))
            .map(ItemStack::count)
            .sum();
        assert_eq!(slabs, 6);
    }

    #[test]
    fn items_without_stonecutter_recipes_skip_the_input() {
        let (_world, player, mut menu) = test_stonecutter("stonecutter_menu_no_recipe");
        let (input, _result, _) = parts(&menu);
        player
            .inventory
            .lock()
            .set_item(9, ItemStack::with_count(&vanilla_items::DIRT, 5));

        menu.clicked(
            Click::QuickMove {
                slot: FIRST_MAIN_SLOT,
            },
            &player,
        );

        assert!(input.lock().get_item(0).is_empty());
        assert_eq!(player.inventory.lock().get_item(0).count(), 5);
    }
}
