//! Vanilla `BannerItem`: a standing-and-wall block item for banners.

use steel_macros::item_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::properties::Direction;

use crate::behavior::context::{InteractionResult, UseOnContext};
use crate::behavior::{BlockItem, ItemBehavior, StandingAndWallBlockItem};

/// Vanilla `BannerItem`, which places the standing banner when looking down
/// and the wall banner otherwise.
#[item_behavior]
pub struct BannerItem {
    /// The standing banner block.
    #[json_arg(vanilla_blocks, json = "block")]
    pub standing_block: BlockRef,
    /// The wall banner block.
    #[json_arg(vanilla_blocks, json = "wall_block")]
    pub wall_block: BlockRef,

    base: StandingAndWallBlockItem,
}

impl BannerItem {
    /// Creates a new banner item behavior.
    #[must_use]
    pub const fn new(standing_block: BlockRef, wall_block: BlockRef) -> Self {
        Self {
            standing_block,
            wall_block,
            base: StandingAndWallBlockItem::new(standing_block, wall_block, Direction::Down),
        }
    }
}

impl ItemBehavior for BannerItem {
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        self.base.use_on(context)
    }

    fn as_block_item(&self) -> Option<&BlockItem> {
        Some(&self.base.base)
    }

    fn as_banner_item(&self) -> Option<&BannerItem> {
        Some(self)
    }
}
