//! Vanilla `BlockState.CODEC`: a bare block id for a default state, otherwise
//! `BlockState.FULL_CODEC`'s `{id, properties}` compound.

use std::str::FromStr;

use simdnbt::borrow::{NbtCompound as BorrowedNbtCompound, NbtTag as BorrowedNbtTag};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::{Block, BlockRef};
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::{BlockStateId, Identifier};

/// Encodes `state` with vanilla `BlockState.CODEC`.
#[must_use]
pub(crate) fn save(state: BlockStateId) -> NbtTag {
    let block = state.get_block();
    if state == block.default_state() {
        return NbtTag::String(block.key.to_string().into());
    }
    NbtTag::Compound(save_full(state))
}

/// Decodes vanilla `BlockState.CODEC`: either a block id or the full state.
#[must_use]
pub(crate) fn load(encoded: BorrowedNbtTag<'_, '_>) -> Option<BlockStateId> {
    if let Some(name) = encoded.string() {
        return block_by_name(&name.to_str()).map(Block::default_state);
    }
    load_full(encoded.compound()?)
}

/// Encodes `state` with vanilla `BlockState.FULL_CODEC`.
#[must_use]
fn save_full(state: BlockStateId) -> NbtCompound {
    let mut encoded = NbtCompound::new();
    encoded.insert("id", state.get_block().key.to_string());

    let state_properties = REGISTRY.blocks.properties(state);
    if state_properties.len() != 0 {
        let mut properties = NbtCompound::new();
        for (name, value) in state_properties {
            properties.insert(name, value);
        }
        encoded.insert("properties", properties);
    }

    encoded
}

/// Decodes vanilla `BlockState.FULL_CODEC`.
fn load_full(encoded: BorrowedNbtCompound<'_, '_>) -> Option<BlockStateId> {
    let block = block_by_name(&encoded.string("id")?.to_str())?;

    let mut properties = Vec::new();
    if let Some(encoded_properties) = encoded.compound("properties") {
        for (name, value) in encoded_properties.iter() {
            let value = value.string()?;
            properties.push((name.to_str().into_owned(), value.to_str().into_owned()));
        }
    }

    REGISTRY.blocks.state_id_from_block_defaulted_properties(
        block,
        properties
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    )
}

fn block_by_name(name: &str) -> Option<BlockRef> {
    REGISTRY.blocks.by_key(&Identifier::from_str(name).ok()?)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound as read_borrowed_compound;
    use simdnbt::owned::NbtTag;
    use steel_registry::blocks::properties::{BlockStateProperties, Direction, PistonType};
    use steel_registry::init_vanilla_registry;
    use steel_registry::vanilla_blocks;

    use super::*;

    fn reborrow<T>(tag: NbtTag, check: impl FnOnce(BorrowedNbtTag<'_, '_>) -> T) -> T {
        let mut wrapper = NbtCompound::new();
        wrapper.insert("state", tag);
        let mut bytes = Vec::new();
        wrapper.write(&mut bytes);
        let borrowed = read_borrowed_compound(&mut Cursor::new(bytes.as_slice()))
            .expect("test NBT should reborrow");
        let view = BorrowedNbtCompound::from(&borrowed);
        check(view.get("state").expect("state tag should be present"))
    }

    #[test]
    fn propertyful_block_state_round_trips_vanilla_nbt_shape() {
        init_vanilla_registry();
        let state = vanilla_blocks::PISTON_HEAD
            .default_state()
            .set_value(&BlockStateProperties::FACING, Direction::East)
            .set_value(&BlockStateProperties::PISTON_TYPE, PistonType::Sticky)
            .set_value(&BlockStateProperties::SHORT, true);

        let NbtTag::Compound(encoded) = save(state) else {
            panic!("a non-default state should encode as the full compound");
        };
        assert_eq!(
            encoded.string("id").map(|name| name.to_str()),
            Some("minecraft:piston_head".into())
        );
        assert!(matches!(
            encoded.get("properties"),
            Some(NbtTag::Compound(_))
        ));

        assert_eq!(reborrow(NbtTag::Compound(encoded), load), Some(state));
    }

    #[test]
    fn default_state_encodes_as_bare_block_id_and_both_shapes_decode() {
        init_vanilla_registry();
        let state = vanilla_blocks::SAND.default_state();

        let encoded = save(state);
        assert!(
            matches!(&encoded, NbtTag::String(name) if name.to_str() == "minecraft:sand"),
            "vanilla writes a default state as its block id"
        );
        assert_eq!(reborrow(encoded, load), Some(state));

        let mut full = NbtCompound::new();
        full.insert("id", "minecraft:sand");
        assert_eq!(reborrow(NbtTag::Compound(full), load), Some(state));
        assert_eq!(reborrow(NbtTag::String("not a block".into()), load), None);
    }
}
