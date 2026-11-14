//! Vanilla `ShelfBlockEntity`: three item slots shown on the shelf's face.

use std::io;
use std::mem;
use std::sync::{Arc, Weak};

use simdnbt::ToNbtTag as _;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::ItemStackTemplate;
use steel_registry::data_components::vanilla_components::CONTAINER;
use steel_registry::data_components::{DataComponentMap, ItemContainerContents};
use steel_registry::game_events::GameEventRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_block_entity_types, vanilla_game_events};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};

use crate::block_entity::{BlockEntity, BlockEntityBase, ImplicitComponentGetter};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `ShelfBlockEntity.MAX_ITEMS`.
pub const SHELF_SLOTS: usize = 3;

const ITEMS_TAG: &str = "Items";
const SLOT_TAG: &str = "Slot";
const ALIGN_ITEMS_TO_BOTTOM_TAG: &str = "align_items_to_bottom";

struct ShelfContainer {
    items: [ItemStack; SHELF_SLOTS],
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a shelf block entity.
unsafe impl DowncastType for ShelfContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/shelf");
}

impl Container for ShelfContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn set_changed(&mut self) {}
}

/// Shelf block entity.
pub struct ShelfBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<ShelfContainer>>,
    container_ref: ContainerRef,
    align_items_to_bottom: SyncMutex<bool>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ShelfBlockEntity`.
unsafe impl DowncastType for ShelfBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/shelf");
}

impl ShelfBlockEntity {
    /// Creates an empty shelf.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let callback_level = Weak::clone(&level);
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::SHELF,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(ShelfContainer {
            items: [ItemStack::empty(), ItemStack::empty(), ItemStack::empty()],
        }));
        let shared_container: SharedContainer = Arc::<SyncMutex<ShelfContainer>>::clone(&container);
        // Vanilla `setChanged()`: hoppers and other container users change the
        // shelf with the `block_activate` game event.
        let after_changed = Arc::new(move || {
            if let Some(world) = callback_level.upgrade() {
                Self::publish_change(&world, pos, Some(&vanilla_game_events::BLOCK_ACTIVATE));
            }
        });
        Self {
            container_ref: ContainerRef::owned_by_block_entity_with_callback(
                shared_container,
                Arc::clone(&base),
                after_changed,
            ),
            base,
            container,
            align_items_to_bottom: SyncMutex::new(false),
        }
    }

    /// Vanilla `setChanged(event)` after the block entity is marked changed:
    /// the game event, then `sendBlockUpdated` so viewers see the items.
    fn publish_change(world: &Arc<World>, pos: BlockPos, event: Option<GameEventRef>) {
        if let Some(event) = event {
            world.game_event(
                event,
                pos,
                &GameEventContext::new(None, Some(world.get_block_state(pos))),
            );
        }
        world.send_block_updated(pos);
    }

    /// Vanilla `setChanged(event)`.
    pub fn set_changed_with(&self, event: Option<GameEventRef>) {
        BlockEntity::set_changed(self);
        if let Some(world) = self.get_level() {
            Self::publish_change(&world, self.base.pos(), event);
        }
    }

    /// Vanilla `swapItemNoUpdate`: puts `stack` in `slot` and returns what
    /// was there, without marking the shelf changed.
    #[must_use]
    pub fn swap_item_no_update(&self, slot: usize, stack: ItemStack) -> ItemStack {
        let mut container = self.container.lock();
        let Some(slot) = container.items.get_mut(slot) else {
            return stack;
        };
        mem::replace(slot, stack)
    }

    /// A copy of the item in `slot`.
    #[must_use]
    pub fn item(&self, slot: usize) -> ItemStack {
        self.container
            .lock()
            .items
            .get(slot)
            .cloned()
            .unwrap_or_else(ItemStack::empty)
    }

    /// Whether the client draws the items at the bottom of the shelf.
    #[must_use]
    pub fn align_items_to_bottom(&self) -> bool {
        *self.align_items_to_bottom.lock()
    }

    /// Vanilla `ItemContainerContents.fromItems(items)`.
    fn container_contents(&self) -> io::Result<ItemContainerContents> {
        let container = self.container.lock();
        let Some(last) = container.items.iter().rposition(|item| !item.is_empty()) else {
            return Ok(ItemContainerContents::empty());
        };
        let items = container.items[..=last]
            .iter()
            .map(|item| {
                if item.is_empty() {
                    Ok(None)
                } else {
                    ItemStackTemplate::from_stack(item).map(Some)
                }
            })
            .collect::<io::Result<_>>()?;
        ItemContainerContents::new(items)
    }

    /// Vanilla `ContainerHelper.saveAllItems(output, items, true)` plus the
    /// alignment flag.
    fn save_items(&self, nbt: &mut NbtCompound) {
        let container = self.container.lock();
        let mut items = Vec::new();
        for (slot, item) in container.items.iter().enumerate() {
            if item.is_empty() {
                continue;
            }
            if let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() {
                item_nbt.insert(SLOT_TAG, slot as i8);
                items.push(item_nbt);
            }
        }
        nbt.insert(ITEMS_TAG, NbtList::Compound(items));
        nbt.insert(
            ALIGN_ITEMS_TO_BOTTOM_TAG,
            *self.align_items_to_bottom.lock(),
        );
    }
}

impl BlockEntity for ShelfBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    /// Vanilla `BlockEntity.preRemoveSideEffects`: `Containers.dropContents`.
    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        let items = mem::take(&mut self.container.lock().items);
        let Some(world) = self.get_level() else {
            return;
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill_with(ItemStack::empty);
        if let Some(items) = nbt.list(ITEMS_TAG)
            && let Some(compounds) = items.compounds()
        {
            for compound in compounds {
                let Some(slot) = compound
                    .byte(SLOT_TAG)
                    .and_then(|s| usize::try_from(s).ok())
                else {
                    continue;
                };
                if slot < SHELF_SLOTS
                    && let Some(item) = ItemStack::from_borrowed_compound(&compound)
                {
                    container.items[slot] = item;
                }
            }
        }
        *self.align_items_to_bottom.lock() =
            nbt.byte(ALIGN_ITEMS_TO_BOTTOM_TAG).is_some_and(|b| b != 0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_items(nbt);
    }

    /// Vanilla `getUpdateTag`: the items and the alignment flag.
    fn get_update_tag(&self) -> Option<NbtCompound> {
        let mut nbt = NbtCompound::new();
        self.save_items(&mut nbt);
        Some(nbt)
    }

    /// Vanilla `ItemContainerContents.copyInto(items)`.
    fn apply_implicit_components(&self, components: &mut ImplicitComponentGetter<'_>) {
        let contents = components.get_or_default(CONTAINER, ItemContainerContents::empty());
        let mut container = self.container.lock();
        for (slot, item) in container.items.iter_mut().enumerate() {
            *item = contents
                .items()
                .get(slot)
                .and_then(Option::as_ref)
                .map_or_else(ItemStack::empty, ItemStackTemplate::create);
        }
    }

    fn collect_implicit_components(&self, components: &mut DataComponentMap) {
        match self.container_contents() {
            Ok(contents) => components.set(CONTAINER, Some(contents)),
            Err(error) => log::warn!("shelf items do not fit a container component: {error}"),
        }
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};

    use super::*;

    fn test_shelf() -> ShelfBlockEntity {
        init_vanilla_registry();
        ShelfBlockEntity::new(
            Weak::new(),
            BlockPos::new(1, 2, 3),
            vanilla_blocks::OAK_SHELF.default_state(),
        )
    }

    #[test]
    fn items_save_by_slot_and_always_write_the_list_and_the_flag() {
        let shelf = test_shelf();
        let empty = shelf.save_custom_only();
        assert!(empty.list(ITEMS_TAG).is_some());
        assert_eq!(empty.byte(ALIGN_ITEMS_TO_BOTTOM_TAG), Some(0));

        let _ = shelf.swap_item_no_update(2, ItemStack::with_count(&vanilla_items::APPLE, 4));
        let saved = shelf.save_custom_only();
        let mut bytes = Vec::new();
        saved.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(bytes.as_slice())).expect("readable NBT");
        let reloaded = test_shelf();
        reloaded.load_additional(&borrowed);

        assert!(reloaded.item(0).is_empty());
        assert!(reloaded.item(2).is(&vanilla_items::APPLE));
        assert_eq!(reloaded.item(2).count(), 4);
    }

    #[test]
    fn swapping_returns_the_previous_item() {
        let shelf = test_shelf();
        let first = shelf.swap_item_no_update(1, ItemStack::new(&vanilla_items::STICK));
        assert!(first.is_empty());
        let second = shelf.swap_item_no_update(1, ItemStack::empty());
        assert!(second.is(&vanilla_items::STICK));
        assert!(shelf.item(1).is_empty());
    }

    #[test]
    fn the_container_component_keeps_gaps_but_drops_trailing_empty_slots() {
        let shelf = test_shelf();
        let _ = shelf.swap_item_no_update(1, ItemStack::new(&vanilla_items::STICK));
        let contents = shelf.container_contents().expect("fits");
        assert_eq!(contents.items().len(), 2);
        assert!(contents.items()[0].is_none());
        assert!(contents.items()[1].is_some());
    }
}
