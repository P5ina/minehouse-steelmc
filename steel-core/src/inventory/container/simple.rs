use std::mem;

use steel_registry::item_stack::ItemStack;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::inventory::container::Container;

/// A Simple Container
pub struct SimpleContainer {
    items: Vec<ItemStack>,
    times_changed: u32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SimpleContainer`.
unsafe impl DowncastType for SimpleContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/simple");
}

impl SimpleContainer {
    /// Creates a new Simple Container
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self {
            items: vec![ItemStack::empty(); size],
            times_changed: 0,
        }
    }

    /// Creates a Simple Container with already initialized items
    #[must_use]
    pub const fn from_items(items: Vec<ItemStack>) -> Self {
        Self {
            items,
            times_changed: 0,
        }
    }

    /// Number of `set_changed` calls so far; compare snapshots to detect a change.
    #[must_use]
    pub const fn times_changed(&self) -> u32 {
        self.times_changed
    }

    /// Vanilla `SimpleContainer.addItem`: merges into matching stacks, then the
    /// first empty slot. Returns what did not fit.
    pub fn add_item(&mut self, item_stack: &ItemStack) -> ItemStack {
        if item_stack.is_empty() {
            return ItemStack::empty();
        }
        let mut remaining = item_stack.clone();
        self.move_item_to_occupied_slots_with_same_type(&mut remaining);
        if remaining.is_empty() {
            return ItemStack::empty();
        }
        self.move_item_to_empty_slots(&mut remaining);
        if remaining.is_empty() {
            ItemStack::empty()
        } else {
            remaining
        }
    }

    /// Vanilla `SimpleContainer.canAddItem`: an empty slot or a matching stack
    /// with room.
    #[must_use]
    pub fn can_add_item(&self, item_stack: &ItemStack) -> bool {
        self.items.iter().any(|target| {
            target.is_empty()
                || (ItemStack::is_same_item_same_components(target, item_stack)
                    && target.count() < target.max_stack_size())
        })
    }

    fn move_item_to_empty_slots(&mut self, source: &mut ItemStack) {
        for slot in 0..self.items.len() {
            if self.items[slot].is_empty() {
                let moved = mem::replace(source, ItemStack::empty());
                self.set_item(slot, moved);
                return;
            }
        }
    }

    fn move_item_to_occupied_slots_with_same_type(&mut self, source: &mut ItemStack) {
        for slot in 0..self.items.len() {
            if ItemStack::is_same_item_same_components(&self.items[slot], source) {
                let max_count = self.get_max_stack_size_for_item(&self.items[slot]);
                let diff = source.count().min(max_count - self.items[slot].count());
                if diff > 0 {
                    self.items[slot].grow(diff);
                    source.shrink(diff);
                    self.set_changed();
                }
                if source.is_empty() {
                    return;
                }
            }
        }
    }

    /// Vanilla `SimpleContainer.fromItemList`: clears, then adds each stack.
    pub fn load_item_list(&mut self, items: impl IntoIterator<Item = ItemStack>) {
        self.clear_content();
        for stack in items {
            let _ = self.add_item(&stack);
        }
    }

    /// Vanilla `SimpleContainer.storeAsItemList`: the non-empty stacks in order.
    #[must_use]
    pub fn store_as_item_list(&self) -> Vec<ItemStack> {
        self.items
            .iter()
            .filter(|stack| !stack.is_empty())
            .cloned()
            .collect()
    }
}

impl Container for SimpleContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn set_changed(&mut self) {
        self.times_changed = self.times_changed.wrapping_add(1);
    }
}
