//! Deferred container loot, vanilla `RandomizableContainer` and `LootTable.fill`.
//!
//! Structure chests, barrels, dispensers and the like are generated with a loot
//! table instead of items. The table is rolled the first time anything reaches
//! the contents: a player opening the container, a hopper, a comparator or the
//! block breaking.

use rand::{Rng, RngExt as _, SeedableRng as _, rngs::StdRng};
use simdnbt::borrow::NbtCompound as NbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::{LootContext, LootTableRef};
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::{BlockPos, Identifier, locks::SyncMutex};

use crate::entity::{LivingEntity as _, entity_loot_ref, living_loot_equipment};
use crate::inventory::container::Container;
use crate::player::Player;

/// NBT key of the deferred loot table.
pub const LOOT_TABLE_TAG: &str = "LootTable";
/// NBT key of the deferred loot table's seed.
pub const LOOT_TABLE_SEED_TAG: &str = "LootTableSeed";

/// A loot table waiting to fill a container, with its seed (0 for a random roll).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingLoot {
    /// The loot table to roll.
    pub table: Identifier,
    /// The seed, or 0 to roll with a fresh random source.
    pub seed: i64,
}

/// The loot table a container still has to roll.
#[derive(Default)]
pub struct ContainerLoot {
    pending: SyncMutex<Option<PendingLoot>>,
}

impl ContainerLoot {
    /// No loot pending.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Vanilla `setLootTable(table, seed)`.
    pub fn set(&self, table: Identifier, seed: i64) {
        *self.pending.lock() = Some(PendingLoot { table, seed });
    }

    /// Clears the pending loot without rolling it.
    pub fn clear(&self) {
        *self.pending.lock() = None;
    }

    /// The pending loot table, if any.
    #[must_use]
    pub fn get(&self) -> Option<PendingLoot> {
        self.pending.lock().clone()
    }

    /// Whether a loot table is still waiting to be rolled.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.lock().is_some()
    }

    /// Takes the pending loot table, leaving none.
    #[must_use]
    pub fn take(&self) -> Option<PendingLoot> {
        self.pending.lock().take()
    }

    /// Vanilla `RandomizableContainer.tryLoadLootTable`: returns whether the
    /// tag had a loot table, in which case vanilla skips loading `Items`.
    pub fn load(&self, nbt: NbtCompoundView<'_, '_>) -> bool {
        let table = nbt
            .string(LOOT_TABLE_TAG)
            .and_then(|table| table.to_str().parse::<Identifier>().ok());
        let seed = nbt.long(LOOT_TABLE_SEED_TAG).unwrap_or(0);
        let loaded = table.is_some();
        *self.pending.lock() = table.map(|table| PendingLoot { table, seed });
        loaded
    }

    /// Vanilla `RandomizableContainer.trySaveLootTable`: returns whether a loot
    /// table was written, in which case vanilla does not save `Items`.
    pub fn save(&self, nbt: &mut NbtCompound) -> bool {
        let Some(pending) = self.get() else {
            return false;
        };
        nbt.insert(LOOT_TABLE_TAG, pending.table.to_string());
        if pending.seed != 0 {
            nbt.insert(LOOT_TABLE_SEED_TAG, pending.seed);
        }
        true
    }

    /// Vanilla `RandomizableContainer.unpackLootTable`: rolls the pending table
    /// into `container`, with `player`'s luck when a player opened it.
    ///
    /// Returns whether a table was rolled.
    pub fn unpack(
        &self,
        container: &mut dyn Container,
        pos: BlockPos,
        player: Option<&Player>,
    ) -> bool {
        let Some(pending) = self.take() else {
            return false;
        };
        // TODO: Trigger CriteriaTriggers.GENERATE_LOOT once that trigger exists.
        let Some(table) = REGISTRY.loot_tables.by_key(&pending.table) else {
            log::warn!("Unknown container loot table {}", pending.table);
            return true;
        };
        let origin = (
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.5,
            f64::from(pos.z()) + 0.5,
        );
        if pending.seed == 0 {
            fill(container, table, origin, player, &mut rand::rng());
        } else {
            #[expect(
                clippy::cast_sign_loss,
                reason = "the seed's bits are reused as the random source's seed"
            )]
            let mut rng = StdRng::seed_from_u64(pending.seed as u64);
            fill(container, table, origin, player, &mut rng);
        }
        true
    }
}

/// Vanilla `LootTable.fill`: rolls `table` and scatters the stacks over the
/// container's empty slots.
pub fn fill<R: Rng>(
    container: &mut dyn Container,
    table: LootTableRef,
    origin: (f64, f64, f64),
    player: Option<&Player>,
    rng: &mut R,
) {
    let equipment = player.map(living_loot_equipment);
    let mut items = {
        let mut ctx = LootContext::new(&mut *rng).with_origin(origin.0, origin.1, origin.2);
        if let Some(player) = player {
            ctx = ctx
                .with_luck(player.get_luck())
                .with_this_entity(entity_loot_ref(player, equipment.as_ref()));
        }
        split_oversized_stacks(table.get_random_items(&mut ctx))
    };
    let mut available_slots = available_slots(container, rng);
    shuffle_and_split_items(&mut items, available_slots.len(), rng);
    for item in items {
        let Some(slot) = available_slots.pop() else {
            log::warn!("Tried to over-fill a container");
            return;
        };
        container.set_item(slot, item);
    }
}

/// Vanilla `LootTable.createStackSplitter`: a rolled stack over its maximum
/// size becomes several full stacks.
fn split_oversized_stacks(items: Vec<ItemStack>) -> Vec<ItemStack> {
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let max = item.max_stack_size();
        if item.count() < max {
            result.push(item);
            continue;
        }
        let mut count = item.count();
        while count > 0 {
            let part = count.min(max);
            count -= part;
            result.push(item.copy_with_count(part));
        }
    }
    result
}

/// Vanilla `LootTable.getAvailableSlots`: the empty slots, shuffled.
fn available_slots<R: Rng>(container: &dyn Container, rng: &mut R) -> Vec<usize> {
    let mut slots: Vec<usize> = (0..container.get_container_size())
        .filter(|&slot| container.get_item(slot).is_empty())
        .collect();
    shuffle(&mut slots, rng);
    slots
}

/// Vanilla `LootTable.shuffleAndSplitItems`: splits stacks into smaller ones
/// while there are spare slots, then shuffles the result.
fn shuffle_and_split_items<R: Rng>(
    result: &mut Vec<ItemStack>,
    available_slots: usize,
    rng: &mut R,
) {
    let mut splittable = Vec::new();
    result.retain(|item| {
        if item.is_empty() {
            return false;
        }
        if item.count() > 1 {
            splittable.push(item.clone());
            return false;
        }
        true
    });

    while available_slots > result.len() + splittable.len() && !splittable.is_empty() {
        let index = next_int(rng, 0, splittable.len() - 1);
        let mut item = splittable.remove(index);
        let remove = next_int(
            rng,
            1,
            usize::try_from(item.count() / 2).unwrap_or_default(),
        );
        let copy = item.split(i32::try_from(remove).unwrap_or(i32::MAX));
        if item.count() > 1 && rng.random_bool(0.5) {
            splittable.push(item);
        } else {
            result.push(item);
        }
        if copy.count() > 1 && rng.random_bool(0.5) {
            splittable.push(copy);
        } else {
            result.push(copy);
        }
    }

    result.append(&mut splittable);
    shuffle(result, rng);
}

/// Vanilla `Mth.nextInt`: a uniform integer in `min..=max`, or `min` when the
/// range is empty.
fn next_int<R: Rng>(rng: &mut R, min: usize, max: usize) -> usize {
    if min >= max {
        min
    } else {
        rng.random_range(min..=max)
    }
}

/// Vanilla `Util.shuffle`: Fisher-Yates from the back.
fn shuffle<T, R: Rng>(list: &mut [T], rng: &mut R) {
    for i in (2..=list.len()).rev() {
        list.swap(i - 1, rng.random_range(0..i));
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_items};
    use steel_utils::locks::IntoShared as _;

    use std::io::Cursor;

    use simdnbt::borrow::read;
    use simdnbt::owned::BaseNbt;

    use super::*;
    use crate::inventory::container::SimpleContainer;

    #[test]
    fn oversized_stacks_split_into_full_stacks() {
        init_vanilla_registry();
        let split =
            split_oversized_stacks(vec![ItemStack::with_count(&vanilla_items::ENDER_PEARL, 40)]);
        let counts: Vec<i32> = split.iter().map(ItemStack::count).collect();
        assert_eq!(counts, vec![16, 16, 8]);
    }

    #[test]
    fn splitting_never_loses_items_or_overfills() {
        init_vanilla_registry();
        let mut rng = StdRng::seed_from_u64(7);
        let mut items = vec![
            ItemStack::with_count(&vanilla_items::IRON_INGOT, 9),
            ItemStack::with_count(&vanilla_items::BREAD, 4),
            ItemStack::new(&vanilla_items::SADDLE),
        ];
        shuffle_and_split_items(&mut items, 27, &mut rng);
        let total: i32 = items.iter().map(ItemStack::count).sum();
        assert_eq!(total, 14);
        assert!(items.len() <= 27);
        assert!(items.len() > 3, "spare slots should let stacks split");
    }

    #[test]
    fn load_and_save_round_trip_the_loot_table() {
        init_vanilla_registry();
        let loot = ContainerLoot::new();
        loot.set(Identifier::vanilla_static("chests/simple_dungeon"), 42);
        let mut nbt = NbtCompound::new();
        assert!(loot.save(&mut nbt));

        let reloaded = ContainerLoot::new();
        let bytes = {
            let mut out = Vec::new();
            BaseNbt::new("", nbt).write(&mut out);
            out
        };
        let base = read(&mut Cursor::new(&bytes[..]))
            .expect("nbt reads back")
            .unwrap();
        assert!(reloaded.load(base.as_compound()));
        assert_eq!(reloaded.get(), loot.get());
    }

    #[test]
    fn unpacking_fills_an_empty_container_once() {
        init_vanilla_registry();
        let loot = ContainerLoot::new();
        loot.set(Identifier::vanilla_static("chests/simple_dungeon"), 1234);
        let container = SimpleContainer::new(27).into_shared();

        assert!(loot.unpack(&mut *container.lock(), BlockPos::new(0, 64, 0), None));
        assert!(!container.lock().is_empty());
        assert!(!loot.is_pending());
        assert!(!loot.unpack(&mut *container.lock(), BlockPos::new(0, 64, 0), None));
    }
}
