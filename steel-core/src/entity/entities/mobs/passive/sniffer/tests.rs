use std::io::Cursor;
use std::sync::Weak;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use steel_registry::init_vanilla_registry;
use steel_utils::Identifier;

use super::*;

fn sniffer(id: i32) -> SnifferEntity {
    SnifferEntity::new(&vanilla_entities::SNIFFER, id, DVec3::ZERO, Weak::new())
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
fn digging_lowers_the_bounding_box() {
    init_vanilla_registry();
    let adult = sniffer(1);
    let standing = adult.dimensions_for_pose(EntityPose::Standing);
    assert!((standing.height - 1.75).abs() < f32::EPSILON);

    adult.transition_to(SnifferState::Digging);
    let digging = adult.dimensions_for_pose(EntityPose::Standing);
    assert!((digging.width - standing.width).abs() < f32::EPSILON);
    assert!((digging.height - (1.75 - DIGGING_BB_HEIGHT_OFFSET)).abs() < 1.0e-5);
    assert!((digging.eye_height - DIGGING_EYE_HEIGHT).abs() < f32::EPSILON);

    adult.transition_to(SnifferState::Rising);
    let rising = adult.dimensions_for_pose(EntityPose::Standing);
    assert!((rising.height - 1.75).abs() < f32::EPSILON);
}

#[test]
fn digging_schedules_the_seed_drop() {
    init_vanilla_registry();
    let adult = sniffer(1);
    adult.transition_to(SnifferState::Digging);
    assert_eq!(
        adult.drop_seed_at_tick(),
        adult.tick_count() + DIGGING_DROP_SEED_OFFSET_TICKS
    );
}

#[test]
fn explored_positions_survive_save_and_load() {
    init_vanilla_registry();
    let explored = vec![
        GlobalPos::new(
            Identifier::vanilla_static("overworld"),
            BlockPos::new(1, 2, 3),
        ),
        GlobalPos::new(
            Identifier::vanilla_static("the_nether"),
            BlockPos::new(-4, 5, -6),
        ),
    ];
    let saved = sniffer(1);
    saved
        .brain
        .memories()
        .lock()
        .set(&SNIFFER_EXPLORED_POSITIONS, Some(explored.clone()));

    let loaded = sniffer(2);
    round_trip(
        |nbt| saved.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert_eq!(loaded.explored_positions(), explored);
}

#[test]
fn only_resting_sniffers_mate() {
    init_vanilla_registry();
    let first = sniffer(1);
    let second = sniffer(2);
    first.set_in_love_time(600);
    second.set_in_love_time(600);
    assert!(first.can_mate(&second));

    second.transition_to(SnifferState::Scenting);
    assert!(first.can_mate(&second));
    second.transition_to(SnifferState::Sniffing);
    assert!(!first.can_mate(&second));
    second.transition_to(SnifferState::Digging);
    assert!(!first.can_mate(&second));
}

#[test]
fn babies_start_older_and_eat_torchflower_seeds() {
    init_vanilla_registry();
    let adult = sniffer(1);
    assert_eq!(adult.get_baby_start_age(), SNIFFER_BABY_START_AGE);
    assert!(adult.is_food(&ItemStack::new(&vanilla_items::TORCHFLOWER_SEEDS)));
    assert!(!adult.is_food(&ItemStack::new(&vanilla_items::WHEAT_SEEDS)));
}

#[test]
fn babies_never_sniff_out_a_dig_spot() {
    init_vanilla_registry();
    let baby = sniffer(1);
    baby.set_age(SNIFFER_BABY_START_AGE);
    assert!(!baby.can_dig());
}
