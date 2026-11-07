use std::io::Cursor;
use std::sync::Weak;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use steel_registry::init_vanilla_registry;

use super::*;

fn camel(id: i32) -> CamelEntity {
    CamelEntity::new(&vanilla_entities::CAMEL, id, DVec3::ZERO, Weak::new())
}

fn camel_husk(id: i32) -> CamelHuskEntity {
    CamelHuskEntity::new(&vanilla_entities::CAMEL_HUSK, id, DVec3::ZERO, Weak::new())
}

fn round_trip(
    save: impl FnOnce(&mut NbtCompound),
    load: impl FnOnce(BorrowedNbtCompoundView<'_, '_>),
) {
    let mut nbt = NbtCompound::new();
    save(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    load((&borrowed).into());
}

#[test]
fn negative_pose_tick_loads_as_sitting() {
    init_vanilla_registry();
    let sitting = camel(1);
    sitting.reset_last_pose_change_tick(-100);

    let loaded = camel(2);
    round_trip(
        |nbt| sitting.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert_eq!(loaded.last_pose_change_tick(), -100);
    assert!(loaded.is_camel_sitting());
    assert_eq!(loaded.pose(), EntityPose::Sitting);
    assert!(loaded.refuse_to_move());
}

#[test]
fn sitting_shrinks_the_camel() {
    init_vanilla_registry();
    let adult = camel(1);
    let standing = adult.dimensions_for_pose(EntityPose::Standing);
    let sitting = adult.dimensions_for_pose(EntityPose::Sitting);
    assert!((sitting.width - standing.width).abs() < f32::EPSILON);
    assert!((standing.height - sitting.height - SITTING_HEIGHT_DIFFERENCE).abs() < 1.0e-5);
    assert!((sitting.eye_height - ADULT_SITTING_EYE_HEIGHT).abs() < f32::EPSILON);

    let baby = camel(2);
    baby.set_age(-24_000);
    let baby_standing = baby.dimensions_for_pose(EntityPose::Standing);
    assert!((baby_standing.height - 1.4).abs() < f32::EPSILON);
    assert!((baby.get_age_scale() - BABY_SCALE).abs() < f32::EPSILON);
    let baby_sitting = baby.dimensions_for_pose(EntityPose::Sitting);
    assert!((baby_sitting.height - 0.425).abs() < f32::EPSILON);
}

#[test]
fn camels_are_always_tame_and_never_rear() {
    init_vanilla_registry();
    let adult = camel(1);
    assert!(adult.is_tamed());
    adult.set_tamed(false);
    assert!(adult.is_tamed());
    assert!(!adult.can_perform_rearing());
    assert!(adult.is_food(&ItemStack::new(&vanilla_items::CACTUS)));
    assert!(!adult.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
}

#[test]
fn camel_husks_never_breed_or_grow_up() {
    init_vanilla_registry();
    let husk = camel_husk(1);
    husk.set_age(-24_000);
    assert!(!AgeableMob::is_baby(&husk));
    assert!(!husk.can_fall_in_love());
    assert!(!husk.can_mate(&camel_husk(2)));
    assert!(!husk.can_mate(&camel(3)));
    assert!(husk.remove_when_far_away(0.0));
    assert!(husk.is_food(&ItemStack::new(&vanilla_items::RABBIT_FOOT)));
    assert!(!husk.is_mob_controlled());
}

#[test]
fn ridden_dash_needs_a_standing_saddled_camel() {
    init_vanilla_registry();
    let adult = camel(1);
    // Without a world the pose time is never past the stand-up animation.
    assert!(adult.refuse_to_move());
    assert!(!adult.can_jump());
}
