//! Loom menu.
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
        menu::builder::SectionKind,
        prelude::*,
        slots::{
            LOOM_BANNER_SLOT, LOOM_DYE_SLOT, LOOM_PATTERN_SLOT, LoomResultHandler, LoomState,
            is_banner_item, is_dye_item, is_pattern_item, loom_result,
        },
    },
    player::player_inventory::PlayerInventory,
    world::World,
};

/// Builds Vanilla's `LoomMenu`.
#[must_use]
pub fn loom(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    pos: BlockPos,
    world: &Arc<World>,
) -> Menu {
    let input_container = SimpleContainer::new(3).into_shared();
    let result_container = ResultContainer::new().into_shared();
    let state = Arc::new(SyncMutex::new(LoomState::default()));

    let mut builder = MenuBuilder::new(&vanilla_menu_types::LOOM, container_id);

    let inputs = builder.section_all_with(
        &input_container,
        SectionKind::restricted(|index, stack| match index {
            LOOM_BANNER_SLOT => is_banner_item(stack),
            LOOM_DYE_SLOT => is_dye_item(stack),
            _ => is_pattern_item(stack),
        }),
    );
    let result = builder.result_slot(LoomResultHandler::new(
        Arc::clone(&input_container),
        Arc::clone(&result_container),
        Arc::clone(&state),
        pos,
        Arc::clone(world),
    ));
    let player = builder.player_inventory(&inventory);
    let selected_pattern_index = builder.data_slot(0);

    builder.route_with_remainder_policy(
        result,
        player.all(),
        FillDirection::Backward,
        FakeResultRemainderPolicy::Drop,
    );
    builder.route(inputs, player.all(), FillDirection::Forward);
    builder.drain(inputs);

    builder.build(LoomKind {
        input_container,
        result_container,
        state,
        block_pos: pos,
        world: Arc::clone(world),
        inputs,
        result,
        player,
        selected_pattern_index,
    })
}

/// Per-menu loom state: the inputs, the virtual result and the selected pattern.
pub struct LoomKind {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    state: Arc<SyncMutex<LoomState>>,
    block_pos: BlockPos,
    world: Arc<World>,
    inputs: Section,
    result: Section,
    player: PlayerInventorySections,
    /// Vanilla `LoomMenu.selectedBannerPatternIndex` data slot.
    selected_pattern_index: DataSlot,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for LoomKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/loom");
}

impl LoomKind {
    fn sync_selected_index(&self, behavior: &mut MenuBehavior) {
        let index = self.state.lock().selected_pattern_index;
        // Pattern indices stay far below `i16::MAX`; vanilla sends the low 16 bits.
        self.selected_pattern_index
            .set(behavior, i16::try_from(index).unwrap_or(-1));
    }

    fn inputs(&self, guard: &ContainerLockGuard) -> Option<[ItemStack; 3]> {
        guard
            .get(ContainerId::from_arc(&self.input_container))
            .map(|input| {
                [LOOM_BANNER_SLOT, LOOM_DYE_SLOT, LOOM_PATTERN_SLOT]
                    .map(|slot| input.get_item(slot).clone())
            })
    }

    fn set_result(&self, guard: &mut ContainerLockGuard, result: ItemStack) {
        if let Some(container) = guard.get_mut(ContainerId::from_arc(&self.result_container)) {
            container.set_item(0, result);
        }
    }

    fn current_result(&self, guard: &ContainerLockGuard) -> ItemStack {
        guard
            .get(ContainerId::from_arc(&self.result_container))
            .map_or_else(ItemStack::empty, |container| container.get_item(0).clone())
    }
}

impl MenuKind for LoomKind {
    /// Vanilla `LoomMenu.stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        still_valid_block(&self.world, self.block_pos, &vanilla_blocks::LOOM, player)
    }

    /// Vanilla `LoomMenu.slotsChanged`.
    fn slots_changed(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        let Some([banner, dye, pattern]) = self.inputs(guard) else {
            return;
        };
        let current = self.current_result(guard);
        let result = self
            .state
            .lock()
            .slots_changed(&banner, &dye, &pattern, &current);
        self.set_result(guard, result);
        self.sync_selected_index(behavior);
    }

    /// Vanilla `LoomMenu.clickMenuButton`.
    fn click_menu_button(
        &mut self,
        behavior: &mut MenuBehavior,
        button_id: i32,
        _player: &Player,
    ) -> bool {
        let Some(pattern) = ({
            let mut state = self.state.lock();
            let pattern = state.pattern_at(button_id);
            if pattern.is_some() {
                state.selected_pattern_index = button_id;
            }
            pattern
        }) else {
            return false;
        };
        self.sync_selected_index(behavior);
        let mut guard = behavior.lock_all_containers();
        if let Some([banner, dye, _]) = self.inputs(&guard) {
            let result = loom_result(&banner, &dye, pattern);
            if !ItemStack::matches(&result, &self.current_result(&guard)) {
                self.set_result(&mut guard, result);
            }
        }
        true
    }

    /// The output container is discarded with the menu; the inputs are drained
    /// by [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    /// Vanilla `LoomMenu.quickMoveStack` for the player's slots; the result and
    /// inputs use the route table.
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
        let (start, end) = if is_banner_item(&clicked) {
            (
                input_slot(LOOM_BANNER_SLOT),
                input_slot(LOOM_BANNER_SLOT) + 1,
            )
        } else if is_dye_item(&clicked) {
            (input_slot(LOOM_DYE_SLOT), input_slot(LOOM_DYE_SLOT) + 1)
        } else if is_pattern_item(&clicked) {
            (
                input_slot(LOOM_PATTERN_SLOT),
                input_slot(LOOM_PATTERN_SLOT) + 1,
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
        if let Some(remainder) = behavior.slots()[slot_index].on_take(guard, &remaining, player) {
            player.add_item_or_drop_with_guard(guard, remainder, Prediction::Predicted);
        }
        Some(clicked)
    }

    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        !self.result.contains(slot_index)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::data_components::vanilla_components::BANNER_PATTERNS;
    use steel_registry::{
        DyeColor, init_vanilla_registry, vanilla_banner_patterns, vanilla_blocks, vanilla_items,
    };
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, types::UpdateFlags};

    use super::{LoomKind, loom};
    use crate::{
        behavior::init_behaviors,
        entity::Entity as _,
        inventory::{
            click::Click,
            container::{Container as _, ResultContainer},
            menu::Menu,
            prelude::*,
        },
        test_support::{TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk},
    };

    /// Menu slot of the player's first main inventory slot (inventory slot 9).
    const FIRST_MAIN_SLOT: usize = 4;
    const RESULT_SLOT: usize = 3;

    fn test_loom(key: &'static str) -> (TestWorld, Arc<Player>, Menu) {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world(key);
        let world = &world_fixture.world;
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::LOOM.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "LoomTester", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
        let menu = loom(Arc::clone(&player.inventory), 1, pos, world);
        (world_fixture, player, menu)
    }

    fn parts(menu: &Menu) -> (Shared<SimpleContainer>, Shared<ResultContainer>, DataSlot) {
        let Some(kind) = menu.kind().downcast_ref::<LoomKind>() else {
            panic!("loom builder should create a loom menu");
        };
        (
            Arc::clone(&kind.input_container),
            Arc::clone(&kind.result_container),
            kind.selected_pattern_index,
        )
    }

    fn quick_move_from_inventory(menu: &mut Menu, player: &Player, stack: ItemStack) {
        player.inventory.lock().set_item(9, stack);
        menu.clicked(
            Click::QuickMove {
                slot: FIRST_MAIN_SLOT,
            },
            player,
        );
    }

    #[test]
    fn slots_accept_banners_dyes_and_patterns() {
        let (_world, _player, menu) = test_loom("loom_menu_slots");
        let slots = menu.behavior().slots();
        let banner = ItemStack::new(&vanilla_items::WHITE_BANNER);
        let dye = ItemStack::new(&vanilla_items::RED_DYE);
        let pattern = ItemStack::new(&vanilla_items::FLOWER_BANNER_PATTERN);
        assert!(slots[0].may_place(&banner));
        assert!(!slots[0].may_place(&dye));
        assert!(slots[1].may_place(&dye));
        assert!(!slots[1].may_place(&banner));
        assert!(slots[2].may_place(&pattern));
        assert!(!slots[2].may_place(&banner));
    }

    #[test]
    fn selecting_a_pattern_weaves_it_onto_the_banner() {
        let (_world, player, mut menu) = test_loom("loom_menu_select");
        let (input, result, selected) = parts(&menu);
        quick_move_from_inventory(
            &mut menu,
            &player,
            ItemStack::with_count(&vanilla_items::WHITE_BANNER, 2),
        );
        quick_move_from_inventory(
            &mut menu,
            &player,
            ItemStack::with_count(&vanilla_items::RED_DYE, 2),
        );
        assert_eq!(input.lock().get_item(0).count(), 2);
        assert_eq!(input.lock().get_item(1).count(), 2);
        assert_eq!(selected.get(menu.behavior()), -1);
        assert!(result.lock().get_item(0).is_empty());

        assert!(menu.click_menu_button(0, &player));
        assert_eq!(selected.get(menu.behavior()), 0);
        let woven = result.lock().get_item(0).clone();
        assert!(woven.is(&vanilla_items::WHITE_BANNER));
        assert_eq!(woven.count(), 1);
        let layers = woven
            .get(BANNER_PATTERNS)
            .expect("the woven banner should have patterns");
        assert_eq!(layers.layers().len(), 1);
        assert_eq!(layers.layers()[0].color(), DyeColor::Red);
        assert!(!menu.click_menu_button(10_000, &player));

        // Shift-clicking the result weaves every banner while dye remains.
        menu.clicked(Click::QuickMove { slot: RESULT_SLOT }, &player);
        assert!(input.lock().get_item(0).is_empty());
        assert!(input.lock().get_item(1).is_empty());
        assert!(result.lock().get_item(0).is_empty());
        assert_eq!(selected.get(menu.behavior()), -1);
        let woven_count: i32 = (0..36)
            .map(|slot| player.inventory.lock().get_item(slot).clone())
            .filter(|stack| stack.is(&vanilla_items::WHITE_BANNER) && stack.has(BANNER_PATTERNS))
            .map(|stack| stack.count())
            .sum();
        assert_eq!(woven_count, 2);
    }

    #[test]
    fn a_single_pattern_item_selects_its_pattern() {
        let (_world, player, mut menu) = test_loom("loom_menu_pattern_item");
        let (input, result, selected) = parts(&menu);
        input
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::WHITE_BANNER));
        input
            .lock()
            .set_item(1, ItemStack::new(&vanilla_items::BLUE_DYE));
        quick_move_from_inventory(
            &mut menu,
            &player,
            ItemStack::new(&vanilla_items::FLOWER_BANNER_PATTERN),
        );
        assert!(
            input
                .lock()
                .get_item(2)
                .is(&vanilla_items::FLOWER_BANNER_PATTERN)
        );
        assert_eq!(selected.get(menu.behavior()), 0);
        let woven = result.lock().get_item(0).clone();
        let layers = woven
            .get(BANNER_PATTERNS)
            .expect("the woven banner should have patterns");
        assert_eq!(
            layers.layers()[0].pattern(),
            &steel_registry::RegistryHolder::Reference(&vanilla_banner_patterns::FLOWER)
        );
    }
}
