use std::io::Cursor;
use std::sync::Arc;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::{BEES, BLOCK_STATE};
use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
use steel_utils::BlockPos;

use super::{BEEHIVE_MAX_OCCUPANTS, BeehiveBlockEntity, LEVEL_HONEY};
use crate::block_entity::BlockEntity as _;
use crate::test_support::fresh_test_world;

fn new_hive(name: &'static str) -> (crate::test_support::TestWorld, BeehiveBlockEntity) {
    init_vanilla_registry();
    let fixture = fresh_test_world(name);
    let hive = BeehiveBlockEntity::new(
        Arc::downgrade(&fixture.world),
        BlockPos::new(4, 64, 4),
        vanilla_blocks::BEE_NEST.default_state(),
    );
    (fixture, hive)
}

#[test]
fn three_worldgen_bees_fill_the_hive() {
    let (_fixture, hive) = new_hive("beehive_worldgen_fill");
    assert!(hive.is_empty());
    for _ in 0..BEEHIVE_MAX_OCCUPANTS {
        assert!(!hive.is_full());
        hive.store_worldgen_bee(0);
    }
    assert_eq!(hive.occupant_count(), BEEHIVE_MAX_OCCUPANTS);
    assert!(hive.is_full());
    assert!(!hive.is_empty());
}

#[test]
fn bees_and_flower_round_trip_through_nbt() {
    let (_fixture, hive) = new_hive("beehive_nbt_round_trip");
    hive.store_worldgen_bee(100);
    hive.store_worldgen_bee(200);
    hive.state.lock().saved_flower_pos = Some(BlockPos::new(1, 2, 3));

    let mut nbt = NbtCompound::new();
    hive.save_additional(&mut nbt);
    assert_eq!(nbt.int_array("flower_pos"), Some(&[1, 2, 3][..]));
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));

    let (_other_fixture, loaded) = new_hive("beehive_nbt_round_trip_loaded");
    loaded.load_additional(&borrowed);
    assert_eq!(loaded.occupant_count(), 2);
    assert_eq!(
        loaded.state.lock().saved_flower_pos,
        Some(BlockPos::new(1, 2, 3))
    );
}

#[test]
fn hive_item_keeps_bees_and_honey_level() {
    let (_fixture, hive) = new_hive("beehive_as_item");
    hive.store_worldgen_bee(0);
    let state = vanilla_blocks::BEE_NEST
        .default_state()
        .set_value(LEVEL_HONEY, 3);
    let item = hive.beehive_as_item(state);
    assert!(item.is(&vanilla_items::BEE_NEST));
    assert_eq!(item.get(BEES).map(|bees| bees.bees().len()), Some(1));
    assert_eq!(
        item.get(BLOCK_STATE)
            .and_then(|properties| properties.get("honey_level")),
        Some("3")
    );
}
