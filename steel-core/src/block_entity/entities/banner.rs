//! Banner block entity.

use std::sync::Weak;

use simdnbt::borrow::{
    BaseNbtCompound as BorrowedNbtCompound, NbtCompound as BorrowedNbtCompoundView,
};
use simdnbt::owned::NbtCompound;
use simdnbt::{FromNbtTag as _, ToNbtTag as _};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::DataComponentMap;
use steel_registry::data_components::components::BannerPatternLayers;
use steel_registry::data_components::vanilla_components::{BANNER_PATTERNS, CUSTOM_NAME};
use steel_registry::item_stack::ItemStack;
use steel_registry::{DyeColor, REGISTRY, vanilla_block_entity_types};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};
use text_components::TextComponent;

use crate::behavior::BLOCK_BEHAVIORS;
use crate::block_entity::{BlockEntity, BlockEntityBase, ImplicitComponentGetter};
use crate::world::World;

const PATTERNS_NBT_KEY: &str = "patterns";
const CUSTOM_NAME_NBT_KEY: &str = "CustomName";

#[derive(Default)]
struct BannerData {
    name: Option<TextComponent>,
    patterns: BannerPatternLayers,
}

/// Banner block entity: the base color of its block, the pattern layers and a custom name.
pub struct BannerBlockEntity {
    base: BlockEntityBase,
    base_color: DyeColor,
    data: SyncMutex<BannerData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `BannerBlockEntity`.
unsafe impl DowncastType for BannerBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/banner");
}

impl BannerBlockEntity {
    /// Creates a banner entity colored like its block.
    ///
    /// A state whose block is not a banner falls back to white, where vanilla would fail the cast.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let color = BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .banner_color()
            .unwrap_or(DyeColor::White);
        Self::with_color(level, pos, state, color)
    }

    /// Creates a banner entity with an explicit base color.
    #[must_use]
    pub fn with_color(
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
        base_color: DyeColor,
    ) -> Self {
        Self {
            base: BlockEntityBase::new(&vanilla_block_entity_types::BANNER, level, pos, state),
            base_color,
            data: SyncMutex::new(BannerData::default()),
        }
    }

    /// The color of the banner block, which patterns are drawn over.
    #[must_use]
    pub const fn base_color(&self) -> DyeColor {
        self.base_color
    }

    /// The custom name given by a renamed banner item.
    #[must_use]
    pub fn custom_name(&self) -> Option<TextComponent> {
        self.data.lock().name.clone()
    }

    /// The pattern layers drawn over the base color.
    #[must_use]
    pub fn patterns(&self) -> BannerPatternLayers {
        self.data.lock().patterns.clone()
    }

    /// The banner item with this banner's patterns and name.
    #[must_use]
    pub fn item(&self) -> ItemStack {
        let mut item = ItemStack::new(REGISTRY.items.by_block(self.get_block_state().get_block()));
        item.apply_components(&self.collect_components());
        item
    }
}

impl BlockEntity for BannerBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: BorrowedNbtCompoundView<'_, '_> = nbt.into();
        let mut data = self.data.lock();
        data.name = nbt
            .get(CUSTOM_NAME_NBT_KEY)
            .and_then(|tag| TextComponent::from_nbt(&tag.to_owned()));
        data.patterns = nbt
            .get(PATTERNS_NBT_KEY)
            .and_then(BannerPatternLayers::from_nbt_tag)
            .unwrap_or_default();
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let data = self.data.lock();
        if !data.patterns.layers().is_empty() {
            nbt.insert(PATTERNS_NBT_KEY, data.patterns.clone().to_nbt_tag());
        }
        if let Some(name) = &data.name {
            nbt.insert(CUSTOM_NAME_NBT_KEY, name.to_codec_nbt());
        }
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        Some(self.save_without_metadata())
    }

    fn apply_implicit_components(&self, components: &mut ImplicitComponentGetter<'_>) {
        let mut data = self.data.lock();
        data.patterns = components.get_or_default(BANNER_PATTERNS, BannerPatternLayers::empty());
        data.name = components.get(CUSTOM_NAME);
    }

    fn collect_implicit_components(&self, components: &mut DataComponentMap) {
        let data = self.data.lock();
        components.set(BANNER_PATTERNS, Some(data.patterns.clone()));
        components.set(CUSTOM_NAME, data.name.clone());
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::ChunkPos;
    use steel_utils::Downcast as _;
    use steel_utils::types::UpdateFlags;

    use super::*;
    use crate::behavior::{BlockLootContext, init_behaviors};
    use crate::entity::ominous_banner_instance;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
    use crate::world::LevelReader as _;

    #[test]
    fn a_broken_banner_drops_with_everything_it_was_placed_with() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("banner_drop_components");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let pos = BlockPos::new(2, 64, 2);
        let state = vanilla_blocks::WHITE_BANNER.default_state();
        assert!(world.set_block(pos, state, UpdateFlags::UPDATE_ALL));
        let block_entity = world.get_block_entity(pos).expect("banners have entities");
        let ominous = ominous_banner_instance();
        block_entity.apply_components_from_item_stack(&ominous);

        let banner = block_entity
            .downcast_ref::<BannerBlockEntity>()
            .expect("a banner entity");
        assert_eq!(banner.base_color(), DyeColor::White);
        assert_eq!(banner.patterns().layers().len(), 8);

        let drops = BlockLootContext::new(world, pos)
            .with_block_entity(Some(block_entity.as_ref()))
            .get_drops(state);
        assert_eq!(drops.len(), 1);
        assert!(ItemStack::is_same_item_same_components(&drops[0], &ominous));
        assert!(ItemStack::is_same_item_same_components(
            &banner.item(),
            &ominous
        ));
    }
}
