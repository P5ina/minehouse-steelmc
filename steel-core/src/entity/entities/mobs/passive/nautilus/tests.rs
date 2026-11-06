use std::sync::Weak;

use glam::DVec3;
use simdnbt::owned::NbtCompound;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_entities, vanilla_items, vanilla_zombie_nautilus_variants,
};

use super::{AbstractNautilus, NautilusEntity, ZombieNautilusEntity};
use crate::entity::{AgeableMob, Animal, Entity, EntityPose, Mob, TamableAnimal};

fn test_nautilus() -> NautilusEntity {
    init_vanilla_registry();
    NautilusEntity::new(&vanilla_entities::NAUTILUS, 1, DVec3::ZERO, Weak::new())
}

fn test_zombie_nautilus() -> ZombieNautilusEntity {
    init_vanilla_registry();
    ZombieNautilusEntity::new(
        &vanilla_entities::ZOMBIE_NAUTILUS,
        1,
        DVec3::ZERO,
        Weak::new(),
    )
}

#[test]
fn nautilus_starts_wild_calm_and_breathes_long() {
    let nautilus = test_nautilus();
    assert!(!nautilus.is_tame());
    assert!(!nautilus.is_aggravated());
    assert!(!nautilus.is_dashing());
    assert_eq!(nautilus.max_air_supply(), 300);
    assert!(!nautilus.is_pushed_by_fluid());
    assert!(nautilus.mob_can_be_leashed());
}

#[test]
fn wild_nautilus_only_takes_taming_items_and_tame_one_eats_fish() {
    let nautilus = test_nautilus();
    assert!(nautilus.is_food(&ItemStack::new(&vanilla_items::PUFFERFISH)));
    assert!(!nautilus.is_food(&ItemStack::new(&vanilla_items::COD)));
    nautilus.set_tame(true, false);
    assert!(nautilus.is_food(&ItemStack::new(&vanilla_items::COD)));
    assert!(!nautilus.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
}

#[test]
fn nautilus_baby_is_half_size() {
    let nautilus = test_nautilus();
    let adult = nautilus.dimensions_for_pose(EntityPose::Standing);
    nautilus.set_age(-24000);
    assert!(AgeableMob::is_baby(&nautilus));
    let baby = nautilus.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.width - adult.width * 0.5).abs() < 1e-6);
}

#[test]
fn only_a_tame_nautilus_takes_a_saddle_and_armor() {
    let nautilus = test_nautilus();
    assert!(!nautilus.can_use_nautilus_slot(EquipmentSlot::Saddle));
    nautilus.set_tame(true, false);
    assert!(nautilus.can_use_nautilus_slot(EquipmentSlot::Saddle));
    assert!(nautilus.can_use_nautilus_slot(EquipmentSlot::Body));
}

#[test]
fn zombie_nautilus_never_breeds_and_burns_through_its_body_slot() {
    let zombie = test_zombie_nautilus();
    zombie.set_age(-24000);
    assert!(!AgeableMob::is_baby(&zombie));
    assert_eq!(zombie.sun_protection_slot(), EquipmentSlot::Body);
}

#[test]
fn zombie_nautilus_saves_its_variant() {
    let zombie = test_zombie_nautilus();
    assert_eq!(
        zombie.variant().key,
        vanilla_zombie_nautilus_variants::TEMPERATE.key
    );
    zombie.set_variant(&vanilla_zombie_nautilus_variants::WARM);
    let mut nbt = NbtCompound::new();
    zombie.save_additional(&mut nbt);
    assert_eq!(
        nbt.string("variant").map(|v| v.to_str().into_owned()),
        Some("minecraft:warm".to_owned())
    );
}
