mod base_rail_block;
mod detector_rail_block;
mod powered_rail_block;
mod rail_block;
mod rail_state;

/// Vanilla `BaseRailBlock.isRail(BlockState)`.
#[must_use]
pub(crate) fn is_rail_state(state: steel_utils::BlockStateId) -> bool {
    base_rail_block::BaseRailBlock::is_rail_state(state)
}

pub use detector_rail_block::*;
pub use powered_rail_block::*;
pub use rail_block::*;
