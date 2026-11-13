//! Dispenser and dropper block entity: a 9-slot container.

use std::{
    mem,
    sync::{Arc, Weak},
};

use simdnbt::ToNbtTag;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_entity_types;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};
use text_components::TextComponent;

use crate::block_entity::container_loot::{ContainerLoot, LOOT_TABLE_SEED_TAG, LOOT_TABLE_TAG};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::world::World;

/// Number of slots in a dispenser or dropper (3 by 3).
pub const DISPENSER_SLOTS: usize = 9;

/// Dispenser or dropper block entity.
///
/// Steel does not yet apply container locks: `lock` and `CustomName` are kept
/// as loaded.
pub struct DispenserBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<DispenserContainer>>,
    container_ref: ContainerRef,
    /// Vanilla `RandomizableContainerBlockEntity` loot table.
    loot: Arc<ContainerLoot>,
    /// Saved fields other than `Items`, written back unchanged.
    preserved: SyncMutex<NbtCompound>,
}

struct DispenserContainer {
    items: Vec<ItemStack>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `DispenserBlockEntity`.
unsafe impl DowncastType for DispenserBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/dispenser");
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a dispenser block entity.
unsafe impl DowncastType for DispenserContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/dispenser");
}

impl DispenserBlockEntity {
    /// Creates a block entity of the `dispenser` type.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self::with_type(&vanilla_block_entity_types::DISPENSER, level, pos, state)
    }

    /// Creates a block entity of the `dropper` type.
    #[must_use]
    pub fn new_dropper(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self::with_type(&vanilla_block_entity_types::DROPPER, level, pos, state)
    }

    fn with_type(
        block_entity_type: BlockEntityTypeRef,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> Self {
        let base = Arc::new(BlockEntityBase::new(block_entity_type, level, pos, state));
        let container = Arc::new(SyncMutex::new(DispenserContainer {
            items: vec![ItemStack::empty(); DISPENSER_SLOTS],
        }));
        let shared_container: SharedContainer =
            Arc::<SyncMutex<DispenserContainer>>::clone(&container);
        let loot = Arc::new(ContainerLoot::new());
        Self {
            container_ref: ContainerRef::owned_by_randomizable_block_entity(
                shared_container,
                Arc::clone(&base),
                Arc::clone(&loot),
            ),
            base,
            container,
            loot,
            preserved: SyncMutex::new(NbtCompound::new()),
        }
    }

    /// Picks a random non-empty slot, each with equal chance, or `None` when empty.
    ///
    /// Vanilla `DispenserBlockEntity.getRandomSlot`: reservoir sampling with one
    /// `nextInt` call per non-empty slot.
    pub fn get_random_slot(&self, mut next_int: impl FnMut(i32) -> i32) -> Option<usize> {
        self.container_ref.unpack_loot_table(None);
        let container = self.container.lock();
        let mut chosen = None;
        let mut seen = 1;
        for (slot, item) in container.items.iter().enumerate() {
            if !item.is_empty() {
                if next_int(seen) == 0 {
                    chosen = Some(slot);
                }
                seen += 1;
            }
        }
        chosen
    }

    /// Returns a copy of the stack in `slot`.
    #[must_use]
    pub fn get_item(&self, slot: usize) -> ItemStack {
        self.container
            .lock()
            .items
            .get(slot)
            .cloned()
            .unwrap_or_else(ItemStack::empty)
    }

    /// Replaces the stack in `slot` and notifies the block entity.
    pub fn set_item(&self, slot: usize, stack: ItemStack) {
        {
            let mut container = self.container.lock();
            container.set_item(slot, stack);
        }
        self.set_changed();
    }

    /// Puts `stack` into the first slots that take it and returns what did not fit.
    ///
    /// Vanilla `DispenserBlockEntity.insertItem`, used for bucket remainders.
    pub fn insert_item(&self, mut stack: ItemStack) -> ItemStack {
        {
            let mut container = self.container.lock();
            let max_stack_size = container.get_max_stack_size_for_item(&stack);
            for slot in 0..DISPENSER_SLOTS {
                let existing = container.items[slot].clone();
                if existing.is_empty() || ItemStack::is_same_item_same_components(&stack, &existing)
                {
                    let amount = stack.count().min(max_stack_size - existing.count());
                    if amount > 0 {
                        if existing.is_empty() {
                            let split = stack.split(amount);
                            container.set_item(slot, split);
                        } else {
                            stack.shrink(amount);
                            container.items[slot].grow(amount);
                        }
                    }
                    if stack.is_empty() {
                        break;
                    }
                }
            }
        }
        self.set_changed();
        stack
    }

    /// Returns the saved custom name, if any.
    #[must_use]
    pub fn custom_name(&self) -> Option<TextComponent> {
        let preserved = self.preserved.lock();
        let tag = preserved.get("CustomName")?;
        TextComponent::from_nbt(tag)
    }
}

impl BlockEntity for DispenserBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        // Vanilla `Containers.dropContents` reads every slot, rolling the loot.
        self.container_ref.unpack_loot_table(None);
        let items = {
            let mut container = self.container.lock();
            mem::replace(
                &mut container.items,
                vec![ItemStack::empty(); DISPENSER_SLOTS],
            )
        };
        let Some(world) = self.get_level() else {
            return;
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt_view: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());

        if !self.loot.load(nbt_view)
            && let Some(items_list) = nbt_view.list("Items")
            && let Some(compounds) = items_list.compounds()
        {
            for compound in compounds {
                let Some(slot) = compound.byte("Slot") else {
                    continue;
                };
                let slot = slot as usize;
                if slot < DISPENSER_SLOTS
                    && let Some(item) = ItemStack::from_borrowed_compound(&compound)
                {
                    container.items[slot] = item;
                }
            }
        }

        let mut preserved = nbt_view.to_owned();
        while preserved.remove("Items").is_some() {}
        while preserved.remove("components").is_some() {}
        while preserved.remove(LOOT_TABLE_TAG).is_some() {}
        while preserved.remove(LOOT_TABLE_SEED_TAG).is_some() {}
        *self.preserved.lock() = preserved;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        for (key, value) in self.preserved.lock().iter() {
            nbt.insert(key.to_string(), value.clone());
        }
        if self.loot.save(nbt) {
            return;
        }
        let container = self.container.lock();
        let mut items: Vec<NbtCompound> = Vec::new();
        for (slot, item) in container.items.iter().enumerate() {
            if item.is_empty() {
                continue;
            }
            if let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() {
                item_nbt.insert("Slot", slot as i8);
                items.push(item_nbt);
            }
        }
        nbt.insert("Items", NbtList::Compound(items));
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        None
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

impl Container for DispenserContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        DISPENSER_SLOTS
    }

    fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        if slot < DISPENSER_SLOTS {
            let max_stack_size = self.get_max_stack_size_for_item(&stack);
            if !stack.is_empty() && stack.count() > max_stack_size {
                stack.set_count(max_stack_size);
            }
            self.items[slot] = stack;
        }
    }

    fn get_max_stack_size(&self) -> i32 {
        64
    }

    fn set_changed(&mut self) {}
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};

    use super::*;

    #[test]
    fn save_keeps_an_unrolled_loot_table() {
        init_vanilla_registry();
        let dispenser = DispenserBlockEntity::new(
            Weak::new(),
            BlockPos::new(0, 64, 0),
            vanilla_blocks::DISPENSER.default_state(),
        );
        let mut nbt = NbtCompound::new();
        nbt.insert("LootTable", "minecraft:chests/jungle_temple_dispenser");
        nbt.insert("LootTableSeed", 42_i64);
        if let Err(error) = dispenser.load_with_owned_components(&nbt) {
            panic!("test NBT must load: {error}");
        }

        let mut saved = NbtCompound::new();
        dispenser.save_additional(&mut saved);

        assert_eq!(
            saved.string("LootTable").map(ToString::to_string),
            Some("minecraft:chests/jungle_temple_dispenser".to_owned())
        );
        assert_eq!(saved.long("LootTableSeed"), Some(42));
    }

    #[test]
    fn random_slot_only_picks_filled_slots() {
        init_vanilla_registry();
        let dispenser = DispenserBlockEntity::new(
            Weak::new(),
            BlockPos::new(0, 64, 0),
            vanilla_blocks::DISPENSER.default_state(),
        );
        assert_eq!(dispenser.get_random_slot(|_| 0), None);
        dispenser.set_item(2, ItemStack::new(&vanilla_items::STONE));
        dispenser.set_item(6, ItemStack::new(&vanilla_items::DIRT));
        // `nextInt(1)` then `nextInt(2)`: 0 for both keeps the last filled slot.
        assert_eq!(dispenser.get_random_slot(|_| 0), Some(6));
        assert_eq!(dispenser.get_random_slot(|bound| bound - 1), Some(2));
    }
}
