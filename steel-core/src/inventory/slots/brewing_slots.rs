//! Brewing stand bottle slots.

use steel_registry::data_components::vanilla_components::POTION_CONTENTS;
use steel_registry::item_stack::ItemStack;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::recipe_access::is_potion_input;
use crate::inventory::slots::{NormalSlot, Slot, SlotStorage};
use crate::player::Player;

/// Vanilla `BrewingStandMenu.PotionSlot`: holds one potion input.
pub struct PotionSlot {
    base: NormalSlot,
}

// SAFETY: This Steel-owned key uniquely identifies brewing stand bottle slots.
unsafe impl DowncastType for PotionSlot {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:slot/brewing_potion");
}

impl PotionSlot {
    /// Creates a brewing stand bottle slot.
    #[must_use]
    pub fn new(container: impl Into<ContainerRef>, index: usize) -> Self {
        Self {
            base: NormalSlot::new(container, index),
        }
    }
}

impl Slot for PotionSlot {
    fn storage(&self) -> &SlotStorage {
        self.base.storage()
    }

    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        self.base.get_item(guard)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        self.base.get_item_mut(guard)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        self.base.set_item(guard, stack);
    }

    /// Vanilla `PotionSlot.mayPlace`.
    fn may_place(&self, stack: &ItemStack) -> bool {
        is_potion_input(stack)
    }

    /// Vanilla `PotionSlot.getMaxStackSize`.
    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        1
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        self.base.set_changed(guard);
    }

    fn get_container_slot(&self) -> usize {
        self.base.get_container_slot()
    }

    /// Vanilla `PotionSlot.onTake`.
    fn on_take(
        &self,
        guard: &mut ContainerLockGuard,
        stack: &ItemStack,
        player: &Player,
    ) -> Option<ItemStack> {
        if let Some(contents) = stack.get(POTION_CONTENTS).cloned() {
            guard.run_unlocked(|| player.trigger_brewed_potion(&contents));
        }
        self.base.set_changed(guard);
        None
    }
}
