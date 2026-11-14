//! Cartography table block behavior.

use std::sync::Arc;

use crate::behavior::InventoryAccess;
use crate::behavior::block::BlockBehavior;
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::inventory::menu::kinds::cartography_table;
use crate::player::Player;
use crate::world::World;
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::vanilla_custom_stats;
use steel_utils::{BlockPos, BlockStateId, translations};
use text_components::TextComponent;

/// Behavior for the cartography table, Vanilla `CartographyTableBlock`.
#[block_behavior]
pub struct CartographyTableBlock {
    block: BlockRef,
}

impl CartographyTableBlock {
    /// Creates a new cartography table block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

impl BlockBehavior for CartographyTableBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn open_menu_provider(
        &self,
        _state: BlockStateId,
        _world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> bool {
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_CARTOGRAPHY_TABLE.msg()),
            move |context| cartography_table(inventory, context.container_id, pos, context.world),
        );
        true
    }

    fn use_without_item(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        self.open_menu_provider(state, world, pos, player);
        player.award_custom_stat(&vanilla_custom_stats::INTERACT_WITH_CARTOGRAPHY_TABLE);
        InteractionResult::Success
    }
}
