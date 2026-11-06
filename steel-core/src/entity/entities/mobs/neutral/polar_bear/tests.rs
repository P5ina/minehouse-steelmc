use std::sync::Weak;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities};

use super::PolarBearEntity;
use crate::entity::neutral_mob::NeutralMob;
use crate::entity::{AgeableMob, Animal, Entity, EntityPose};

fn test_bear() -> PolarBearEntity {
    init_vanilla_registry();
    PolarBearEntity::new(&vanilla_entities::POLAR_BEAR, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn polar_bear_stands_up_through_synced_data() {
    let bear = test_bear();
    assert!(!bear.is_standing());
    bear.set_standing(true);
    assert!(bear.is_standing());
}

#[test]
fn polar_bear_eats_nothing_and_starts_calm() {
    let bear = test_bear();
    assert!(!bear.is_food(&ItemStack::empty()));
    assert!(!bear.is_angry());
}

#[test]
fn baby_polar_bear_has_vanilla_dimensions() {
    let bear = test_bear();
    bear.set_age(bear.get_baby_start_age());
    assert!(AgeableMob::is_baby(&bear));
    let baby = bear.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.width - 0.7).abs() < 1e-6);
    assert!((baby.height - 0.7).abs() < 1e-6);
    assert!((baby.eye_height - 0.343_75).abs() < 1e-6);
}
