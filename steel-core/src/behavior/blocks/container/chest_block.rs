//! Chest and trapped chest behavior.
//!
//! Two chests facing the same way side by side join into a double chest. Each half
//! keeps its own block entity; menus, comparators and hoppers see the halves as one
//! container, `RIGHT` half first.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, ChestType};
use steel_registry::stat::custom::CustomStatRef;
use steel_registry::{vanilla_block_entity_types, vanilla_blocks, vanilla_custom_stats};
use steel_utils::{BlockPos, BlockStateId, Direction, Downcast as _, translations};
use text_components::TextComponent;

use crate::behavior::InventoryAccess;
use crate::behavior::block::{
    BlockBehavior, BlockEntityCreation, schedule_water_tick_if_waterlogged,
};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::entities::ChestBlockEntity;
use crate::block_entity::{BLOCK_ENTITIES, SharedBlockEntity};
use crate::inventory::container::calculate_redstone_signal_from_containers;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::chest_halves;
use crate::player::Player;
use crate::world::{LevelReader, ScheduledTickAccess, World, is_redstone_conductor};

/// The block entities a chest resolves to.
pub enum ChestCombineResult {
    /// The chest is blocked or has no block entity.
    None,
    /// A chest without a usable partner.
    Single(SharedBlockEntity),
    /// Both halves of a double chest, `RIGHT` half first.
    Double(SharedBlockEntity, SharedBlockEntity),
}

impl ChestCombineResult {
    /// The containers in the order Vanilla's `CompoundContainer` addresses them.
    #[must_use]
    pub fn containers(&self) -> Vec<ContainerRef> {
        match self {
            Self::None => Vec::new(),
            Self::Single(chest) => chest.container_ref().into_iter().collect(),
            Self::Double(first, second) => match (first.container_ref(), second.container_ref()) {
                (Some(first), Some(second)) => vec![first, second],
                _ => Vec::new(),
            },
        }
    }
}

/// Shared chest logic for [`ChestBlock`] and [`TrappedChestBlock`].
struct ChestBehavior {
    block: BlockRef,
    block_entity_type: BlockEntityTypeRef,
    open_stat: CustomStatRef,
}

impl ChestBehavior {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> BlockStateId {
        let mut chest_type = ChestType::Single;
        let mut facing = context.horizontal_direction().opposite();
        let secondary_use = context.is_secondary_use_active();
        let clicked_face = context.clicked_face();
        let world = context.world;
        let pos = context.place_pos();

        if clicked_face.is_horizontal()
            && secondary_use
            && let Some(partner_facing) =
                self.candidate_partner_facing(world.as_ref(), pos, clicked_face.opposite())
            && partner_facing.axis() != clicked_face.axis()
        {
            facing = partner_facing;
            chest_type = if facing.rotate_y_counter_clockwise() == clicked_face.opposite() {
                ChestType::Right
            } else {
                ChestType::Left
            };
        }

        if chest_type == ChestType::Single && !secondary_use {
            chest_type = self.chest_type(world.as_ref(), pos, facing);
        }

        self.block
            .default_state()
            .set_value(&BlockStateProperties::FACING, facing)
            .set_value(&BlockStateProperties::CHEST_TYPE, chest_type)
            .set_value(
                &BlockStateProperties::WATERLOGGED,
                context.is_water_source(),
            )
    }

    fn chest_type(&self, world: &dyn LevelReader, pos: BlockPos, facing: Direction) -> ChestType {
        if self.candidate_partner_facing(world, pos, facing.rotate_y_clockwise()) == Some(facing) {
            ChestType::Left
        } else if self.candidate_partner_facing(world, pos, facing.rotate_y_counter_clockwise())
            == Some(facing)
        {
            ChestType::Right
        } else {
            ChestType::Single
        }
    }

    /// The facing of a single chest of this kind next to `pos`, if there is one.
    fn candidate_partner_facing(
        &self,
        world: &dyn LevelReader,
        pos: BlockPos,
        direction: Direction,
    ) -> Option<Direction> {
        let state = world.get_block_state(pos.relative(direction));
        (self.chest_can_connect_to(state)
            && state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Single)
            .then(|| state.get_value(&BlockStateProperties::FACING))
    }

    fn chest_can_connect_to(&self, state: BlockStateId) -> bool {
        state.get_block() == self.block
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        neighbor_state: BlockStateId,
    ) -> BlockStateId {
        schedule_water_tick_if_waterlogged(state, world, pos);
        if self.chest_can_connect_to(neighbor_state) && direction.is_horizontal() {
            let neighbor_type = neighbor_state.get_value(&BlockStateProperties::CHEST_TYPE);
            if state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Single
                && neighbor_type != ChestType::Single
                && state.get_value(&BlockStateProperties::FACING)
                    == neighbor_state.get_value(&BlockStateProperties::FACING)
                && connected_direction(neighbor_state) == direction.opposite()
            {
                let chest_type = match neighbor_type {
                    ChestType::Left => ChestType::Right,
                    ChestType::Right | ChestType::Single => ChestType::Left,
                };
                return state.set_value(&BlockStateProperties::CHEST_TYPE, chest_type);
            }
        } else if connected_direction(state) == direction {
            return state.set_value(&BlockStateProperties::CHEST_TYPE, ChestType::Single);
        }
        state
    }

    /// Resolves this chest and its partner. Vanilla `ChestBlock.combine`.
    fn combine(
        &self,
        state: BlockStateId,
        world: &World,
        pos: BlockPos,
        ignore_blocked: bool,
    ) -> ChestCombineResult {
        let is_blocked = |pos| !ignore_blocked && is_chest_blocked_at(world, pos);
        let Some(chest) = self.chest_block_entity(world, pos) else {
            return ChestCombineResult::None;
        };
        if is_blocked(pos) {
            return ChestCombineResult::None;
        }
        let chest_type = state.get_value(&BlockStateProperties::CHEST_TYPE);
        if chest_type == ChestType::Single {
            return ChestCombineResult::Single(chest);
        }

        let neighbor_pos = pos.relative(connected_direction(state));
        let neighbor_state = world.get_block_state(neighbor_pos);
        if neighbor_state.get_block() == state.get_block() {
            let neighbor_type = neighbor_state.get_value(&BlockStateProperties::CHEST_TYPE);
            if neighbor_type != ChestType::Single
                && neighbor_type != chest_type
                && neighbor_state.get_value(&BlockStateProperties::FACING)
                    == state.get_value(&BlockStateProperties::FACING)
            {
                if is_blocked(neighbor_pos) {
                    return ChestCombineResult::None;
                }
                if let Some(neighbor) = self.chest_block_entity(world, neighbor_pos) {
                    return if chest_type == ChestType::Right {
                        ChestCombineResult::Double(chest, neighbor)
                    } else {
                        ChestCombineResult::Double(neighbor, chest)
                    };
                }
            }
        }
        ChestCombineResult::Single(chest)
    }

    fn chest_block_entity(&self, world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|entity| entity.get_type() == self.block_entity_type)
    }

    fn use_without_item(&self, state: BlockStateId, world: &World, pos: BlockPos, player: &Player) {
        let combined = self.combine(state, world, pos, false);
        let (title, rows) = match &combined {
            ChestCombineResult::None => return,
            ChestCombineResult::Single(chest) => (
                custom_name(chest).unwrap_or_else(|| {
                    TextComponent::translated(translations::CONTAINER_CHEST.msg())
                }),
                3,
            ),
            ChestCombineResult::Double(first, second) => (
                custom_name(first)
                    .or_else(|| custom_name(second))
                    .unwrap_or_else(|| {
                        TextComponent::translated(translations::CONTAINER_CHEST_DOUBLE.msg())
                    }),
                6,
            ),
        };
        let containers = combined.containers();
        if containers.is_empty() {
            return;
        }

        let inventory = Arc::clone(&player.inventory);
        player.open_menu(title, move |context| {
            chest_halves(inventory, context.container_id, &containers, rows)
        });
        player.award_custom_stat(self.open_stat);
        // TODO: Anger nearby piglins (PiglinAi.angerNearbyPiglins)
        // TODO: Count viewers (ContainerOpenersCounter) for the lid, sounds and trapped chest power.
    }

    fn get_analog_output_signal(&self, state: BlockStateId, world: &World, pos: BlockPos) -> i32 {
        let containers = self.combine(state, world, pos, false).containers();
        if containers.is_empty() {
            return 0;
        }
        let guard = ContainerLockGuard::lock_all(&containers);
        let locked: Vec<_> = containers
            .iter()
            .filter_map(|container| guard.get(container.container_id()))
            .collect();
        calculate_redstone_signal_from_containers(&locked)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            self.block_entity_type,
            level,
            pos,
            state,
        ))
    }
}

/// The direction from a chest half to its partner.
#[must_use]
pub fn connected_direction(state: BlockStateId) -> Direction {
    let facing: Direction = state.get_value(&BlockStateProperties::FACING);
    if state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Left {
        facing.rotate_y_clockwise()
    } else {
        facing.rotate_y_counter_clockwise()
    }
}

/// Whether a chest at `pos` cannot be opened.
#[must_use]
pub fn is_chest_blocked_at(world: &World, pos: BlockPos) -> bool {
    let above = pos.above();
    // TODO: Cats sitting on the chest block it too (isCatSittingOnChest); Steel has no cats yet.
    is_redstone_conductor(world, world.get_block_state(above), above)
}

fn custom_name(chest: &SharedBlockEntity) -> Option<TextComponent> {
    chest
        .downcast_ref::<ChestBlockEntity>()
        .and_then(ChestBlockEntity::custom_name)
}

#[block_behavior]
/// Chest block behavior.
pub struct ChestBlock {
    block: BlockRef,
}

#[block_behavior]
/// Trapped chest block behavior. Its redstone output needs viewer counting, which Steel lacks.
pub struct TrappedChestBlock {
    block: BlockRef,
}

macro_rules! impl_chest_block {
    ($name:ident, $entity_type:ident, $stat:ident) => {
        impl $name {
            /// Creates the chest behavior for `block`.
            #[must_use]
            pub const fn new(block: BlockRef) -> Self {
                Self { block }
            }

            const fn chest(&self) -> ChestBehavior {
                ChestBehavior {
                    block: self.block,
                    block_entity_type: &vanilla_block_entity_types::$entity_type,
                    open_stat: &vanilla_custom_stats::$stat,
                }
            }

            /// Resolves this chest and its partner, Vanilla `ChestBlock.combine`.
            #[must_use]
            pub fn combine(
                &self,
                state: BlockStateId,
                world: &World,
                pos: BlockPos,
                ignore_blocked: bool,
            ) -> ChestCombineResult {
                self.chest().combine(state, world, pos, ignore_blocked)
            }
        }

        impl BlockBehavior for $name {
            fn get_state_for_placement(
                &self,
                context: &BlockPlaceContext<'_>,
            ) -> Option<BlockStateId> {
                Some(self.chest().get_state_for_placement(context))
            }

            fn update_shape(
                &self,
                state: BlockStateId,
                world: &dyn ScheduledTickAccess,
                pos: BlockPos,
                direction: Direction,
                _neighbor_pos: BlockPos,
                neighbor_state: BlockStateId,
            ) -> BlockStateId {
                self.chest()
                    .update_shape(state, world, pos, direction, neighbor_state)
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
                self.chest().use_without_item(state, world, pos, player);
                InteractionResult::Success
            }

            fn affect_neighbors_after_removal(
                &self,
                _state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                _moved_by_piston: bool,
            ) {
                world.update_neighbor_for_output_signal(pos, self.chest().block);
            }

            fn new_block_entity(
                &self,
                level: Weak<World>,
                pos: BlockPos,
                state: BlockStateId,
            ) -> BlockEntityCreation {
                self.chest().new_block_entity(level, pos, state)
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
                self.chest().get_analog_output_signal(state, world, pos)
            }
        }
    };
}

impl_chest_block!(ChestBlock, CHEST, OPEN_CHEST);
impl_chest_block!(TrappedChestBlock, TRAPPED_CHEST, TRIGGER_TRAPPED_CHEST);

/// The containers of the chest at `pos`, `RIGHT` half first, or `None` if the
/// block is not a chest. Vanilla `ChestBlock.getContainer`.
#[must_use]
pub fn chest_container(
    state: BlockStateId,
    world: &World,
    pos: BlockPos,
    ignore_blocked: bool,
) -> Option<Vec<ContainerRef>> {
    let block = state.get_block();
    let chest = if block == &vanilla_blocks::CHEST {
        ChestBlock::new(block).chest()
    } else if block == &vanilla_blocks::TRAPPED_CHEST {
        TrappedChestBlock::new(block).chest()
    } else {
        return None;
    };
    Some(
        chest
            .combine(state, world, pos, ignore_blocked)
            .containers(),
    )
}
