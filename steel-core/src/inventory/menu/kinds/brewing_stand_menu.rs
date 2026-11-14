//! Brewing stand menu.

use std::array::from_fn;

use steel_registry::data_components::vanilla_components::BREWING_FUEL;
use steel_registry::vanilla_menu_types;
use steel_utils::{DowncastType, DowncastTypeKey, locks::Shared};

use crate::block_entity::entities::{
    BREWING_FUEL_SLOT, BREWING_INGREDIENT_SLOT, BrewingStandContainer,
};
use crate::inventory::menu::builder::SectionKind;
use crate::inventory::prelude::*;
use crate::inventory::recipe_access::{BREWING_REAGENTS, RECIPE_ACCESS, is_potion_input};
use crate::inventory::slots::PotionSlot;
use crate::player::player_inventory::{PlayerInventory, Prediction};

/// Vanilla `BrewingStandMenu.FuelSlot.mayPlaceItem`.
fn may_place_fuel(stack: &ItemStack) -> bool {
    stack.has(BREWING_FUEL)
}

/// Vanilla `BrewingStandMenu.IngredientsSlot.mayPlace`.
fn may_place_ingredient(stack: &ItemStack) -> bool {
    RECIPE_ACCESS.property_set(&BREWING_REAGENTS).test(stack)
}

/// Vanilla `moveItemStackTo` over one section.
fn move_into(
    behavior: &MenuBehavior,
    guard: &mut ContainerLockGuard,
    slot_index: usize,
    remaining: &mut ItemStack,
    section: Section,
    direction: FillDirection,
) -> bool {
    behavior.move_item_stack_to(
        guard,
        slot_index,
        remaining,
        section.start(),
        section.end(),
        direction,
    )
}

/// Builds Vanilla's `BrewingStandMenu` over a brewing stand container.
#[must_use]
pub fn brewing_stand(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: ContainerRef,
) -> Menu {
    let mut builder = MenuBuilder::new(&vanilla_menu_types::BREWING_STAND, container_id);

    let bottles = builder.section_at(
        container.clone(),
        [0, 1, 2],
        SectionKind::custom(|container, index| Box::new(PotionSlot::new(container.clone(), index))),
    );
    let ingredient = builder.section_at(
        container.clone(),
        [BREWING_INGREDIENT_SLOT],
        SectionKind::restricted(|_, stack| may_place_ingredient(stack)),
    );
    let fuel = builder.section_at(
        container.clone(),
        [BREWING_FUEL_SLOT],
        SectionKind::restricted(|_, stack| may_place_fuel(stack)),
    );
    let data = from_fn(|_| builder.data_slot(0));
    let player = builder.player_inventory(&inventory);

    builder.build(BrewingStandKind {
        container,
        bottles,
        ingredient,
        fuel,
        player,
        data,
    })
}

/// State and shift-click behavior of Vanilla's `BrewingStandMenu`.
pub struct BrewingStandKind {
    container: ContainerRef,
    bottles: Section,
    ingredient: Section,
    fuel: Section,
    player: PlayerInventorySections,
    data: [DataSlot; 4],
}

// SAFETY: This Steel-owned key uniquely identifies brewing stand menu behavior.
unsafe impl DowncastType for BrewingStandKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/brewing_stand");
}

impl BrewingStandKind {
    fn update_data(&self, behavior: &mut MenuBehavior, guard: &ContainerLockGuard) {
        let Some(container) =
            guard.get_typed::<BrewingStandContainer>(self.container.container_id())
        else {
            return;
        };
        for (slot, value) in self.data.into_iter().zip(container.data()) {
            slot.set(behavior, value);
        }
    }

    /// Vanilla `quickMoveStack` for a player inventory slot. Returns whether
    /// the quick move goes on to update the source slot.
    fn move_from_player(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        clicked: &ItemStack,
        remaining: &mut ItemStack,
    ) -> bool {
        if may_place_fuel(clicked) {
            // Vanilla returns early once the fuel slot took anything, or when
            // the stack fits the ingredient slot but the ingredient slot is full.
            if move_into(
                behavior,
                guard,
                slot_index,
                remaining,
                self.fuel,
                FillDirection::Forward,
            ) || (may_place_ingredient(remaining)
                && !move_into(
                    behavior,
                    guard,
                    slot_index,
                    remaining,
                    self.ingredient,
                    FillDirection::Forward,
                ))
            {
                behavior.update_quick_move_source(guard, slot_index, remaining, clicked);
                return false;
            }
            return true;
        }
        let target = if may_place_ingredient(clicked) {
            self.ingredient
        } else if is_potion_input(clicked) {
            self.bottles
        } else if self.player.main().contains(slot_index) {
            self.player.hotbar()
        } else if self.player.hotbar().contains(slot_index) {
            self.player.main()
        } else {
            self.player.all()
        };
        move_into(
            behavior,
            guard,
            slot_index,
            remaining,
            target,
            FillDirection::Forward,
        )
    }

    const fn is_stand_slot(&self, slot_index: usize) -> bool {
        self.bottles.contains(slot_index)
            || self.ingredient.contains(slot_index)
            || self.fuel.contains(slot_index)
    }
}

impl MenuKind for BrewingStandKind {
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.container.still_valid(player)
    }

    fn on_open(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.update_data(behavior, guard);
    }

    fn on_tick(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.update_data(behavior, guard);
    }

    /// Vanilla `BrewingStandMenu.quickMoveStack`.
    fn quick_move(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> Option<ItemStack> {
        if slot_index >= behavior.slots().len() {
            return Some(ItemStack::empty());
        }
        let clicked = behavior.slots()[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return Some(ItemStack::empty());
        }
        let mut remaining = clicked.clone();

        let proceed = if self.is_stand_slot(slot_index) {
            move_into(
                behavior,
                guard,
                slot_index,
                &mut remaining,
                self.player.all(),
                FillDirection::Backward,
            )
        } else {
            self.move_from_player(behavior, guard, slot_index, &clicked, &mut remaining)
        };
        if !proceed {
            return Some(ItemStack::empty());
        }

        behavior.update_quick_move_source(guard, slot_index, &remaining, &clicked);
        if remaining.count() == clicked.count() {
            return Some(ItemStack::empty());
        }
        if let Some(remainder) =
            behavior.slots()[slot_index].on_take(guard, &mut clicked.clone(), player)
        {
            player.add_item_or_drop_with_guard(guard, remainder, Prediction::Predicted);
        }
        Some(clicked)
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::data_components::vanilla_components::PotionContents;
    use steel_registry::{init_vanilla_registry, vanilla_items, vanilla_potions};
    use steel_utils::locks::IntoShared as _;

    use super::*;
    use crate::inventory::container::SimpleContainer;

    fn menu() -> Menu {
        brewing_stand(
            PlayerInventory::new().into_shared(),
            1,
            ContainerRef::from(SimpleContainer::new(5).into_shared()),
        )
    }

    #[test]
    fn slots_accept_what_vanilla_accepts() {
        init_vanilla_registry();
        let menu = menu();
        let slots = menu.behavior().slots();
        let water =
            PotionContents::create_item_stack(&vanilla_items::POTION, &vanilla_potions::WATER);
        let wart = ItemStack::new(&vanilla_items::NETHER_WART);
        let blaze = ItemStack::new(&vanilla_items::BLAZE_POWDER);

        assert!(slots[0].may_place(&water));
        assert!(slots[0].may_place(&ItemStack::new(&vanilla_items::GLASS_BOTTLE)));
        assert!(!slots[0].may_place(&wart));
        assert!(slots[3].may_place(&wart));
        assert!(!slots[3].may_place(&water));
        assert!(slots[4].may_place(&blaze));
        assert!(!slots[4].may_place(&wart));
        assert_eq!(slots.len(), 5 + 36);
    }
}
