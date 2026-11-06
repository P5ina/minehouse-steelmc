use std::io::Cursor;

use simdnbt::borrow::read_compound;
use steel_registry::init_vanilla_registry;

use super::memory_types::{
    GAZE_COOLDOWN_TICKS, IS_PANICKING, NEAREST_LIVING_ENTITIES, WALK_TARGET,
};
use super::*;

/// A stand-in body: the brain core never calls into it.
struct Body;

fn brain_with(memory_types: &[&'static dyn AnyMemoryModuleType]) -> Brain<Body> {
    Brain::new(memory_types, Vec::new(), Vec::new(), None)
}

#[test]
fn unregistered_memory_ignores_writes() {
    let brain = brain_with(&[]);
    let mut memories = brain.memories().lock();
    memories.set(&GAZE_COOLDOWN_TICKS, Some(5));

    assert_eq!(memories.get(&GAZE_COOLDOWN_TICKS), None);
    assert!(
        !memories.check(
            MemoryRequirement::new(&GAZE_COOLDOWN_TICKS, MemoryStatus::Registered)
                .memory()
                .id(),
            MemoryStatus::Registered
        )
    );
}

#[test]
fn expiring_memory_clears_after_its_time_to_live() {
    let brain = brain_with(&[&GAZE_COOLDOWN_TICKS]);
    let mut memories = brain.memories().lock();
    memories.set_with_expiry(&GAZE_COOLDOWN_TICKS, 7, 2);

    // Vanilla counts 2 -> 1 -> 0 and clears on the tick after reaching 0.
    memories.forget_outdated();
    memories.forget_outdated();
    assert_eq!(memories.get(&GAZE_COOLDOWN_TICKS), Some(7));
    memories.forget_outdated();
    assert_eq!(memories.get(&GAZE_COOLDOWN_TICKS), None);
}

#[test]
fn empty_collection_clears_memory() {
    let brain = brain_with(&[&NEAREST_LIVING_ENTITIES]);
    let mut memories = brain.memories().lock();
    memories.set(&NEAREST_LIVING_ENTITIES, Some(Vec::new()));

    assert!(!memories.has_value(&NEAREST_LIVING_ENTITIES));
    assert!(memories.check(
        AnyMemoryModuleType::id(&NEAREST_LIVING_ENTITIES),
        MemoryStatus::ValueAbsent
    ));
}

#[test]
fn saved_memories_round_trip_and_skip_unsaved_types() {
    init_vanilla_registry();
    let brain = brain_with(&[&GAZE_COOLDOWN_TICKS, &IS_PANICKING, &WALK_TARGET]);
    {
        let mut memories = brain.memories().lock();
        memories.set_with_expiry(&GAZE_COOLDOWN_TICKS, 3, 40);
        memories.set(&IS_PANICKING, Some(true));
        memories.set(
            &WALK_TARGET,
            Some(memory_types::WalkTarget::to_point(
                glam::DVec3::ZERO,
                1.0,
                0,
            )),
        );
    }

    let saved = brain.save();
    let mut bytes = Vec::new();
    saved.write(&mut bytes);
    let packed =
        read_compound(&mut Cursor::new(bytes.as_slice())).expect("saved brain is valid NBT");
    let loaded: Brain<Body> = Brain::new(
        &[&GAZE_COOLDOWN_TICKS, &IS_PANICKING, &WALK_TARGET],
        Vec::new(),
        Vec::new(),
        Some((&packed).into()),
    );

    let memories = loaded.memories().lock();
    assert_eq!(memories.get(&GAZE_COOLDOWN_TICKS), Some(3));
    assert_eq!(memories.time_until_expiry(&GAZE_COOLDOWN_TICKS), Some(40));
    assert_eq!(memories.get(&IS_PANICKING), Some(true));
    assert!(!memories.has_value(&WALK_TARGET));
}

#[test]
fn activity_switches_only_when_requirements_hold() {
    let brain: Brain<Body> = Brain::new(
        &[&IS_PANICKING],
        Vec::new(),
        vec![
            ActivityData::new(Activity::IDLE, Vec::new()),
            ActivityData::new(Activity::PANIC, Vec::new()).while_memory_present(&IS_PANICKING),
        ],
        None,
    );

    brain.set_active_activity_if_possible(Activity::PANIC);
    assert!(brain.is_active(Activity::IDLE));
    assert_eq!(brain.active_non_core_activity(), Some(Activity::IDLE));

    brain.memories().lock().set(&IS_PANICKING, Some(true));
    brain.set_active_activity_if_possible(Activity::PANIC);
    assert!(brain.is_active(Activity::PANIC));
    assert!(brain.is_active(Activity::CORE));

    // Leaving PANIC erases the memory it requires.
    brain.use_default_activity();
    assert!(!brain.memories().lock().has_value(&IS_PANICKING));
}
