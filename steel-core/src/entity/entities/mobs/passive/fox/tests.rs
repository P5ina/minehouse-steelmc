use std::sync::Weak;
use std::sync::atomic::Ordering;

use glam::DVec3;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::FoxVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};
use uuid::Uuid;

use super::{FoxEntity, int_array_to_uuid, uuid_to_int_array};
use crate::entity::{Animal, Entity, LivingEntity, Mob};

fn test_fox() -> FoxEntity {
    init_vanilla_registry();
    FoxEntity::new(&vanilla_entities::FOX, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn flags_drive_sleeping_crouching_and_movement() {
    let fox = test_fox();
    assert!(fox.can_move());
    fox.set_sleeping_flag(true);
    assert!(fox.is_sleeping());
    assert!(!fox.can_move());
    fox.set_is_crouching(true);
    assert!(fox.is_crouching());
    assert!(!fox.look_control_resets_x_rot());
    fox.clear_states();
    assert!(!fox.is_sleeping());
    assert!(!fox.is_crouching());
    assert!(fox.can_move());
    assert!(fox.look_control_resets_x_rot());
}

#[test]
fn defending_flag_uses_the_sign_bit() {
    let fox = test_fox();
    fox.set_defending(true);
    assert!(fox.is_defending());
    assert!(!fox.is_sitting());
    fox.set_defending(false);
    assert!(!fox.is_defending());
}

#[test]
fn second_trusted_slot_is_replaced() {
    let fox = test_fox();
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let third = Uuid::from_u128(3);
    fox.add_trusted(first);
    fox.add_trusted(second);
    fox.add_trusted(third);
    assert_eq!(fox.trusted(), vec![first, third]);
}

#[test]
fn uuid_int_array_round_trips() {
    let uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210);
    let ints = uuid_to_int_array(uuid);
    assert_eq!(
        int_array_to_uuid([ints[0], ints[1], ints[2], ints[3]]),
        uuid
    );
}

#[test]
fn saves_variant_and_trust() {
    let fox = test_fox();
    fox.set_variant(FoxVariant::Snow);
    fox.add_trusted(Uuid::from_u128(7));
    let mut nbt = NbtCompound::new();
    fox.save_additional(&mut nbt);
    assert_eq!(
        nbt.string("Type").map(ToString::to_string),
        Some("snow".to_owned())
    );
    assert!(nbt.list("Trusted").is_some());
}

#[test]
fn holds_one_item_and_prefers_food() {
    let fox = test_fox();
    assert!(fox.can_hold_item(&ItemStack::new(&vanilla_items::FEATHER)));
    fox.set_main_hand(ItemStack::new(&vanilla_items::FEATHER));
    assert!(!fox.can_hold_item(&ItemStack::new(&vanilla_items::LEATHER)));
    fox.ticks_since_eaten.store(1, Ordering::Relaxed);
    assert!(fox.can_hold_item(&ItemStack::new(&vanilla_items::SWEET_BERRIES)));
}

#[test]
fn sweet_berries_are_fox_food() {
    let fox = test_fox();
    assert!(fox.is_food(&ItemStack::new(&vanilla_items::SWEET_BERRIES)));
    assert!(!fox.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
    assert!(fox.can_pick_up_loot());
    assert!(LivingEntity::is_alive(&fox));
}
