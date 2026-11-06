use std::sync::Weak;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::{DyeColor, init_vanilla_registry, vanilla_entities, vanilla_items};
use uuid::Uuid;

use super::{ArmorCrackiness, WolfEntity, armor_crackiness};
use crate::entity::neutral_mob::NeutralMob;
use crate::entity::{AgeableMob, Animal, Entity, EntityPose, LivingEntity, TamableAnimal};

fn test_wolf() -> WolfEntity {
    init_vanilla_registry();
    WolfEntity::new(&vanilla_entities::WOLF, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn wolf_starts_wild_calm_and_red_collared() {
    let wolf = test_wolf();
    assert!(!wolf.is_tame());
    assert!(!wolf.is_angry());
    assert!(!wolf.is_in_sitting_pose());
    assert_eq!(wolf.collar_color(), DyeColor::Red);
}

#[test]
fn wolf_eats_meat_but_not_bones() {
    let wolf = test_wolf();
    assert!(wolf.is_food(&ItemStack::new(&vanilla_items::BEEF)));
    assert!(!wolf.is_food(&ItemStack::new(&vanilla_items::BONE)));
}

#[test]
fn taming_flags_and_owner_round_trip_through_synced_data() {
    let wolf = test_wolf();
    let owner = Uuid::from_u128(7);
    wolf.set_owner_uuid(Some(owner));
    wolf.set_tame(true, false);
    wolf.set_in_sitting_pose(true);
    assert!(wolf.is_tame());
    assert!(wolf.is_in_sitting_pose());
    assert_eq!(wolf.owner_uuid(), Some(owner));
    wolf.set_in_sitting_pose(false);
    assert!(wolf.is_tame());
    assert!(!wolf.is_in_sitting_pose());
}

#[test]
fn wild_wolves_never_mate() {
    let wolf = test_wolf();
    let partner = WolfEntity::new(&vanilla_entities::WOLF, 2, DVec3::ZERO, Weak::new());
    assert!(!wolf.can_mate(&partner));
}

#[test]
fn wolf_armor_cracks_at_vanilla_thresholds() {
    assert_eq!(armor_crackiness(0, 64), ArmorCrackiness::None);
    assert_eq!(armor_crackiness(4, 64), ArmorCrackiness::Low);
    assert_eq!(armor_crackiness(21, 64), ArmorCrackiness::Medium);
    assert_eq!(armor_crackiness(44, 64), ArmorCrackiness::High);
}

#[test]
fn baby_wolf_has_vanilla_dimensions() {
    let wolf = test_wolf();
    wolf.set_age(wolf.get_baby_start_age());
    assert!(AgeableMob::is_baby(&wolf));
    let baby = wolf.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.width - 0.3).abs() < 1e-6);
    assert!((baby.height - 0.425).abs() < 1e-6);
    assert!((baby.eye_height - 0.343_75).abs() < 1e-6);
    assert!(LivingEntity::get_health(&wolf) > 0.0);
}
