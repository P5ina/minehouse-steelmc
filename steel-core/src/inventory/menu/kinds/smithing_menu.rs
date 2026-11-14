//! Smithing table menu.
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
        prelude::*,
        recipe_access::{RECIPE_ACCESS, SMITHING_ADDITION, SMITHING_BASE, SMITHING_TEMPLATE},
        slots::{ADDITIONAL_SLOT, BASE_SLOT, SmithingResultHandler, TEMPLATE_SLOT},
    },
    player::player_inventory::PlayerInventory,
    world::World,
};

/// Vanilla `SmithingMenu.createInputSlotDefinitions`: each input slot accepts
/// only items of its recipe property set.
fn may_place_input(index: usize, stack: &ItemStack) -> bool {
    let key = match index {
        TEMPLATE_SLOT => &SMITHING_TEMPLATE,
        BASE_SLOT => &SMITHING_BASE,
        ADDITIONAL_SLOT => &SMITHING_ADDITION,
        _ => return false,
    };
    RECIPE_ACCESS.property_set(key).test(stack)
}

/// Builds the smithing table menu.
#[must_use]
pub fn smithing(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    pos: BlockPos,
    world: &Arc<World>,
) -> Menu {
    let input_container = SimpleContainer::new(3).into_shared();
    let result_container = ResultContainer::new().into_shared();
    let handler = SmithingResultHandler::new(
        Arc::clone(&input_container),
        Arc::clone(&result_container),
        pos,
        Arc::clone(world),
    );

    let mut builder = MenuBuilder::new(&vanilla_menu_types::SMITHING, container_id);

    let input =
        builder.section_all_with(&input_container, SectionKind::restricted(may_place_input));
    let result = builder.result_slot(handler.clone());
    let player = builder.player_inventory(&inventory);
    let has_recipe_error = builder.data_slot(0);

    builder.route_with_remainder_policy(
        result,
        player.all(),
        FillDirection::Backward,
        FakeResultRemainderPolicy::Discard,
    );
    builder.route(input, player.all(), FillDirection::Forward);
    builder.drain(input);

    builder.build(SmithingKind {
        input_container,
        result_container,
        handler,
        block_pos: pos,
        world: Arc::clone(world),
        input,
        result,
        player,
        has_recipe_error,
    })
}

/// Per-menu smithing state: the three inputs, the virtual result and the
/// recipe error flag.
pub struct SmithingKind {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    handler: SmithingResultHandler,
    block_pos: BlockPos,
    world: Arc<World>,
    input: Section,
    result: Section,
    player: PlayerInventorySections,
    /// Vanilla `SmithingMenu.hasRecipeError` data slot.
    has_recipe_error: DataSlot,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for SmithingKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/smithing");
}

impl SmithingKind {
    /// Vanilla `SmithingMenu.canMoveIntoInputSlots`.
    fn can_move_into_input_slots(&self, guard: &ContainerLockGuard, stack: &ItemStack) -> bool {
        let Some(inputs) = guard.get(ContainerId::from_arc(&self.input_container)) else {
            return false;
        };
        [TEMPLATE_SLOT, BASE_SLOT, ADDITIONAL_SLOT]
            .into_iter()
            .any(|slot| may_place_input(slot, stack) && inputs.get_item(slot).is_empty())
    }
}

impl MenuKind for SmithingKind {
    /// Vanilla `ItemCombinerMenu.stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        still_valid_block(
            &self.world,
            self.block_pos,
            &vanilla_blocks::SMITHING_TABLE,
            player,
        )
    }

    /// Vanilla `SmithingMenu.slotsChanged`: recomputes the result, then flags a
    /// full set of inputs that makes nothing.
    fn slots_changed(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.handler.update_result(guard);
        let Some(inputs) = guard.get(ContainerId::from_arc(&self.input_container)) else {
            return;
        };
        let inputs_filled = [TEMPLATE_SLOT, BASE_SLOT, ADDITIONAL_SLOT]
            .into_iter()
            .all(|slot| !inputs.get_item(slot).is_empty());
        let result_empty = guard
            .get(ContainerId::from_arc(&self.result_container))
            .is_none_or(|result| result.get_item(0).is_empty());
        self.has_recipe_error
            .set(behavior, i16::from(inputs_filled && result_empty));
    }

    /// Clears the virtual result on close. Inputs are drained by [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.result_container.lock().set_item(0, ItemStack::empty());
    }

    /// Vanilla `SmithingMenu.canTakeItemForPickAll`.
    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        !self.result.contains(slot_index)
    }

    /// Vanilla `ItemCombinerMenu.quickMoveStack` for the player's slots; the
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

        let destination = if self.can_move_into_input_slots(guard, &clicked) {
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
        if let Some(remainder) = behavior.slots()[slot_index].on_take(guard, &remaining, player) {
            player.add_item_or_drop_with_guard(guard, remainder, Prediction::Predicted);
        }
        Some(clicked)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::{
        data_components::vanilla_components::TRIM, init_vanilla_registry, vanilla_blocks,
        vanilla_items,
    };
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, types::UpdateFlags};

    use super::{SmithingKind, smithing};
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

    fn test_smithing(key: &'static str) -> (TestWorld, Arc<Player>, Menu) {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world(key);
        let world = &world_fixture.world;
        let pos = BlockPos::new(0, 64, 0);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::SMITHING_TABLE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "SmithingTester", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));
        let menu = smithing(Arc::clone(&player.inventory), 1, pos, world);
        (world_fixture, player, menu)
    }

    fn containers(menu: &Menu) -> (Shared<SimpleContainer>, Shared<ResultContainer>, DataSlot) {
        let Some(kind) = menu.kind().downcast_ref::<SmithingKind>() else {
            panic!("smithing builder should create a smithing menu");
        };
        (
            Arc::clone(&kind.input_container),
            Arc::clone(&kind.result_container),
            kind.has_recipe_error,
        )
    }

    #[test]
    fn netherite_upgrade_keeps_components_and_consumes_one_of_each_input() {
        let (_world, player, mut menu) = test_smithing("smithing_menu_netherite_upgrade");
        let (input, result, has_recipe_error) = containers(&menu);
        let mut sword = ItemStack::new(&vanilla_items::DIAMOND_SWORD);
        sword.set_damage_value(42);
        {
            let mut inventory = player.inventory.lock();
            inventory.set_item(
                9,
                ItemStack::new(&vanilla_items::NETHERITE_UPGRADE_SMITHING_TEMPLATE),
            );
            inventory.set_item(10, sword);
            inventory.set_item(
                11,
                ItemStack::with_count(&vanilla_items::NETHERITE_INGOT, 2),
            );
        }

        // Each input lands in the one slot whose property set accepts it.
        for slot in FIRST_MAIN_SLOT..FIRST_MAIN_SLOT + 3 {
            menu.clicked(Click::QuickMove { slot }, &player);
        }
        {
            let input = input.lock();
            assert!(
                input
                    .get_item(0)
                    .is(&vanilla_items::NETHERITE_UPGRADE_SMITHING_TEMPLATE)
            );
            assert!(input.get_item(1).is(&vanilla_items::DIAMOND_SWORD));
            assert_eq!(input.get_item(2).count(), 2);
        }
        let upgraded = result.lock().get_item(0).clone();
        assert!(upgraded.is(&vanilla_items::NETHERITE_SWORD));
        assert_eq!(upgraded.get_damage_value(), 42);
        assert_eq!(has_recipe_error.get(menu.behavior()), 0);

        menu.clicked(Click::QuickMove { slot: RESULT_SLOT }, &player);

        let input = input.lock();
        assert!(input.get_item(0).is_empty());
        assert!(input.get_item(1).is_empty());
        assert_eq!(input.get_item(2).count(), 1);
        assert!(result.lock().get_item(0).is_empty());
        let inventory = player.inventory.lock();
        assert!((0..36).any(|slot| inventory.get_item(slot).is(&vanilla_items::NETHERITE_SWORD)));
    }

    #[test]
    fn repeating_the_same_trim_is_a_recipe_error() {
        let (_world, player, mut menu) = test_smithing("smithing_menu_trim");
        let (input, result, has_recipe_error) = containers(&menu);
        {
            let mut input = input.lock();
            input.set_item(
                0,
                ItemStack::new(&vanilla_items::COAST_ARMOR_TRIM_SMITHING_TEMPLATE),
            );
            input.set_item(1, ItemStack::new(&vanilla_items::IRON_CHESTPLATE));
            input.set_item(2, ItemStack::new(&vanilla_items::GOLD_INGOT));
        }
        menu.clicked(
            Click::Pickup {
                slot: FIRST_MAIN_SLOT,
                button: MouseButton::Left,
            },
            &player,
        );
        let trimmed = result.lock().get_item(0).clone();
        assert!(trimmed.is(&vanilla_items::IRON_CHESTPLATE));
        assert!(trimmed.get(TRIM).is_some());
        assert_eq!(has_recipe_error.get(menu.behavior()), 0);

        input.lock().set_item(1, trimmed);
        menu.clicked(
            Click::Pickup {
                slot: FIRST_MAIN_SLOT,
                button: MouseButton::Left,
            },
            &player,
        );
        assert!(result.lock().get_item(0).is_empty());
        assert_eq!(has_recipe_error.get(menu.behavior()), 1);
    }

    #[test]
    fn filled_template_slot_sends_templates_to_the_hotbar() {
        let (_world, player, mut menu) = test_smithing("smithing_menu_filled_template");
        let (input, _result, _) = containers(&menu);
        input.lock().set_item(
            0,
            ItemStack::new(&vanilla_items::NETHERITE_UPGRADE_SMITHING_TEMPLATE),
        );
        player.inventory.lock().set_item(
            9,
            ItemStack::new(&vanilla_items::NETHERITE_UPGRADE_SMITHING_TEMPLATE),
        );

        menu.clicked(
            Click::QuickMove {
                slot: FIRST_MAIN_SLOT,
            },
            &player,
        );

        assert_eq!(input.lock().get_item(0).count(), 1);
        let inventory = player.inventory.lock();
        assert!(inventory.get_item(9).is_empty());
        assert!(
            inventory
                .get_item(0)
                .is(&vanilla_items::NETHERITE_UPGRADE_SMITHING_TEMPLATE)
        );
    }
}
