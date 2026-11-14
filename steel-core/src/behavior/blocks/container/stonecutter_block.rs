//! Stonecutter block behavior.

use std::sync::Arc;

use crate::behavior::InventoryAccess;
use crate::behavior::block::BlockBehavior;
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::entity::ai::path::PathComputationType;
use crate::inventory::menu::kinds::stonecutter;
use crate::player::Player;
use crate::world::World;
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_custom_stats;
use steel_utils::{BlockPos, BlockStateId, translations};
use text_components::TextComponent;

/// Behavior for the stonecutter, Vanilla `StonecutterBlock`.
#[block_behavior]
pub struct StonecutterBlock {
    block: BlockRef,
}

impl StonecutterBlock {
    /// Creates a new stonecutter block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

impl BlockBehavior for StonecutterBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state().set_value(
            &BlockStateProperties::HORIZONTAL_FACING,
            context.horizontal_direction().opposite(),
        ))
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
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
            TextComponent::translated(translations::CONTAINER_STONECUTTER.msg()),
            move |context| stonecutter(inventory, context.container_id, pos, context.world),
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
        player.award_custom_stat(&vanilla_custom_stats::INTERACT_WITH_STONECUTTER);
        InteractionResult::Success
    }
}
