//! Brewing stand block behavior.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::properties::Direction;
use steel_registry::{vanilla_block_entity_types, vanilla_custom_stats};
use steel_utils::{BlockPos, BlockStateId, translations};
use text_components::TextComponent;

use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::{BlockHitResult, BlockPlaceContext, InteractionResult, InventoryAccess};
use crate::block_entity::{BLOCK_ENTITIES, BlockEntityTicker};
use crate::entity::ai::path::PathComputationType;
use crate::inventory::container::calculate_redstone_signal_from_container;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::brewing_stand;
use crate::player::Player;
use crate::world::World;

/// Vanilla `BrewingStandBlock`.
#[block_behavior]
pub struct BrewingStandBlock {
    block: BlockRef,
}

impl BrewingStandBlock {
    /// Creates this behavior for its registered block.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

impl BlockBehavior for BrewingStandBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn open_menu_provider(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> bool {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return false;
        };
        if block_entity.get_type() != &vanilla_block_entity_types::BREWING_STAND {
            return false;
        }
        let Some(container) = ContainerRef::from_block_entity(block_entity) else {
            return false;
        };
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_BREWING.msg()),
            move |context| brewing_stand(inventory, context.container_id, container),
        );
        true
    }

    /// Vanilla `BrewingStandBlock.useWithoutItem`.
    fn use_without_item(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if self.open_menu_provider(state, world, pos, player) {
            player.award_custom_stat(&vanilla_custom_stats::INTERACT_WITH_BREWINGSTAND);
        }
        InteractionResult::Success
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::BREWING_STAND,
            level,
            pos,
            state,
        ))
    }

    fn get_block_entity_ticker(
        &self,
        _world: &Arc<World>,
        _state: BlockStateId,
        block_entity_type: BlockEntityTypeRef,
    ) -> Option<BlockEntityTicker> {
        BlockEntityTicker::for_matching_entity_tick(
            block_entity_type,
            &vanilla_block_entity_types::BREWING_STAND,
        )
    }

    /// Vanilla `Containers` neighbor update after the stand is destroyed.
    fn affect_neighbors_after_removal(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        _state: BlockStateId,
        world: &World,
        pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        let Some(container) = world
            .get_block_entity(pos)
            .and_then(ContainerRef::from_block_entity)
        else {
            return 0;
        };
        let guard = ContainerLockGuard::lock_all(&[&container]);
        guard
            .get(container.container_id())
            .map_or(0, calculate_redstone_signal_from_container)
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }
}
