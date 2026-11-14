//! Lectern block behavior.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, BoolProperty, Direction};
use steel_registry::data_components::vanilla_components::BLOCK_ENTITY_DATA;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{sound_events, vanilla_block_entity_types, vanilla_custom_stats};
use steel_utils::{BlockPos, BlockStateId, Downcast as _, translations};
use text_components::TextComponent;

use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::blocks::redstone::{MAX_REDSTONE_SIGNAL, MIN_REDSTONE_SIGNAL};
use crate::behavior::{
    BlockHitResult, BlockPlaceContext, InteractionResult, InventoryAccess, PlacementSource,
};
use crate::block_entity::entities::{
    LecternBlockEntity, lectern_change_powered, lectern_reset_book_state, lectern_update_below,
};
use crate::block_entity::{BLOCK_ENTITIES, SharedBlockEntity};
use crate::entity::ai::path::PathComputationType;
use crate::inventory::menu::kinds::lectern;
use crate::player::Player;
use crate::world::{LevelReader, SignalQueryContext, World};
use steel_utils::types::InteractionHand;

const POWERED: &BoolProperty = &BlockStateProperties::POWERED;
const HAS_BOOK: &BoolProperty = &BlockStateProperties::HAS_BOOK;

/// Vanilla `Player.canUseGameMasterBlocks`.
fn can_use_game_master_blocks(player: &Player) -> bool {
    player.abilities.lock().instabuild && player.is_operator()
}

/// Vanilla `LecternBlock`.
#[block_behavior]
pub struct LecternBlock {
    block: BlockRef,
}

impl LecternBlock {
    /// Creates this behavior for its registered block.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn lectern(world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|entity| entity.get_type() == &vanilla_block_entity_types::LECTERN)
    }

    /// Vanilla `LecternBlock.placeBook`.
    fn place_book(
        player: &Player,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        inv: &mut InventoryAccess,
    ) {
        let Some(block_entity) = Self::lectern(world, pos) else {
            return;
        };
        let Some(lectern) = block_entity.downcast_ref::<LecternBlockEntity>() else {
            return;
        };
        let has_infinite_materials = player.has_infinite_materials();
        let book = inv.with_item(|item| item.consume_and_return(1, has_infinite_materials));
        lectern.set_book(book);
        lectern_reset_book_state(Some(player), world, pos, state, true);
        world.play_block_sound(&sound_events::ITEM_BOOK_PUT, pos, 1.0, 1.0, None);
    }
}

impl BlockBehavior for LecternBlock {
    /// Vanilla `LecternBlock.getStateForPlacement`.
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        let has_book = context.player().is_some_and(can_use_game_master_blocks)
            && context.with_item(|stack| {
                stack
                    .get(BLOCK_ENTITY_DATA)
                    .is_some_and(|data| data.data().as_compound().get("Book").is_some())
            });
        Some(
            self.block
                .default_state()
                .set_value(
                    &BlockStateProperties::HORIZONTAL_FACING,
                    context.horizontal_direction().opposite(),
                )
                .set_value(HAS_BOOK, has_book),
        )
    }

    /// Vanilla `BlockItem.updateCustomBlockEntityTag` for the operator-only lectern data.
    fn set_placed_by(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        source: &PlacementSource<'_>,
    ) {
        if !source.player().is_some_and(can_use_game_master_blocks) {
            return;
        }
        let data = source.with_item(|stack| {
            stack
                .get(BLOCK_ENTITY_DATA)
                .filter(|data| data.block_entity_type() == &vanilla_block_entity_types::LECTERN)
                .map(|data| data.data().copy_tag())
        });
        if let Some(data) = data
            && let Some(block_entity) = Self::lectern(world, pos)
            && let Some(lectern) = block_entity.downcast_ref::<LecternBlockEntity>()
        {
            lectern.apply_item_block_entity_data(&data);
        }
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::LECTERN,
            level,
            pos,
            state,
        ))
    }

    /// Vanilla `LecternBlock.tick`: ends the page-turn pulse.
    fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        lectern_change_powered(world, pos, state, false);
    }

    fn affect_neighbors_after_removal(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        if state.get_value(POWERED) {
            lectern_update_below(world, pos, state);
        }
    }

    fn is_signal_source(&self, _state: BlockStateId, _context: SignalQueryContext) -> bool {
        true
    }

    fn get_own_signal(
        &self,
        state: BlockStateId,
        _world: &dyn LevelReader,
        _pos: BlockPos,
        _context: SignalQueryContext,
    ) -> i32 {
        if state.get_value(POWERED) {
            MAX_REDSTONE_SIGNAL
        } else {
            MIN_REDSTONE_SIGNAL
        }
    }

    fn get_direct_signal(
        &self,
        state: BlockStateId,
        _world: &dyn LevelReader,
        _pos: BlockPos,
        direction: Direction,
        _context: SignalQueryContext,
    ) -> i32 {
        if direction == Direction::Up && state.get_value(POWERED) {
            MAX_REDSTONE_SIGNAL
        } else {
            MIN_REDSTONE_SIGNAL
        }
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        state: BlockStateId,
        world: &World,
        pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        if !state.get_value(HAS_BOOK) {
            return 0;
        }
        Self::lectern(world, pos)
            .and_then(|entity| {
                entity
                    .downcast_ref::<LecternBlockEntity>()
                    .map(LecternBlockEntity::redstone_signal)
            })
            .unwrap_or(0)
    }

    /// Vanilla `LecternBlock.useItemOn`.
    fn use_item_on(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if state.get_value(HAS_BOOK) {
            return InteractionResult::TryEmptyHandInteraction;
        }
        let (is_book, is_empty) = inv.with_item(|item| {
            (
                item.item().has_tag(&ItemTag::LECTERN_BOOKS),
                item.is_empty(),
            )
        });
        if is_book {
            Self::place_book(player, world, pos, state, inv);
            return InteractionResult::Success;
        }
        if is_empty && hand == InteractionHand::MainHand {
            InteractionResult::Pass
        } else {
            InteractionResult::TryEmptyHandInteraction
        }
    }

    /// Vanilla `LecternBlock.useWithoutItem`.
    fn use_without_item(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if !state.get_value(HAS_BOOK) {
            return InteractionResult::Consume;
        }
        if self.open_menu_provider(state, world, pos, player) {
            player.award_custom_stat(&vanilla_custom_stats::INTERACT_WITH_LECTERN);
        }
        InteractionResult::Success
    }

    /// Vanilla `LecternBlock.getMenuProvider`: no menu without a book.
    fn open_menu_provider(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> bool {
        if !state.get_value(HAS_BOOK) {
            return false;
        }
        let Some(block_entity) = Self::lectern(world, pos) else {
            return false;
        };
        let Some(lectern_entity) = block_entity.downcast_ref::<LecternBlockEntity>() else {
            return false;
        };
        let book_access = lectern_entity.book_access();
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_LECTERN.msg()),
            move |context| lectern(context.container_id, book_access),
        );
        true
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};

    use steel_registry::item_stack::ItemStack;

    use super::*;

    #[test]
    fn lectern_books_tag_accepts_writable_and_written_books() {
        init_vanilla_registry();
        assert!(
            ItemStack::new(&vanilla_items::WRITABLE_BOOK)
                .item()
                .has_tag(&ItemTag::LECTERN_BOOKS)
        );
        assert!(
            ItemStack::new(&vanilla_items::WRITTEN_BOOK)
                .item()
                .has_tag(&ItemTag::LECTERN_BOOKS)
        );
        assert!(
            !ItemStack::new(&vanilla_items::BOOK)
                .item()
                .has_tag(&ItemTag::LECTERN_BOOKS)
        );
        let state = vanilla_blocks::LECTERN.default_state();
        assert!(!state.get_value(HAS_BOOK) && !state.get_value(POWERED));
    }
}
