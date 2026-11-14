//! Banner markers placed on a map by using it on a banner.

use simdnbt::borrow::NbtCompound as BorrowedNbtCompound;
use simdnbt::owned::{NbtCompound, NbtTag};
use simdnbt::{FromNbtTag as _, ToNbtTag as _};
use steel_registry::map_decoration_type::MapDecorationType;
use steel_registry::{DyeColor, vanilla_map_decoration_types as types};
use steel_utils::{BlockPos, Downcast as _};
use text_components::TextComponent;

use crate::block_entity::entities::BannerBlockEntity;
use crate::world::LevelReader;

/// Vanilla `MapBanner`: a banner marked on a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapBanner {
    /// The banner block.
    pub pos: BlockPos,
    /// The base color of the banner.
    pub color: DyeColor,
    /// The custom name of the banner, shown under its marker.
    pub name: Option<TextComponent>,
}

impl MapBanner {
    /// The banner at `pos`, or `None` when no banner stands there.
    #[must_use]
    pub fn from_world(world: &dyn LevelReader, pos: BlockPos) -> Option<Self> {
        let block_entity = world.get_block_entity(pos)?;
        let banner = block_entity.downcast_ref::<BannerBlockEntity>()?;
        Some(Self {
            pos,
            color: banner.base_color(),
            name: banner.custom_name(),
        })
    }

    /// The marker shown for a banner of this color.
    #[must_use]
    pub const fn decoration(&self) -> &'static MapDecorationType {
        match self.color {
            DyeColor::White => &types::BANNER_WHITE,
            DyeColor::Orange => &types::BANNER_ORANGE,
            DyeColor::Magenta => &types::BANNER_MAGENTA,
            DyeColor::LightBlue => &types::BANNER_LIGHT_BLUE,
            DyeColor::Yellow => &types::BANNER_YELLOW,
            DyeColor::Lime => &types::BANNER_LIME,
            DyeColor::Pink => &types::BANNER_PINK,
            DyeColor::Gray => &types::BANNER_GRAY,
            DyeColor::LightGray => &types::BANNER_LIGHT_GRAY,
            DyeColor::Cyan => &types::BANNER_CYAN,
            DyeColor::Purple => &types::BANNER_PURPLE,
            DyeColor::Blue => &types::BANNER_BLUE,
            DyeColor::Brown => &types::BANNER_BROWN,
            DyeColor::Green => &types::BANNER_GREEN,
            DyeColor::Red => &types::BANNER_RED,
            DyeColor::Black => &types::BANNER_BLACK,
        }
    }

    /// The decoration key of this marker.
    #[must_use]
    pub fn id(&self) -> String {
        format!("banner-{},{},{}", self.pos.x(), self.pos.y(), self.pos.z())
    }

    /// Encodes vanilla `MapBanner.CODEC`.
    #[must_use]
    pub fn to_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert(
            "pos",
            NbtTag::IntArray(vec![self.pos.x(), self.pos.y(), self.pos.z()]),
        );
        nbt.insert("color", self.color.to_nbt_tag());
        if let Some(name) = &self.name {
            nbt.insert("name", name.to_codec_nbt());
        }
        nbt
    }

    /// Decodes vanilla `MapBanner.CODEC`; the color and name are read leniently.
    #[must_use]
    pub fn from_nbt(nbt: BorrowedNbtCompound<'_, '_>) -> Option<Self> {
        let pos = nbt.int_array("pos")?;
        let [x, y, z] = pos[..] else {
            return None;
        };
        Some(Self {
            pos: BlockPos::new(x, y, z),
            color: nbt
                .get("color")
                .and_then(DyeColor::from_nbt_tag)
                .unwrap_or(DyeColor::White),
            name: nbt
                .get("name")
                .and_then(|tag| TextComponent::from_nbt(&tag.to_owned())),
        })
    }
}
