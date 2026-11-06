use std::sync::Weak;
use std::sync::atomic::Ordering;

use glam::DVec3;
use steel_registry::{init_vanilla_registry, vanilla_entities};

use super::{
    ElderGuardianEntity, GUARDIAN_ATTACK_DURATION, Guardian, GuardianEntity,
    guardian_attack_selector,
};
use crate::entity::entities::{PigEntity, SquidEntity};
use crate::entity::{Entity, Mob};
use crate::test_support::fresh_test_world;

#[test]
fn guardian_attack_selector_targets_squids_beyond_three_blocks() {
    init_vanilla_registry();
    let guardian = GuardianEntity::new(&vanilla_entities::GUARDIAN, 1, DVec3::ZERO, Weak::new());
    let far_squid = SquidEntity::new(
        &vanilla_entities::SQUID,
        2,
        DVec3::new(4.0, 0.0, 0.0),
        Weak::new(),
    );
    let near_squid = SquidEntity::new(
        &vanilla_entities::SQUID,
        3,
        DVec3::new(2.0, 0.0, 0.0),
        Weak::new(),
    );
    let pig = PigEntity::new(
        &vanilla_entities::PIG,
        4,
        DVec3::new(4.0, 0.0, 0.0),
        Weak::new(),
    );

    let world_fixture = fresh_test_world("guardian_attack_selector");
    let world = &world_fixture.world;

    assert!(guardian_attack_selector(&guardian, &far_squid, world));
    assert!(!guardian_attack_selector(&guardian, &near_squid, world));
    assert!(!guardian_attack_selector(&guardian, &pig, world));
}

#[test]
fn elder_guardian_is_persistent_and_charges_faster() {
    init_vanilla_registry();
    let guardian = GuardianEntity::new(&vanilla_entities::GUARDIAN, 1, DVec3::ZERO, Weak::new());
    let elder = ElderGuardianEntity::new(
        &vanilla_entities::ELDER_GUARDIAN,
        2,
        DVec3::ZERO,
        Weak::new(),
    );

    assert!(!guardian.is_persistence_required());
    assert!(elder.is_persistence_required());
    assert_eq!(guardian.attack_duration(), GUARDIAN_ATTACK_DURATION);
    assert_eq!(elder.attack_duration(), 60);
    assert!(guardian.as_guardian().is_some());
    assert!(elder.as_guardian().is_some());
}

#[test]
fn hurting_a_guardian_triggers_its_random_stroll() {
    init_vanilla_registry();
    let guardian = GuardianEntity::new(&vanilla_entities::GUARDIAN, 1, DVec3::ZERO, Weak::new());
    assert!(!guardian.random_stroll_trigger().load(Ordering::Relaxed));
    guardian.trigger_random_stroll();
    assert!(guardian.random_stroll_trigger().load(Ordering::Relaxed));
}
