//! Vanilla `Display.BlockDisplay`: renders one block state.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_blocks;
use steel_registry::vanilla_entity_data::BlockDisplayEntityData;
use steel_utils::BlockStateId;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::display::{load_display, save_display, tick_display};
use crate::block_entity::block_state_nbt;
use crate::entity::{Entity, EntityBase, EntityBaseLoad, EntitySyncedData};
use crate::world::World;

/// Vanilla `BlockDisplay.TAG_BLOCK_STATE`.
const TAG_BLOCK_STATE: &str = "block_state";

/// Vanilla `Display.BlockDisplay`.
#[entity_behavior(class = "BlockDisplay")]
pub struct BlockDisplayEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<BlockDisplayEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `BlockDisplayEntity`.
unsafe impl DowncastType for BlockDisplayEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/block_display");
}

impl BlockDisplayEntity {
    /// Creates a block display showing air.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(BlockDisplayEntityData::new()),
        }
    }

    /// Creates a block display from saved base data; `load_additional` follows.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(BlockDisplayEntityData::new()),
        }
    }

    /// Vanilla `BlockDisplay.getBlockState`.
    #[must_use]
    pub fn block_state(&self) -> BlockStateId {
        *self.entity_data.lock().block_state.get()
    }

    /// Vanilla `BlockDisplay.setBlockState`.
    pub fn set_block_state(&self, state: BlockStateId) {
        self.entity_data.lock().block_state.set(state);
    }
}

impl Entity for BlockDisplayEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn tick(self: Arc<Self>) {
        tick_display(&*self);
    }

    fn no_physics(&self) -> bool {
        true
    }

    fn piston_push_reaction(&self) -> PushReaction {
        PushReaction::IgnoreEntity
    }

    fn is_ignoring_block_triggers(&self) -> bool {
        true
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let data = self.entity_data.lock();
        save_display(&data.display, nbt);
        nbt.insert(
            TAG_BLOCK_STATE,
            block_state_nbt::save(*data.block_state.get()),
        );
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let mut data = self.entity_data.lock();
        load_display(&mut data.display, nbt);
        data.block_state.set(
            nbt.get(TAG_BLOCK_STATE)
                .and_then(block_state_nbt::load)
                .unwrap_or_else(|| vanilla_blocks::AIR.default_state()),
        );
    }
}

#[cfg(test)]
mod tests {
    use simdnbt::owned::NbtTag;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::blocks::properties::{BlockStateProperties, Direction};
    use steel_registry::vanilla_entities;
    use steel_registry::{init_vanilla_registry, vanilla_blocks};

    use super::super::display::tests::with_borrowed;
    use super::*;

    fn block_display() -> BlockDisplayEntity {
        BlockDisplayEntity::new(
            &vanilla_entities::BLOCK_DISPLAY,
            1,
            DVec3::ZERO,
            Weak::new(),
        )
    }

    #[test]
    fn block_state_saves_with_the_vanilla_codec_and_loads_back() {
        init_vanilla_registry();
        let display = block_display();
        let state = vanilla_blocks::FURNACE
            .default_state()
            .set_value(&BlockStateProperties::HORIZONTAL_FACING, Direction::East);
        display.set_block_state(state);

        let mut nbt = NbtCompound::new();
        display.save_additional(&mut nbt);
        assert!(matches!(nbt.get("block_state"), Some(NbtTag::Compound(_))));
        assert!(nbt.compound("transformation").is_some());

        let loaded = block_display();
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        assert_eq!(loaded.block_state(), state);
    }

    #[test]
    fn a_bare_block_id_loads_its_default_state_and_garbage_loads_air() {
        init_vanilla_registry();
        let mut nbt = NbtCompound::new();
        nbt.insert("block_state", "minecraft:stone");
        let loaded = block_display();
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        assert_eq!(loaded.block_state(), vanilla_blocks::STONE.default_state());

        let mut nbt = NbtCompound::new();
        nbt.insert("block_state", 7);
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        assert_eq!(loaded.block_state(), vanilla_blocks::AIR.default_state());
    }
}
