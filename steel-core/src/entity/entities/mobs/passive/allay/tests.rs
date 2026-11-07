use std::io::Cursor;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};
use steel_utils::BlockPos;

use super::{AllayEntity, DUPLICATION_COOLDOWN_TICKS};
use crate::entity::{Entity, EquipmentSlot, Mob};
use crate::inventory::container::Container as _;

fn test_allay() -> AllayEntity {
    init_vanilla_registry();
    AllayEntity::new(&vanilla_entities::ALLAY, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn duplication_cooldown_drives_can_duplicate() {
    let allay = test_allay();
    assert!(allay.can_duplicate());
    allay.reset_duplication_cooldown();
    assert!(!allay.can_duplicate());
    for _ in 0..DUPLICATION_COOLDOWN_TICKS {
        allay.update_duplication_cooldown();
    }
    assert!(allay.can_duplicate());
}

#[test]
fn jukebox_starts_and_stops_dancing() {
    let allay = test_allay();
    let jukebox = BlockPos::new(3, 64, 3);
    allay.set_jukebox_playing(jukebox, true);
    assert!(allay.is_dancing());
    // A different jukebox stopping does not stop the dance.
    allay.set_jukebox_playing(BlockPos::new(9, 64, 9), false);
    assert!(allay.is_dancing());
    allay.set_jukebox_playing(jukebox, false);
    assert!(!allay.is_dancing());
}

#[test]
fn items_are_equal_only_for_the_same_item() {
    init_vanilla_registry();
    let diamond = ItemStack::new(&vanilla_items::DIAMOND);
    let more_diamonds = ItemStack::with_count(&vanilla_items::DIAMOND, 5);
    let emerald = ItemStack::new(&vanilla_items::EMERALD);
    assert!(AllayEntity::allay_considers_item_equal(
        &diamond,
        &more_diamonds
    ));
    assert!(!AllayEntity::allay_considers_item_equal(&diamond, &emerald));
}

#[test]
fn empty_handed_allay_cannot_pick_up_loot() {
    let allay = test_allay();
    assert!(!allay.can_pick_up_loot());
    allay.set_item_slot(
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::DIAMOND),
    );
    assert!(allay.has_item_in_hand());
    assert!(allay.can_pick_up_loot());
}

#[test]
fn inventory_and_cooldown_round_trip() {
    let allay = test_allay();
    allay
        .inventory
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::DIAMOND, 7));
    allay.set_duplication_cooldown(1234);
    let mut nbt = NbtCompound::new();
    allay.save_additional(&mut nbt);

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let parsed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("saved allay NBT should parse: {error}"));

    let loaded = test_allay();
    loaded.load_additional((&parsed).into());
    assert_eq!(loaded.state.lock().duplication_cooldown, 1234);
    assert!(!loaded.can_duplicate());
    let item = loaded.inventory.lock().get_item(0).clone();
    assert_eq!(item.count(), 7);
}

#[test]
fn removing_inventory_items_empties_it() {
    let allay = test_allay();
    allay
        .inventory
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::DIAMOND, 3));
    assert!(!allay.inventory_is_empty());
    let one = allay.remove_one_from_inventory();
    assert_eq!(one.count(), 1);
    let rest = allay.remove_all_inventory_items();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].count(), 2);
    assert!(allay.inventory_is_empty());
}
