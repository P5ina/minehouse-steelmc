use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_math::{DEGREE_180, convert_to_rotation_segment};
use steel_registry::blocks::properties::{
    BlockStateProperties, Direction, EnumProperty, IntProperty,
};
use steel_registry::blocks::{BlockRef, block_state_ext::BlockStateExt};
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_blocks;
use steel_registry::{DyeColor, REGISTRY};
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use crate::behavior::{BlockBehavior, BlockEntityCreation, BlockPlaceContext};
use crate::block_entity::SharedBlockEntity;
use crate::block_entity::entities::BannerBlockEntity;
use crate::world::{LevelReader, ScheduledTickAccess, World};

const FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;

/// Shared behavior for standing banner blocks
#[block_behavior]
pub struct BannerBlock {
    block: BlockRef,
    #[json_arg(
        r#enum = "DyeColor",
        json = "color",
        module = "steel_registry::dye_color"
    )]
    color: DyeColor,
}

const ROTATION_16: &IntProperty = &BlockStateProperties::ROTATION_16;

impl BannerBlock {
    /// Creates a new banner block behavior
    #[must_use]
    pub const fn new(block: BlockRef, color: DyeColor) -> Self {
        Self { block, color }
    }
}

impl BlockBehavior for BannerBlock {
    fn is_possible_to_respawn_in_this(&self, _state: BlockStateId) -> bool {
        true
    }

    fn banner_color(&self) -> Option<DyeColor> {
        Some(self.color)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        new_banner_block_entity(level, pos, state, self.color)
    }

    fn get_clone_item_stack(
        &self,
        block: BlockRef,
        _state: BlockStateId,
        block_entity: Option<SharedBlockEntity>,
        _include_data: bool,
    ) -> Option<ItemStack> {
        Some(banner_clone_item_stack(block, block_entity))
    }

    fn can_survive(&self, _state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        world.get_block_state(pos.below()).is_solid()
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        if direction == Direction::Down && !self.can_survive(state, world, pos) {
            return REGISTRY.blocks.get_default_state_id(&vanilla_blocks::AIR);
        }
        state
    }

    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state().set_value(
            ROTATION_16,
            convert_to_rotation_segment(context.rotation() + DEGREE_180),
        ))
    }
}

/// Shared behavior for wall banner blocks
#[block_behavior]
pub struct WallBannerBlock {
    block: BlockRef,
    #[json_arg(
        r#enum = "DyeColor",
        json = "color",
        module = "steel_registry::dye_color"
    )]
    color: DyeColor,
}

impl WallBannerBlock {
    /// Creates a new wall banner block behavior
    #[must_use]
    pub const fn new(block: BlockRef, color: DyeColor) -> Self {
        Self { block, color }
    }
}

impl BlockBehavior for WallBannerBlock {
    fn is_possible_to_respawn_in_this(&self, _state: BlockStateId) -> bool {
        true
    }

    fn banner_color(&self) -> Option<DyeColor> {
        Some(self.color)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        new_banner_block_entity(level, pos, state, self.color)
    }

    fn get_clone_item_stack(
        &self,
        block: BlockRef,
        _state: BlockStateId,
        block_entity: Option<SharedBlockEntity>,
        _include_data: bool,
    ) -> Option<ItemStack> {
        Some(banner_clone_item_stack(block, block_entity))
    }

    fn can_survive(&self, state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        let facing = state.get_value(FACING);
        world
            .get_block_state(facing.opposite().relative(pos))
            .is_solid()
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        let facing = state.get_value(FACING);
        if direction == facing.opposite() && !self.can_survive(state, world, pos) {
            return REGISTRY.blocks.get_default_state_id(&vanilla_blocks::AIR);
        }
        state
    }

    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        for direction in context.get_nearest_looking_directions() {
            if !direction.is_horizontal() {
                continue;
            }

            let state = self
                .block
                .default_state()
                .set_value(FACING, direction.opposite());
            if self.can_survive(state, context.world.as_ref(), context.place_pos()) {
                return Some(state);
            }
        }

        None
    }
}

fn new_banner_block_entity(
    level: Weak<World>,
    pos: BlockPos,
    state: BlockStateId,
    color: DyeColor,
) -> BlockEntityCreation {
    BlockEntityCreation::Created(Arc::new(BannerBlockEntity::with_color(
        level, pos, state, color,
    )))
}

/// Vanilla `AbstractBannerBlock.getCloneItemStack`: the banner item keeps its patterns and name.
fn banner_clone_item_stack(block: BlockRef, block_entity: Option<SharedBlockEntity>) -> ItemStack {
    block_entity
        .as_deref()
        .and_then(|block_entity| block_entity.downcast_ref::<BannerBlockEntity>())
        .map_or_else(
            || ItemStack::new(REGISTRY.items.by_block(block)),
            BannerBlockEntity::item,
        )
}
