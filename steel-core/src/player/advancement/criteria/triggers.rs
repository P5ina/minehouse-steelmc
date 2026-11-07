//! Vanilla `CriteriaTriggers` fired by players.

use steel_registry::item_stack::ItemStack;
use steel_utils::Identifier;

use super::trigger;
use crate::inventory::container::Container as _;
use crate::player::Player;

/// `minecraft:tick`
pub(crate) const TICK: Identifier = Identifier::vanilla_static("tick");
/// `minecraft:location`
pub(crate) const LOCATION: Identifier = Identifier::vanilla_static("location");
/// `minecraft:slept_in_bed`
pub(crate) const SLEPT_IN_BED: Identifier = Identifier::vanilla_static("slept_in_bed");
/// `minecraft:hero_of_the_village` (vanilla `CriteriaTriggers.RAID_WIN`)
pub(crate) const RAID_WIN: Identifier = Identifier::vanilla_static("hero_of_the_village");
/// `minecraft:voluntary_exile` (vanilla `CriteriaTriggers.RAID_OMEN`)
pub(crate) const RAID_OMEN: Identifier = Identifier::vanilla_static("voluntary_exile");
/// `minecraft:inventory_changed`
pub(crate) const INVENTORY_CHANGED: Identifier = Identifier::vanilla_static("inventory_changed");

impl Player {
    /// Vanilla `PlayerTrigger.trigger(player)` for a trigger without
    /// conditions of its own (`tick`, `location`, `slept_in_bed`, ...).
    pub(crate) fn trigger_player_criteria(&self, trigger_id: &Identifier) {
        trigger(self, trigger_id, |_| true);
    }

    /// Vanilla `InventoryChangeTrigger.trigger(player, inventory, changedItem)`.
    pub(crate) fn trigger_inventory_changed(&self, changed_item: &ItemStack) {
        let items: Vec<ItemStack> = {
            let inventory = self.inventory.lock();
            (0..inventory.get_container_size())
                .map(|slot| inventory.get_item(slot).clone())
                .collect()
        };
        let mut slots_full = 0;
        let mut slots_empty = 0;
        let mut slots_occupied = 0;
        for stack in &items {
            if stack.is_empty() {
                slots_empty += 1;
            } else {
                slots_occupied += 1;
                if stack.count() >= stack.max_stack_size() {
                    slots_full += 1;
                }
            }
        }

        trigger(self, &INVENTORY_CHANGED, |conditions| {
            let slots = &conditions.slots;
            if !(slots.full.matches(slots_full)
                && slots.empty.matches(slots_empty)
                && slots.occupied.matches(slots_occupied))
            {
                return false;
            }
            match conditions.items.as_slice() {
                [] => true,
                [predicate] => !changed_item.is_empty() && predicate.test(changed_item),
                predicates => {
                    // Every predicate must match some stack; each stack
                    // removes all predicates it matches.
                    let mut remaining: Vec<_> = predicates.iter().collect();
                    for stack in &items {
                        if remaining.is_empty() {
                            return true;
                        }
                        if !stack.is_empty() {
                            remaining.retain(|predicate| !predicate.test(stack));
                        }
                    }
                    remaining.is_empty()
                }
            }
        });
    }
}
