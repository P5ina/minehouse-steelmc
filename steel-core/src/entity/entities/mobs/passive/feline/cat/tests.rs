use std::sync::Weak;

use glam::DVec3;
use steel_registry::cat_variant::SpawnCondition;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    DyeColor, REGISTRY, RegistryExt as _, init_vanilla_registry, vanilla_entities, vanilla_items,
};
use steel_utils::Identifier;
use steel_utils::random::legacy_random::LegacyRandom;
use uuid::Uuid;

use super::CatEntity;
use crate::entity::{AgeableMob, Animal, Entity, EntityPose, Mob, TamableAnimal};

fn test_cat() -> CatEntity {
    init_vanilla_registry();
    CatEntity::new(&vanilla_entities::CAT, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn cat_starts_wild_black_and_red_collared() {
    let cat = test_cat();
    assert!(!cat.is_tame());
    assert!(!cat.is_lying());
    assert_eq!(cat.collar_color(), DyeColor::Red);
    assert_eq!(cat.variant().key, Identifier::vanilla_static("black"));
    assert_eq!(
        cat.sound_variant().key,
        Identifier::vanilla_static("classic")
    );
}

#[test]
fn cat_eats_fish_only() {
    let cat = test_cat();
    assert!(cat.is_food(&ItemStack::new(&vanilla_items::COD)));
    assert!(cat.is_food(&ItemStack::new(&vanilla_items::SALMON)));
    assert!(!cat.is_food(&ItemStack::new(&vanilla_items::BEEF)));
}

#[test]
fn wild_cats_despawn_and_tame_ones_stay() {
    let cat = test_cat();
    for _ in 0..2401 {
        cat.advance_tick_count();
    }
    assert!(cat.remove_when_far_away(0.0));
    cat.set_owner_uuid(Some(Uuid::from_u128(3)));
    cat.set_tame(true, true);
    assert!(!cat.remove_when_far_away(0.0));
    assert_eq!(cat.ambient_sound_interval(), 120);
}

#[test]
fn only_tame_cats_mate() {
    let cat = test_cat();
    let partner = CatEntity::new(&vanilla_entities::CAT, 2, DVec3::ZERO, Weak::new());
    assert!(!cat.can_mate(&partner));
    for tame in [&cat, &partner] {
        tame.set_owner_uuid(Some(Uuid::from_u128(5)));
        tame.set_tame(true, true);
    }
    // Tame but not in love yet.
    assert!(!cat.can_mate(&partner));
}

#[test]
fn baby_cat_has_vanilla_dimensions() {
    let cat = test_cat();
    cat.set_age(cat.get_baby_start_age());
    let baby = cat.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.width - 0.3).abs() < 1e-6);
    assert!((baby.height - 0.35).abs() < 1e-6);
}

#[test]
fn spawn_variant_prefers_witch_huts_then_full_moon() {
    init_vanilla_registry();
    let mut random = LegacyRandom::from_seed(1);
    let in_hut = REGISTRY.cat_variants.select_spawn_variant(
        |condition| matches!(condition, SpawnCondition::Structure { .. }),
        &mut random,
    );
    assert_eq!(
        in_hut.map(|variant| &variant.key),
        Some(&Identifier::vanilla_static("all_black"))
    );

    let full_moon = |condition: &SpawnCondition| match condition {
        SpawnCondition::MoonBrightness { min, max } => {
            SpawnCondition::matches_moon_brightness(*min, *max, 1.0)
        }
        _ => false,
    };
    for _ in 0..16 {
        let variant = REGISTRY
            .cat_variants
            .select_spawn_variant(full_moon, &mut random);
        assert!(variant.is_some());
    }

    let mut seen_all_black = false;
    for _ in 0..64 {
        let variant = REGISTRY
            .cat_variants
            .select_spawn_variant(|_| false, &mut random);
        seen_all_black |= variant.is_some_and(|variant| variant.key.path == "all_black");
    }
    assert!(
        !seen_all_black,
        "all black needs a witch hut or a bright moon"
    );
    assert!(
        REGISTRY
            .cat_variants
            .by_key(&Identifier::vanilla_static("tabby"))
            .is_some()
    );
}

#[test]
fn moon_brightness_bounds_are_inclusive() {
    assert!(SpawnCondition::matches_moon_brightness(
        Some(0.9),
        None,
        0.9
    ));
    assert!(!SpawnCondition::matches_moon_brightness(
        Some(0.9),
        None,
        0.75
    ));
    assert!(SpawnCondition::matches_moon_brightness(
        None,
        Some(0.5),
        0.5
    ));
}
