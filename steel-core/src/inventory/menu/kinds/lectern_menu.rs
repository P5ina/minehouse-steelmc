//! Lectern menu.

use steel_registry::vanilla_menu_types;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::block_entity::entities::LecternBook;
use crate::inventory::menu::builder::SectionKind;
use crate::inventory::prelude::*;
use crate::player::player_inventory::Prediction;

/// Vanilla `LecternMenu.BUTTON_PREV_PAGE`.
pub const BUTTON_PREV_PAGE: i32 = 1;
/// Vanilla `LecternMenu.BUTTON_NEXT_PAGE`.
pub const BUTTON_NEXT_PAGE: i32 = 2;
/// Vanilla `LecternMenu.BUTTON_TAKE_BOOK`.
pub const BUTTON_TAKE_BOOK: i32 = 3;
/// Vanilla `LecternMenu.BUTTON_PAGE_JUMP_RANGE_START`.
pub const BUTTON_PAGE_JUMP_RANGE_START: i32 = 100;

/// Builds Vanilla's `LecternMenu` over a lectern's book access.
#[must_use]
pub fn lectern(container_id: u8, book_access: ContainerRef) -> Menu {
    let mut builder = MenuBuilder::new(&vanilla_menu_types::LECTERN, container_id);
    builder.section_at(book_access.clone(), [0], SectionKind::Normal);
    let page = builder.data_slot(0);
    builder.build(LecternKind { book_access, page })
}

/// State and button handling of Vanilla's `LecternMenu`.
pub struct LecternKind {
    book_access: ContainerRef,
    page: DataSlot,
}

// SAFETY: This Steel-owned key uniquely identifies lectern menu behavior.
unsafe impl DowncastType for LecternKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/lectern");
}

impl LecternKind {
    fn lectern<'a>(&self, guard: &'a ContainerLockGuard) -> Option<&'a LecternBook> {
        guard.get_typed::<LecternBook>(self.book_access.container_id())
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Vanilla sends data slots as shorts"
    )]
    fn sync_page(&self, behavior: &mut MenuBehavior, guard: &ContainerLockGuard) {
        if let Some(lectern) = self.lectern(guard) {
            self.page.set(behavior, lectern.page() as i16);
        }
    }

    /// Vanilla `LecternMenu.setData`: the page goes through the lectern's
    /// clamped `setPage`, then the menu syncs.
    fn set_page(&self, behavior: &mut MenuBehavior, page: i32) {
        let mut guard = ContainerLockGuard::lock_all(&[&self.book_access]);
        let id = self.book_access.container_id();
        let changed = guard
            .get_typed_mut::<LecternBook>(id)
            .is_some_and(|lectern| {
                let before = lectern.page();
                lectern.set_page(page);
                lectern.page() != before
            });
        if changed {
            // Vanilla `LecternBlockEntity.setChanged` plus the page pulse.
            guard.set_changed(id);
        }
        self.sync_page(behavior, &guard);
    }

    /// Vanilla `LecternMenu.clickMenuButton` for `BUTTON_TAKE_BOOK`.
    fn take_book(&self, player: &Player) -> bool {
        if !player.abilities.lock().may_build {
            return false;
        }
        let book = {
            let mut guard = ContainerLockGuard::lock_all(&[&self.book_access]);
            let id = self.book_access.container_id();
            let Some(book) = guard
                .get_mut(id)
                .map(|lectern| lectern.remove_item_no_update(0))
            else {
                return false;
            };
            guard.set_changed(id);
            book
        };
        player.add_item_or_drop(book, Prediction::Predicted);
        true
    }
}

impl MenuKind for LecternKind {
    /// Vanilla `LecternBlockEntity.bookAccess.stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.book_access.still_valid(player) && {
            let guard = ContainerLockGuard::lock_all(&[&self.book_access]);
            self.lectern(&guard).is_some_and(LecternBook::has_book)
        }
    }

    fn on_open(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.sync_page(behavior, guard);
    }

    fn on_tick(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.sync_page(behavior, guard);
    }

    /// Vanilla `LecternMenu.clickMenuButton`.
    fn click_menu_button(
        &mut self,
        behavior: &mut MenuBehavior,
        button_id: i32,
        player: &Player,
    ) -> bool {
        if button_id >= BUTTON_PAGE_JUMP_RANGE_START {
            self.set_page(behavior, button_id - BUTTON_PAGE_JUMP_RANGE_START);
            return true;
        }
        match button_id {
            BUTTON_PREV_PAGE | BUTTON_NEXT_PAGE => {
                let current = {
                    let guard = ContainerLockGuard::lock_all(&[&self.book_access]);
                    self.lectern(&guard).map_or(0, LecternBook::page)
                };
                let step = if button_id == BUTTON_PREV_PAGE { -1 } else { 1 };
                self.set_page(behavior, current + step);
                true
            }
            BUTTON_TAKE_BOOK => self.take_book(player),
            _ => false,
        }
    }

    /// Vanilla `LecternMenu.quickMoveStack`.
    fn quick_move(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        _slot_index: usize,
        _player: &Player,
    ) -> Option<ItemStack> {
        Some(ItemStack::empty())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::blocks::properties::BlockStateProperties;
    use steel_registry::data_components::components::{Filterable, WritableBookContent};
    use steel_registry::data_components::vanilla_components::WRITABLE_BOOK_CONTENT;
    use steel_registry::{vanilla_blocks, vanilla_items};
    use steel_utils::{BlockPos, ChunkPos, Downcast as _, types::UpdateFlags};

    use super::*;
    use crate::block_entity::SharedBlockEntity;
    use crate::block_entity::entities::LecternBlockEntity;
    use crate::bootstrap::init_globals;
    use crate::entity::Entity as _;
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
    };
    use crate::world::{LevelReader as _, World};

    const POS: BlockPos = BlockPos::new(0, 64, 0);

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

    fn lectern_entity(world: &Arc<World>) -> SharedBlockEntity {
        world.get_block_entity(POS).expect("a lectern block entity")
    }

    fn test_lectern(key: &'static str) -> (TestWorld, Arc<Player>, Menu) {
        init_globals();
        let fixture = fresh_test_world(key);
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(POS));
        assert!(
            world.set_block(
                POS,
                vanilla_blocks::LECTERN
                    .default_state()
                    .set_value(&BlockStateProperties::HAS_BOOK, true),
                UpdateFlags::UPDATE_ALL,
            )
        );
        let entity = lectern_entity(world);
        let lectern = entity
            .downcast_ref::<LecternBlockEntity>()
            .expect("the lectern block entity");
        lectern.set_book(writable_book(3));
        let player = TestPlayerBuilder::new(Arc::clone(world), "LecternTester", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 64.0, 1.5));
        let menu = super::lectern(1, lectern.book_access());
        (fixture, player, menu)
    }

    fn page(world: &Arc<World>) -> i32 {
        lectern_entity(world)
            .downcast_ref::<LecternBlockEntity>()
            .expect("the lectern block entity")
            .page()
    }

    #[test]
    fn page_buttons_turn_and_pulse_the_lectern() {
        let (fixture, player, mut menu) = test_lectern("lectern_menu_pages");
        let world = &fixture.world;
        assert!(menu.still_valid(&player));

        assert!(menu.click_menu_button(BUTTON_NEXT_PAGE, &player));
        assert_eq!(page(world), 1);
        assert!(
            world
                .get_block_state(POS)
                .get_value(&BlockStateProperties::POWERED)
        );

        assert!(menu.click_menu_button(BUTTON_PAGE_JUMP_RANGE_START + 9, &player));
        assert_eq!(page(world), 2);
        assert!(menu.click_menu_button(BUTTON_PREV_PAGE, &player));
        assert_eq!(page(world), 1);
        assert!(!menu.click_menu_button(4, &player));
    }

    #[test]
    fn taking_the_book_empties_the_lectern() {
        let (fixture, player, mut menu) = test_lectern("lectern_menu_take");
        let world = &fixture.world;

        assert!(menu.click_menu_button(BUTTON_TAKE_BOOK, &player));
        let state = world.get_block_state(POS);
        assert!(!state.get_value(&BlockStateProperties::HAS_BOOK));
        assert!(!menu.still_valid(&player));
        assert_eq!(page(world), 0);
        assert_eq!(
            player
                .inventory
                .lock()
                .count_item(&vanilla_items::WRITABLE_BOOK),
            1
        );
    }
}
