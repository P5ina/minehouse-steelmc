use std::sync::Weak;

use glam::DVec3;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::ParrotVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, sound_events, vanilla_entities, vanilla_items};

use super::{MOB_SOUNDS, ParrotEntity, imitated_sound};
use crate::entity::{AgeableMob, Animal, Entity, LivingEntity, TamableAnimal};

fn test_parrot() -> ParrotEntity {
    init_vanilla_registry();
    ParrotEntity::new(&vanilla_entities::PARROT, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn parrot_starts_wild_red_and_never_a_baby() {
    let parrot = test_parrot();
    assert!(!parrot.is_tame());
    assert_eq!(parrot.variant(), ParrotVariant::RedBlue);
    parrot.set_age(-24000);
    assert!(!AgeableMob::is_baby(&parrot));
}

#[test]
fn parrot_neither_breeds_nor_counts_seeds_as_food() {
    let parrot = test_parrot();
    let partner = ParrotEntity::new(&vanilla_entities::PARROT, 2, DVec3::ZERO, Weak::new());
    assert!(!parrot.is_food(&ItemStack::new(&vanilla_items::WHEAT_SEEDS)));
    assert!(!parrot.can_mate(&partner));
}

#[test]
fn parrot_flies_to_its_owner_and_drifts_through_the_air() {
    let parrot = test_parrot();
    assert!(parrot.can_fly_to_owner());
    assert!(parrot.omnidirectional_air_mover());
    assert!(parrot.is_pushable());
}

#[test]
fn parrot_waits_before_it_can_sit_on_a_shoulder() {
    let parrot = test_parrot();
    assert!(!parrot.can_sit_on_shoulder());
    *parrot.ride_cooldown_counter.lock() = 101;
    assert!(parrot.can_sit_on_shoulder());
}

#[test]
fn parrot_variant_saves_as_a_legacy_int() {
    let parrot = test_parrot();
    parrot.set_variant(ParrotVariant::Gray);
    let mut nbt = NbtCompound::new();
    parrot.save_additional(&mut nbt);
    assert_eq!(nbt.int("Variant"), Some(4));
}

#[test]
fn parrot_imitates_the_mobs_vanilla_lists() {
    init_vanilla_registry();
    assert_eq!(MOB_SOUNDS.len(), 42);
    assert_eq!(
        imitated_sound(&vanilla_entities::CAVE_SPIDER),
        Some(&sound_events::ENTITY_PARROT_IMITATE_SPIDER as _)
    );
    assert_eq!(
        imitated_sound(&vanilla_entities::HAPPY_GHAST),
        Some(&sound_events::INTENTIONALLY_EMPTY as _)
    );
    assert_eq!(imitated_sound(&vanilla_entities::PARROT), None);
    assert_eq!(imitated_sound(&vanilla_entities::COW), None);
}

#[test]
fn parrot_pitch_stays_near_one() {
    for _ in 0..100 {
        let pitch = ParrotEntity::random_pitch();
        assert!((0.8..=1.2).contains(&pitch));
    }
}
