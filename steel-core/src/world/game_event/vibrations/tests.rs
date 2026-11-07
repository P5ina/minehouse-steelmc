use std::io::Cursor;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::game_events::GameEvent;
use steel_registry::{init_vanilla_registry, vanilla_game_events};

use super::{
    VibrationData, VibrationInfo, VibrationSelector, distance_between_in_blocks,
    game_event_frequency, redstone_strength_for_distance,
};
use steel_utils::BlockPos;

fn vibration(event: &'static GameEvent, distance: f32) -> VibrationInfo {
    VibrationInfo::new(event, distance, DVec3::new(1.5, 64.5, 2.5), None)
}

fn round_trip(nbt: &NbtCompound) -> Option<VibrationData> {
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let parsed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("vibration NBT should parse: {error}"));
    VibrationData::load((&parsed).into())
}

#[test]
fn frequencies_match_the_vanilla_table() {
    init_vanilla_registry();
    assert_eq!(game_event_frequency(&vanilla_game_events::STEP), 1);
    assert_eq!(
        game_event_frequency(&vanilla_game_events::NOTE_BLOCK_PLAY),
        10
    );
    assert_eq!(game_event_frequency(&vanilla_game_events::EXPLODE), 15);
    assert_eq!(game_event_frequency(&vanilla_game_events::RESONATE_7), 7);
    assert_eq!(game_event_frequency(&vanilla_game_events::JUKEBOX_PLAY), 0);
}

#[test]
fn redstone_strength_falls_off_with_distance() {
    assert_eq!(redstone_strength_for_distance(0.0, 8), 15);
    assert_eq!(redstone_strength_for_distance(4.0, 8), 8);
    assert_eq!(redstone_strength_for_distance(100.0, 8), 1);
}

#[test]
fn block_distance_is_euclidean() {
    let distance = distance_between_in_blocks(BlockPos::new(0, 0, 0), BlockPos::new(3, 4, 0));
    assert!((distance - 5.0).abs() < f32::EPSILON);
}

#[test]
fn selector_keeps_the_closest_vibration_of_a_tick() {
    init_vanilla_registry();
    let mut selector = VibrationSelector::default();
    selector.add_candidate(vibration(&vanilla_game_events::STEP, 5.0), 10);
    selector.add_candidate(vibration(&vanilla_game_events::STEP, 3.0), 10);
    selector.add_candidate(vibration(&vanilla_game_events::STEP, 4.0), 10);
    // A later tick never replaces the pending candidate.
    selector.add_candidate(vibration(&vanilla_game_events::STEP, 1.0), 11);

    assert!(selector.chosen_candidate(10).is_none());
    let Some(chosen) = selector.chosen_candidate(11) else {
        panic!("a candidate from an earlier tick is chosen");
    };
    assert!((chosen.distance() - 3.0).abs() < f32::EPSILON);
    selector.start_over();
    assert!(selector.chosen_candidate(11).is_none());
}

#[test]
fn selector_breaks_distance_ties_by_frequency() {
    init_vanilla_registry();
    let mut selector = VibrationSelector::default();
    selector.add_candidate(vibration(&vanilla_game_events::STEP, 2.0), 0);
    selector.add_candidate(vibration(&vanilla_game_events::EXPLODE, 2.0), 0);
    selector.add_candidate(vibration(&vanilla_game_events::EAT, 2.0), 0);
    let Some(chosen) = selector.chosen_candidate(1) else {
        panic!("a candidate is chosen");
    };
    assert_eq!(chosen.game_event().key, vanilla_game_events::EXPLODE.key);
}

#[test]
fn data_round_trips_through_nbt() {
    init_vanilla_registry();
    let mut data = VibrationData::default();
    data.set_current_vibration(Some(vibration(&vanilla_game_events::NOTE_BLOCK_PLAY, 6.5)));
    data.set_travel_time_in_ticks(4);

    let Some(loaded) = round_trip(&data.save()) else {
        panic!("saved vibration data should load");
    };
    assert_eq!(loaded.travel_time_in_ticks(), 4);
    let Some(current) = loaded.current_vibration() else {
        panic!("the traveling vibration is saved");
    };
    assert_eq!(
        current.game_event().key,
        vanilla_game_events::NOTE_BLOCK_PLAY.key
    );
    assert!((current.distance() - 6.5).abs() < f32::EPSILON);
    assert_eq!(current.pos(), DVec3::new(1.5, 64.5, 2.5));
    assert!(
        loaded
            .selection_strategy()
            .chosen_candidate(i64::MAX)
            .is_none()
    );
}

#[test]
fn negative_event_delay_is_rejected() {
    init_vanilla_registry();
    let mut selector = NbtCompound::new();
    selector.insert("tick", -1i64);
    let mut nbt = NbtCompound::new();
    nbt.insert("selector", selector);
    nbt.insert("event_delay", -1);
    assert!(round_trip(&nbt).is_none());
    nbt = NbtCompound::new();
    let mut selector = NbtCompound::new();
    selector.insert("tick", -1i64);
    nbt.insert("selector", selector);
    nbt.insert("event_delay", 3);
    assert_eq!(
        round_trip(&nbt).map(|data| data.travel_time_in_ticks()),
        Some(3)
    );
    nbt = NbtCompound::new();
    nbt.insert("event_delay", -1);
    assert!(round_trip(&nbt).is_none());
}
