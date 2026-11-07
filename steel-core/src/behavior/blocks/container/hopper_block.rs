//! Hopper block behavior.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{Axis, BlockStateProperties};
use steel_registry::{vanilla_block_entity_types, vanilla_custom_stats};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Direction, Downcast as _, translations};
use text_components::TextComponent;

use crate::behavior::InventoryAccess;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::entities::HopperBlockEntity;
use crate::block_entity::{BLOCK_ENTITIES, BlockEntityTicker};
use crate::entity::Entity;
use crate::entity::InsideBlockEffectCollector;
use crate::entity::entities::ItemEntity;
use crate::inventory::container::calculate_redstone_signal_from_container;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::hopper;
use crate::player::Player;
use crate::world::{SignalGetter as _, World};

/// Hopper block behavior.
#[block_behavior]
pub struct HopperBlock {
    block: BlockRef,
}

impl HopperBlock {
    /// Creates a new hopper block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn check_powered_state(state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let enabled = !world.has_neighbor_signal(pos);
        if enabled != state.get_value(&BlockStateProperties::ENABLED) {
            world.set_block(
                pos,
                state.set_value(&BlockStateProperties::ENABLED, enabled),
                UpdateFlags::UPDATE_CLIENTS,
            );
        }
    }
}

impl BlockBehavior for HopperBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        let direction = context.clicked_face().opposite();
        let facing = if direction.axis() == Axis::Y {
            Direction::Down
        } else {
            direction
        };
        Some(
            self.block
                .default_state()
                .set_value(&BlockStateProperties::FACING_HOPPER, facing)
                .set_value(&BlockStateProperties::ENABLED, true),
        )
    }

    fn on_place(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        if old_state.get_block() != state.get_block() {
            Self::check_powered_state(state, world, pos);
        }
    }

    fn handle_neighbor_changed(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _source_block: BlockRef,
        _moved_by_piston: bool,
    ) {
        Self::check_powered_state(state, world, pos);
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
        let Some(hopper_entity) = block_entity.downcast_ref::<HopperBlockEntity>() else {
            return false;
        };
        let Some(container) = block_entity.container_ref() else {
            return false;
        };
        let title = hopper_entity
            .custom_name()
            .unwrap_or_else(|| TextComponent::translated(translations::CONTAINER_HOPPER.msg()));
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(title, move |context| {
            hopper(inventory, context.container_id, container)
        });
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
        if self.open_menu_provider(state, world, pos, player) {
            player.award_custom_stat(&vanilla_custom_stats::INSPECT_HOPPER);
        }
        InteractionResult::Success
    }

    fn entity_inside(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        entity: &dyn Entity,
        _effect_collector: &mut InsideBlockEffectCollector,
        _is_precise: bool,
    ) {
        let Some(item) = entity.downcast_ref::<ItemEntity>() else {
            return;
        };
        let Some(block_entity) = world.get_block_entity(pos) else {
            return;
        };
        if let Some(hopper_entity) = block_entity.downcast_ref::<HopperBlockEntity>() {
            hopper_entity.entity_inside(world, item);
        }
    }

    fn affect_neighbors_after_removal(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::HOPPER,
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
            &vanilla_block_entity_types::HOPPER,
        )
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
        guard.get(container.container_id()).map_or(0, |container| {
            calculate_redstone_signal_from_container(container)
        })
    }
}
