//! Dispenser and dropper block behaviors.

use std::slice;
use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, Direction};
use steel_registry::item_stack::ItemStack;
use steel_registry::stat::custom::CustomStatRef;
use steel_registry::{
    level_events, vanilla_block_entity_types, vanilla_custom_stats, vanilla_game_events,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, translations};
use text_components::TextComponent;
use text_components::translation::Translation;

use crate::behavior::InventoryAccess;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::behavior::dispense::{BlockSource, DispenseBehavior, default_dispense};
use crate::block_entity::entities::{DispenserBlockEntity, HopperBlockEntity, add_item};
use crate::block_entity::{BLOCK_ENTITIES, SharedBlockEntity};
use crate::inventory::container::calculate_redstone_signal_from_container;
use crate::inventory::lock::{ContainerId, ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::dispenser;
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{SignalGetter as _, World};

/// Ticks between a redstone pulse and the dispense.
const TRIGGER_DELAY: i32 = 4;

/// Dispenser block behavior.
#[block_behavior]
pub struct DispenserBlock {
    block: BlockRef,
}

/// Dropper block behavior: like a dispenser, but always drops or inserts one item.
#[block_behavior]
pub struct DropperBlock {
    block: BlockRef,
}

/// What differs between a dispenser and a dropper.
#[derive(Clone, Copy)]
struct Kind {
    block: BlockRef,
    entity_type: BlockEntityTypeRef,
    title: &'static Translation<0>,
    inspect_stat: CustomStatRef,
    dropper: bool,
}

impl Kind {
    fn placement(self, context: &BlockPlaceContext<'_>) -> BlockStateId {
        self.block.default_state().set_value(
            &BlockStateProperties::FACING,
            context.get_nearest_looking_direction().opposite(),
        )
    }

    /// Vanilla `DispenserBlock.neighborChanged`: the dispenser or the block above
    /// powered starts a dispense in 4 ticks.
    fn neighbor_changed(self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let powered = world.has_neighbor_signal(pos) || world.has_neighbor_signal(pos.above());
        let triggered = state.get_value(&BlockStateProperties::TRIGGERED);
        if powered && !triggered {
            world.schedule_block_tick_default(pos, self.block, TRIGGER_DELAY);
            world.set_block(
                pos,
                state.set_value(&BlockStateProperties::TRIGGERED, true),
                UpdateFlags::UPDATE_CLIENTS,
            );
        } else if !powered && triggered {
            world.set_block(
                pos,
                state.set_value(&BlockStateProperties::TRIGGERED, false),
                UpdateFlags::UPDATE_CLIENTS,
            );
        }
    }

    fn dispense_from(self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let Some(block_entity) = world.get_block_entity(pos) else {
            log::warn!("Ignoring dispensing attempt without a block entity at {pos:?}");
            return;
        };
        let Some(entity) = block_entity.downcast_ref::<DispenserBlockEntity>() else {
            log::warn!("Ignoring dispensing attempt without a dispenser block entity at {pos:?}");
            return;
        };
        let source = BlockSource {
            world,
            pos,
            state,
            entity,
        };
        let Some(slot) = entity.get_random_slot(|bound| rand::random_range(0..bound)) else {
            world.level_event(level_events::SOUND_DISPENSER_FAIL, pos, 0, None);
            if !self.dropper {
                world.game_event(
                    &vanilla_game_events::BLOCK_ACTIVATE,
                    pos,
                    &GameEventContext::new(None, Some(state)),
                );
            }
            return;
        };
        let stack = entity.get_item(slot);
        let rest = if self.dropper {
            drop_into_container_or_world(&source, &block_entity, stack)
        } else {
            DispenseBehavior::for_item(&stack).dispense(&source, stack)
        };
        entity.set_item(slot, rest);
    }

    /// Opens the dispenser menu (vanilla `getMenuProvider` + `openMenu`),
    /// returning whether the block entity provided one.
    fn open_menu(self, world: &World, pos: BlockPos, player: &Player) -> bool {
        let Some(block_entity) = world.get_block_entity(pos) else {
            return false;
        };
        let Some(entity) = block_entity.downcast_ref::<DispenserBlockEntity>() else {
            return false;
        };
        let Some(container) = block_entity.container_ref() else {
            return false;
        };
        if !ContainerRef::prepare_open(slice::from_ref(&container), player) {
            return false;
        }
        let title = entity
            .custom_name()
            .unwrap_or_else(|| TextComponent::translated(self.title.msg()));
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(title, move |context| {
            dispenser(inventory, context.container_id, container)
        });
        true
    }

    fn open(self, world: &World, pos: BlockPos, player: &Player) -> InteractionResult {
        if self.open_menu(world, pos, player) {
            player.award_custom_stat(self.inspect_stat);
        }
        InteractionResult::Success
    }

    fn new_block_entity(
        self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            self.entity_type,
            level,
            pos,
            state,
        ))
    }
}

/// Vanilla `DropperBlock.dispenseFrom`: insert one item into the container in
/// front, or drop it when there is none.
fn drop_into_container_or_world(
    source: &BlockSource<'_>,
    block_entity: &SharedBlockEntity,
    stack: ItemStack,
) -> ItemStack {
    if stack.is_empty() {
        return stack;
    }
    let facing = source.facing();
    let Some(targets) =
        HopperBlockEntity::get_container_at(source.world, source.pos.relative(facing))
    else {
        return default_dispense(source, stack);
    };
    let Some(own) = block_entity.container_ref() else {
        return stack;
    };
    let source_id = own.container_id();
    let target_ids: Vec<ContainerId> = targets.iter().map(ContainerRef::container_id).collect();
    let mut locked = vec![own];
    locked.extend(targets);
    let rest = {
        let mut guard = ContainerLockGuard::lock_all(&locked);
        add_item(
            &mut guard,
            Some(source_id),
            &target_ids,
            stack.copy_with_count(1),
            Some(facing.opposite()),
        )
    };
    let mut result = stack;
    if rest.is_empty() {
        result.shrink(1);
    }
    result
}

fn analog_output(world: &World, pos: BlockPos) -> i32 {
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

macro_rules! impl_dispenser_block {
    ($name:ident, $entity:ident, $title:ident, $stat:ident, $dropper:expr) => {
        impl $name {
            /// Creates the block behavior.
            #[must_use]
            pub const fn new(block: BlockRef) -> Self {
                Self { block }
            }

            fn kind(&self) -> Kind {
                Kind {
                    block: self.block,
                    entity_type: &vanilla_block_entity_types::$entity,
                    title: &translations::$title,
                    inspect_stat: &vanilla_custom_stats::$stat,
                    dropper: $dropper,
                }
            }
        }

        impl BlockBehavior for $name {
            fn get_state_for_placement(
                &self,
                context: &BlockPlaceContext<'_>,
            ) -> Option<BlockStateId> {
                Some(self.kind().placement(context))
            }

            fn handle_neighbor_changed(
                &self,
                state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                _source_block: BlockRef,
                _moved_by_piston: bool,
            ) {
                self.kind().neighbor_changed(state, world, pos);
            }

            fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
                self.kind().dispense_from(state, world, pos);
            }

            fn open_menu_provider(
                &self,
                _state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                player: &Player,
            ) -> bool {
                self.kind().open_menu(world, pos, player)
            }

            fn use_without_item(
                &self,
                _state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                player: &Player,
                _hit_result: &BlockHitResult,
                _inv: &mut InventoryAccess,
            ) -> InteractionResult {
                self.kind().open(world, pos, player)
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
                self.kind().new_block_entity(level, pos, state)
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
                analog_output(world, pos)
            }
        }
    };
}

impl_dispenser_block!(
    DispenserBlock,
    DISPENSER,
    CONTAINER_DISPENSER,
    INSPECT_DISPENSER,
    false
);
impl_dispenser_block!(
    DropperBlock,
    DROPPER,
    CONTAINER_DROPPER,
    INSPECT_DROPPER,
    true
);
