//! Lectern block entity.

use std::io::Cursor;
use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::{
    BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView,
    read_compound as read_borrowed_compound,
};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::data_components::vanilla_components::{
    WRITABLE_BOOK_CONTENT, WRITTEN_BOOK_CONTENT,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    level_events, vanilla_block_entity_types, vanilla_blocks, vanilla_game_events,
};
use steel_utils::locks::{IntoShared as _, SyncMutex};
use steel_utils::nbt::{merge_nbt_compounds, nbt_compounds_equal};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::Entity;
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::world::game_event::GameEventContext;
use crate::world::{LevelReader as _, World};

/// Vanilla `LecternBlock.PAGE_CHANGE_IMPULSE_TICKS`.
pub const PAGE_CHANGE_IMPULSE_TICKS: i32 = 2;

/// Vanilla `LecternBlockEntity.bookAccess`: the one-slot view of the book that
/// the lectern menu works on, together with the page state.
pub struct LecternBook {
    book: [ItemStack; 1],
    page: i32,
    page_count: i32,
    /// Vanilla `onBookItemRemove` ran; the block state is published unlocked.
    book_removed: bool,
    /// Vanilla `setPage` changed the page; the pulse is published unlocked.
    page_changed: bool,
}

// SAFETY: This Steel-owned key uniquely identifies the lectern's book access.
unsafe impl DowncastType for LecternBook {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/lectern");
}

impl LecternBook {
    fn new() -> Self {
        Self {
            book: [ItemStack::empty()],
            page: 0,
            page_count: 0,
            book_removed: false,
            page_changed: false,
        }
    }

    /// The book on the lectern.
    #[must_use]
    pub const fn book(&self) -> &ItemStack {
        &self.book[0]
    }

    /// Vanilla `LecternBlockEntity.hasBook`.
    #[must_use]
    pub fn has_book(&self) -> bool {
        has_book_content(self.book())
    }

    /// Vanilla `LecternBlockEntity.getPage`.
    #[must_use]
    pub const fn page(&self) -> i32 {
        self.page
    }

    /// Vanilla `LecternBlockEntity.setPage`.
    pub const fn set_page(&mut self, page: i32) {
        let new_page = clamp_page(page, self.page_count);
        if new_page != self.page {
            self.page = new_page;
            self.page_changed = true;
        }
    }

    /// Vanilla `LecternBlockEntity.getRedstoneSignal`.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "Vanilla divides the page index as floats and floors the result"
    )]
    pub fn redstone_signal(&self) -> i32 {
        let page_progress = if self.page_count > 1 {
            self.page as f32 / (self.page_count as f32 - 1.0)
        } else {
            1.0
        };
        (page_progress * 14.0).floor() as i32 + i32::from(self.has_book())
    }

    /// Vanilla `LecternBlockEntity.setBook`.
    fn set_book(&mut self, book: ItemStack) {
        // TODO: Vanilla resolves written book components (`WrittenBookContent.resolveForItem`)
        // here; Steel has no text component resolution yet.
        self.page_count = page_count(&book);
        self.book[0] = book;
        self.page = 0;
    }

    /// Vanilla `LecternBlockEntity.onBookItemRemove`.
    const fn on_book_item_remove(&mut self) {
        self.page = 0;
        self.page_count = 0;
        self.book_removed = true;
    }
}

impl Container for LecternBook {
    fn items(&self) -> &[ItemStack] {
        &self.book
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.book
    }

    fn remove_item(&mut self, slot: usize, count: i32) -> ItemStack {
        if slot != 0 {
            return ItemStack::empty();
        }
        let result = self.book[0].split(count);
        if self.book[0].is_empty() {
            self.on_book_item_remove();
        }
        result
    }

    fn remove_item_no_update(&mut self, slot: usize) -> ItemStack {
        if slot != 0 {
            return ItemStack::empty();
        }
        let previous = mem::take(&mut self.book[0]);
        self.on_book_item_remove();
        previous
    }

    /// Vanilla ignores writes into the book slot.
    fn set_item(&mut self, _slot: usize, _stack: ItemStack) {}

    fn get_max_stack_size(&self) -> i32 {
        1
    }

    fn set_changed(&mut self) {}

    fn can_place_item(&self, _slot: usize, _stack: &ItemStack) -> bool {
        false
    }

    fn clear_content(&mut self) -> i32 {
        0
    }
}

/// Vanilla `Mth.clamp(page, 0, pageCount - 1)`, which yields -1 without pages.
const fn clamp_page(page: i32, page_count: i32) -> i32 {
    let page = if page < 0 { 0 } else { page };
    if page > page_count - 1 {
        page_count - 1
    } else {
        page
    }
}

fn has_book_content(book: &ItemStack) -> bool {
    book.has(WRITABLE_BOOK_CONTENT) || book.has(WRITTEN_BOOK_CONTENT)
}

/// Vanilla `LecternBlockEntity.getPageCount`.
fn page_count(book: &ItemStack) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "book page lists are capped far below i32::MAX"
    )]
    if let Some(written) = book.get(WRITTEN_BOOK_CONTENT) {
        written.pages().len() as i32
    } else {
        book.get(WRITABLE_BOOK_CONTENT)
            .map_or(0, |writable| writable.pages().len() as i32)
    }
}

/// Vanilla `LecternBlockEntity`.
pub struct LecternBlockEntity {
    base: Arc<BlockEntityBase>,
    book: Arc<SyncMutex<LecternBook>>,
    book_access: ContainerRef,
}

// SAFETY: This Steel-owned key uniquely identifies this concrete block entity.
unsafe impl DowncastType for LecternBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/lectern");
}

impl LecternBlockEntity {
    /// Creates the block entity at a live world position.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let callback_level = Weak::clone(&level);
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::LECTERN,
            level,
            pos,
            state,
        ));
        let book = LecternBook::new().into_shared();
        let callback_book = Arc::clone(&book);
        let after_changed = Arc::new(move || {
            if let Some(world) = callback_level.upgrade() {
                Self::publish_pending(&world, pos, &callback_book);
            }
        });
        let shared: SharedContainer = Arc::<SyncMutex<LecternBook>>::clone(&book);
        Self {
            book_access: ContainerRef::owned_by_block_entity_with_callback(
                shared,
                Arc::clone(&base),
                after_changed,
            ),
            base,
            book,
        }
    }

    /// Runs the world side effects that book access changes asked for.
    fn publish_pending(world: &Arc<World>, pos: BlockPos, book: &SyncMutex<LecternBook>) {
        let (page_changed, book_removed) = {
            let mut book = book.lock();
            let pending = (book.page_changed, book.book_removed);
            book.page_changed = false;
            book.book_removed = false;
            pending
        };
        let state = world.get_block_state(pos);
        if state.get_block() != &vanilla_blocks::LECTERN {
            return;
        }
        if page_changed {
            signal_page_change(world, pos, state);
        }
        if book_removed {
            reset_book_state(None, world, pos, world.get_block_state(pos), false);
        }
    }

    /// The menu's view of the book, Vanilla `bookAccess`.
    ///
    /// Not exposed as [`BlockEntity::container_ref`]: hoppers and comparators
    /// do not see the lectern as a container.
    #[must_use]
    pub fn book_access(&self) -> ContainerRef {
        self.book_access.clone()
    }

    /// Vanilla `LecternBlockEntity.getBook`.
    #[must_use]
    pub fn book(&self) -> ItemStack {
        self.book.lock().book().clone()
    }

    /// Vanilla `LecternBlockEntity.getPage`.
    #[must_use]
    pub fn page(&self) -> i32 {
        self.book.lock().page()
    }

    /// Vanilla `LecternBlockEntity.hasBook`.
    #[must_use]
    pub fn has_book(&self) -> bool {
        self.book.lock().has_book()
    }

    /// Vanilla `LecternBlockEntity.getRedstoneSignal`.
    #[must_use]
    pub fn redstone_signal(&self) -> i32 {
        self.book.lock().redstone_signal()
    }

    /// Vanilla `LecternBlockEntity.setBook`.
    pub fn set_book(&self, book: ItemStack) {
        self.book.lock().set_book(book);
        self.base.set_changed();
    }

    /// Merges an item's `BLOCK_ENTITY_DATA` payload into this lectern, Vanilla
    /// `TypedEntityData.loadInto`.
    #[expect(
        clippy::must_use_candidate,
        reason = "placement ignores whether the data changed anything"
    )]
    pub fn apply_item_block_entity_data(&self, payload: &NbtCompound) -> bool {
        let before = self.save_custom_only();
        let mut merged = self.save_custom_only();
        merge_nbt_compounds(&mut merged, payload);
        if nbt_compounds_equal(&before, &merged) {
            return false;
        }
        let mut bytes = Vec::new();
        merged.write(&mut bytes);
        let Ok(borrowed) = read_borrowed_compound(&mut Cursor::new(bytes.as_slice())) else {
            log::warn!(
                "failed to reborrow item block-entity data for lectern at {:?}",
                self.get_block_pos()
            );
            return false;
        };
        self.load_additional(&borrowed);
        self.set_changed();
        true
    }
}

/// Vanilla `LecternBlock.resetBookState`.
pub(crate) fn reset_book_state(
    source: Option<&dyn Entity>,
    world: &Arc<World>,
    pos: BlockPos,
    state: BlockStateId,
    has_book: bool,
) {
    let new_state = state
        .set_value(&BlockStateProperties::POWERED, false)
        .set_value(&BlockStateProperties::HAS_BOOK, has_book);
    world.set_block(pos, new_state, UpdateFlags::UPDATE_ALL);
    world.game_event(
        &vanilla_game_events::BLOCK_CHANGE,
        pos,
        &GameEventContext::new(source, Some(new_state)),
    );
    update_below(world, pos, state);
}

/// Vanilla `LecternBlock.signalPageChange`.
pub(crate) fn signal_page_change(world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
    change_powered(world, pos, state, true);
    world.schedule_block_tick_default(pos, state.get_block(), PAGE_CHANGE_IMPULSE_TICKS);
    world.level_event(level_events::SOUND_PAGE_TURN, pos, 0, None);
}

/// Vanilla `LecternBlock.changePowered`.
pub(crate) fn change_powered(
    world: &Arc<World>,
    pos: BlockPos,
    state: BlockStateId,
    powered: bool,
) {
    world.set_block(
        pos,
        state.set_value(&BlockStateProperties::POWERED, powered),
        UpdateFlags::UPDATE_ALL,
    );
    update_below(world, pos, state);
}

/// Vanilla `LecternBlock.updateBelow`. Experimental redstone orientations are
/// intentionally omitted.
pub(crate) fn update_below(world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
    world.update_neighbors_at(pos.below(), state.get_block());
}

impl BlockEntity for LecternBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    /// Vanilla `LecternBlockEntity.preRemoveSideEffects`.
    fn pre_remove_side_effects(&self, pos: BlockPos, state: BlockStateId) {
        if !state
            .try_get_value(&BlockStateProperties::HAS_BOOK)
            .unwrap_or(false)
        {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        let Some(direction) = state.try_get_value(&BlockStateProperties::HORIZONTAL_FACING) else {
            return;
        };
        let book = self.book();
        let (step_x, _, step_z) = direction.offset();
        let x_offset = 0.25 * f64::from(step_x);
        let z_offset = 0.25 * f64::from(step_z);
        let spawn = DVec3::new(
            f64::from(pos.x()) + 0.5 + x_offset,
            f64::from(pos.y()) + 1.0,
            f64::from(pos.z()) + 0.5 + z_offset,
        );
        if let Some(entity) = world.spawn_item(spawn, book) {
            entity.set_default_pickup_delay();
        }
    }

    /// Vanilla `LecternBlockEntity.loadAdditional`.
    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let book = nbt
            .compound("Book")
            .and_then(|compound| ItemStack::from_borrowed_compound(&compound))
            .unwrap_or_else(ItemStack::empty);
        let mut lectern = self.book.lock();
        lectern.set_book(book);
        lectern.page = clamp_page(nbt.int("Page").unwrap_or(0), lectern.page_count);
    }

    /// Vanilla `LecternBlockEntity.saveAdditional`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        let lectern = self.book.lock();
        if !lectern.book().is_empty()
            && let NbtTag::Compound(book) = lectern.book().clone().to_nbt_tag()
        {
            nbt.insert("Book", book);
            nbt.insert("Page", lectern.page);
        }
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::data_components::components::{Filterable, WritableBookContent};
    use steel_registry::{init_vanilla_registry, vanilla_items};

    use super::*;

    fn writable_book(pages: usize) -> ItemStack {
        let mut book = ItemStack::new(&vanilla_items::WRITABLE_BOOK);
        book.set(
            WRITABLE_BOOK_CONTENT,
            WritableBookContent::new(
                (0..pages)
                    .map(|page| Filterable::pass_through(format!("page {page}")))
                    .collect(),
            )
            .expect("a short writable book"),
        );
        book
    }

    #[test]
    fn pages_clamp_and_drive_the_comparator() {
        init_vanilla_registry();
        let mut lectern = LecternBook::new();
        assert_eq!(lectern.redstone_signal(), 14);
        lectern.set_book(writable_book(5));
        assert_eq!(lectern.redstone_signal(), 1);
        lectern.set_page(2);
        assert!(lectern.page_changed);
        assert_eq!(lectern.redstone_signal(), 8);
        lectern.set_page(99);
        assert_eq!(lectern.page(), 4);
        assert_eq!(lectern.redstone_signal(), 15);
        lectern.set_page(-3);
        assert_eq!(lectern.page(), 0);
    }

    #[test]
    fn a_plain_book_has_no_pages() {
        init_vanilla_registry();
        let mut lectern = LecternBook::new();
        lectern.set_book(ItemStack::new(&vanilla_items::BOOK));
        assert!(!lectern.has_book());
        lectern.set_page(3);
        assert_eq!(lectern.page(), -1);
    }

    #[test]
    fn taking_the_book_resets_the_pages() {
        init_vanilla_registry();
        let mut lectern = LecternBook::new();
        lectern.set_book(writable_book(3));
        lectern.set_page(2);
        assert!(!lectern.can_place_item(0, &writable_book(1)));
        lectern.set_item(0, ItemStack::empty());
        assert!(!lectern.book().is_empty());
        let taken = lectern.remove_item_no_update(0);
        assert!(taken.is(&vanilla_items::WRITABLE_BOOK));
        assert!(lectern.book_removed);
        assert_eq!(lectern.page(), 0);
        assert_eq!(lectern.page_count, 0);
    }
}
