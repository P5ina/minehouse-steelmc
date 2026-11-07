use std::io::Cursor;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, DoubleBlockHalf};
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items};
use steel_utils::BlockPos;

use super::goals::BeeGrowCropGoal;
use super::{BeeEntity, attracts_bees};
use crate::entity::{Animal as _, Entity as _, LivingEntity as _};

fn test_bee() -> BeeEntity {
    init_vanilla_registry();
    BeeEntity::new(&vanilla_entities::BEE, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn flags_are_independent() {
    let bee = test_bee();
    bee.set_has_nectar(true);
    assert!(bee.has_nectar());
    assert!(!bee.has_stung());
    bee.set_has_stung(true);
    bee.set_rolling(true);
    assert!(bee.has_nectar() && bee.has_stung() && bee.is_rolling());
    bee.set_has_nectar(false);
    assert!(!bee.has_nectar());
    assert!(bee.has_stung() && bee.is_rolling());
}

#[test]
fn hive_and_flower_round_trip_through_nbt() {
    let bee = test_bee();
    bee.set_hive_pos(BlockPos::new(1, 64, 2));
    bee.set_saved_flower_pos(BlockPos::new(3, 65, 4));
    bee.set_has_nectar(true);
    bee.increment_crops_grown_since_pollination();

    let mut nbt = NbtCompound::new();
    bee.save_additional(&mut nbt);
    assert_eq!(nbt.int_array("hive_pos"), Some(&[1, 64, 2][..]));
    assert_eq!(nbt.int_array("flower_pos"), Some(&[3, 65, 4][..]));
    assert_eq!(nbt.byte("HasNectar"), Some(1));
    assert_eq!(nbt.int("CropsGrownSincePollination"), Some(1));
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));

    let loaded = test_bee();
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.hive_pos(), Some(BlockPos::new(1, 64, 2)));
    assert_eq!(loaded.saved_flower_pos(), Some(BlockPos::new(3, 65, 4)));
    assert!(loaded.has_nectar());
    assert_eq!(loaded.crops_grown_since_pollination(), 1);
}

#[test]
fn drop_off_nectar_clears_nectar_and_crops() {
    let bee = test_bee();
    bee.set_has_nectar(true);
    bee.increment_crops_grown_since_pollination();
    bee.drop_off_nectar();
    assert!(!bee.has_nectar());
    assert_eq!(bee.crops_grown_since_pollination(), 0);
}

#[test]
fn only_dry_flowers_and_sunflower_tops_attract_bees() {
    init_vanilla_registry();
    assert!(attracts_bees(vanilla_blocks::POPPY.default_state()));
    assert!(!attracts_bees(vanilla_blocks::STONE.default_state()));
    let sunflower = vanilla_blocks::SUNFLOWER.default_state();
    let top = sunflower.set_value(
        &BlockStateProperties::DOUBLE_BLOCK_HALF,
        DoubleBlockHalf::Upper,
    );
    let bottom = sunflower.set_value(
        &BlockStateProperties::DOUBLE_BLOCK_HALF,
        DoubleBlockHalf::Lower,
    );
    assert!(attracts_bees(top));
    assert!(!attracts_bees(bottom));
}

#[test]
fn crops_grow_one_stage_and_stop_at_max() {
    init_vanilla_registry();
    let age_7 = &BlockStateProperties::AGE_7;
    let wheat = vanilla_blocks::WHEAT.default_state();
    let grown =
        BeeGrowCropGoal::grown_state(wheat).unwrap_or_else(|| panic!("young wheat should grow"));
    assert_eq!(grown.get_value(age_7), 1);
    assert!(BeeGrowCropGoal::grown_state(wheat.set_value(age_7, 7)).is_none());
    let torchflower = BeeGrowCropGoal::grown_state(
        vanilla_blocks::TORCHFLOWER_CROP
            .default_state()
            .set_value(&BlockStateProperties::AGE_1, 1),
    );
    assert_eq!(
        torchflower.map(|state| state.get_block()),
        Some(&vanilla_blocks::TORCHFLOWER)
    );
    assert!(BeeGrowCropGoal::grown_state(vanilla_blocks::POPPY.default_state()).is_none());
}

#[test]
fn flowers_are_food_and_effects_come_from_wither_rose_and_eyeblossom() {
    let bee = test_bee();
    assert!(bee.is_food(&ItemStack::new(&vanilla_items::POPPY)));
    assert!(!bee.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
    assert!(
        BeeEntity::bee_interaction_effect(&ItemStack::new(&vanilla_items::WITHER_ROSE)).is_some()
    );
    assert!(
        BeeEntity::bee_interaction_effect(&ItemStack::new(&vanilla_items::OPEN_EYEBLOSSOM))
            .is_some()
    );
    assert!(BeeEntity::bee_interaction_effect(&ItemStack::new(&vanilla_items::POPPY)).is_none());
}

#[test]
fn bee_has_vanilla_health_volume_and_flight() {
    let bee = test_bee();
    assert!((bee.get_health() - 10.0).abs() < f32::EPSILON);
    assert!((bee.sound_volume() - 0.4).abs() < f32::EPSILON);
    assert!(bee.omnidirectional_air_mover());
}
