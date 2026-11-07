use std::io::Cursor;
use std::sync::Weak;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use steel_registry::{init_vanilla_registry, vanilla_instrument_tags};

use super::*;

fn goat(id: i32) -> GoatEntity {
    GoatEntity::new(&vanilla_entities::GOAT, id, DVec3::ZERO, Weak::new())
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

fn horn_instrument(horn: &ItemStack) -> Option<&'static str> {
    horn.get(INSTRUMENT)
        .and_then(|component| component.instrument().as_reference())
        .map(|instrument| instrument.key.path.as_ref())
}

#[test]
fn goat_persists_screaming_and_horns() {
    init_vanilla_registry();
    let screamer = goat(1);
    screamer.set_screaming_goat(true);
    screamer.set_left_horn(false);

    let loaded = goat(2);
    round_trip(
        |nbt| screamer.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.is_screaming_goat());
    assert!(!loaded.has_left_horn());
    assert!(loaded.has_right_horn());
}

#[test]
fn missing_horn_tags_default_to_both_horns() {
    init_vanilla_registry();
    let loaded = goat(1);
    loaded.set_left_horn(false);
    round_trip(|_| {}, |nbt| loaded.load_additional(nbt));
    assert!(loaded.has_left_horn());
    assert!(loaded.has_right_horn());
    assert!(!loaded.is_screaming_goat());
}

#[test]
fn uuid_hash_matches_java() {
    let uuid = Uuid::from_u64_pair(0xFFFF_FFFF_0000_0000, 0x1);
    assert_eq!(java_uuid_hash_code(uuid), -2);
    assert_eq!(
        java_uuid_hash_code(Uuid::from_u64_pair(0x1_0000_0002, 0x3)),
        0
    );
}

#[test]
fn horn_follows_the_goat_uuid_and_kind() {
    init_vanilla_registry();
    let regular = goat(1);
    let first = horn_instrument(&regular.create_horn());
    assert_eq!(first, horn_instrument(&regular.create_horn()));
    let Some(first) = first else {
        panic!("a regular goat horn carries an instrument");
    };
    assert!(
        [
            "ponder_goat_horn",
            "sing_goat_horn",
            "seek_goat_horn",
            "feel_goat_horn"
        ]
        .contains(&first),
        "{first}"
    );

    regular.set_screaming_goat(true);
    let Some(screaming) = horn_instrument(&regular.create_horn()) else {
        panic!("a screaming goat horn carries an instrument");
    };
    assert!(
        [
            "admire_goat_horn",
            "call_goat_horn",
            "yearn_goat_horn",
            "dream_goat_horn"
        ]
        .contains(&screaming),
        "{screaming}"
    );
    assert!(
        REGISTRY
            .instruments
            .get_tag(&vanilla_instrument_tags::InstrumentTag::SCREAMING_GOAT_HORNS)
            .is_some_and(|tag| tag.len() == 4)
    );
}

#[test]
fn horns_drop_one_at_a_time_and_never_from_babies() {
    init_vanilla_registry();
    let adult = goat(1);
    assert!(adult.drop_horn());
    assert_ne!(adult.has_left_horn(), adult.has_right_horn());
    assert!(adult.drop_horn());
    assert!(!adult.has_left_horn() && !adult.has_right_horn());
    assert!(!adult.drop_horn());

    let baby = goat(2);
    baby.set_age(-24000);
    assert!(!baby.drop_horn());
    assert!(baby.has_left_horn() && baby.has_right_horn());
}

#[test]
fn falls_hurt_ten_points_less() {
    init_vanilla_registry();
    let goat_entity = goat(1);
    let base = LivingEntityBase::calculate_fall_damage(
        20.0,
        1.0,
        vanilla_attributes::SAFE_FALL_DISTANCE.default_value,
        vanilla_attributes::FALL_DAMAGE_MULTIPLIER.default_value,
    );
    assert_eq!(
        goat_entity.calculate_fall_damage(20.0, 1.0),
        base - GOAT_FALL_DAMAGE_REDUCTION
    );
    assert!(goat_entity.calculate_fall_damage(5.0, 1.0) <= 0);
}

#[test]
fn long_jumping_shrinks_the_goat() {
    init_vanilla_registry();
    let adult = goat(1);
    let standing = adult.dimensions_for_pose(EntityPose::Standing);
    let jumping = adult.dimensions_for_pose(EntityPose::LongJumping);
    assert!((jumping.width - standing.width * 0.7).abs() < 1.0e-6);
    assert!((jumping.height - standing.height * 0.7).abs() < 1.0e-6);

    let baby = goat(2);
    baby.set_age(-24000);
    let baby_dimensions = baby.dimensions_for_pose(EntityPose::Standing);
    assert!((baby_dimensions.width - 0.45).abs() < 1.0e-6);
    assert!((baby_dimensions.eye_height - 0.593_75).abs() < 1.0e-6);
}

#[test]
fn baby_goats_hit_softer() {
    init_vanilla_registry();
    let goat_entity = goat(1);
    goat_entity.set_age(-24000);
    goat_entity.age_boundary_changed(true);
    let damage = goat_entity
        .attributes()
        .lock()
        .required_value(vanilla_attributes::ATTACK_DAMAGE);
    assert!((damage - BABY_ATTACK_DAMAGE).abs() < 1.0e-9);
}

#[test]
fn head_turns_at_most_fifteen_degrees() {
    init_vanilla_registry();
    let goat_entity = goat(1);
    goat_entity.set_y_body_rot(10.0);
    goat_entity.set_y_head_rot(90.0);
    assert!((goat_entity.y_head_rot() - 25.0).abs() < 1.0e-4);
    goat_entity.set_y_head_rot(-30.0);
    assert!((goat_entity.y_head_rot() - -5.0).abs() < 1.0e-4);
}

#[test]
fn edge_cases_of_time_between_rams() {
    init_vanilla_registry();
    let goat_entity = goat(1);
    assert_eq!(goat_entity.time_between_rams().min_inclusive, 600);
    goat_entity.set_screaming_goat(true);
    assert_eq!(goat_entity.time_between_rams().max_inclusive, 300);
}
