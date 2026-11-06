use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::HorseVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

use super::chested::AbstractChestedHorse as _;
use super::*;
use crate::entity::{Animal, Entity, SharedEntity};

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
fn offspring_attribute_stays_in_range() {
    for _ in 0..1000 {
        let Some(value) = create_offspring_attribute(0.1, 0.3, 0.1125, 0.3375) else {
            panic!("a non-empty range always yields a value");
        };
        assert!((0.1125..=0.3375).contains(&value), "{value}");
    }
    assert_eq!(create_offspring_attribute(1.0, 2.0, 3.0, 3.0), None);
}

#[test]
fn generated_attributes_match_vanilla_bounds() {
    assert_eq!(generate_max_health(|_| 0).to_bits(), 15.0_f32.to_bits());
    assert_eq!(
        generate_max_health(|bound| bound - 1).to_bits(),
        30.0_f32.to_bits()
    );
    assert!((generate_jump_strength(|| 1.0) - 1.0).abs() < 1e-6);
    assert!((generate_speed(|| 1.0) - 0.3375).abs() < 1e-6);
}

#[test]
fn every_equine_exposes_horse_capabilities() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    let mule = MuleEntity::new(&vanilla_entities::MULE, 3, DVec3::ZERO, Weak::new());
    let skeleton = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        4,
        DVec3::ZERO,
        Weak::new(),
    );
    let zombie =
        ZombieHorseEntity::new(&vanilla_entities::ZOMBIE_HORSE, 5, DVec3::ZERO, Weak::new());
    for entity in [&horse as &dyn Entity, &donkey, &mule, &skeleton, &zombie] {
        assert!(entity.is_abstract_horse());
        assert!(entity.as_player_rideable_jumping().is_some());
    }
    assert!(!horse.is_tamed());
    assert!(
        !horse
            .as_player_rideable_jumping()
            .is_some_and(PlayerRideableJumping::can_jump)
    );
}

#[test]
fn horse_and_donkey_can_mate_but_mules_cannot() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    let mule = MuleEntity::new(&vanilla_entities::MULE, 3, DVec3::ZERO, Weak::new());
    for parent in [&horse as &dyn AbstractHorse, &donkey] {
        parent.set_tamed(true);
        parent.set_in_love(None);
    }
    mule.set_tamed(true);
    mule.set_in_love(None);

    assert!(Animal::can_mate(&horse, &donkey));
    assert!(Animal::can_mate(&donkey, &horse));
    assert!(!Animal::can_mate(&mule, &horse));
    assert!(!Animal::can_mate(&horse, &mule));
}

#[test]
fn horse_food_differs_from_zombie_horse_food() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let zombie =
        ZombieHorseEntity::new(&vanilla_entities::ZOMBIE_HORSE, 2, DVec3::ZERO, Weak::new());
    let apple = ItemStack::new(&vanilla_items::APPLE);
    assert!(Animal::is_food(&horse, &apple));
    assert!(!Animal::is_food(&zombie, &apple));
    assert!(!Animal::can_fall_in_love(&zombie));
}

#[test]
fn horse_persists_taming_temper_and_coat() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    horse.set_tamed(true);
    horse.set_temper(42);
    horse.set_variant_and_markings(HorseVariant::Chestnut, 3);

    let loaded = HorseEntity::new(&vanilla_entities::HORSE, 2, DVec3::ZERO, Weak::new());
    round_trip(
        |nbt| horse.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.is_tamed());
    assert_eq!(loaded.temper(), 42);
    assert_eq!(loaded.variant(), HorseVariant::Chestnut);
    assert_eq!(loaded.markings(), 3);
}

#[test]
fn donkey_persists_its_chest() {
    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    donkey.set_chest(true);

    let loaded = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    round_trip(
        |nbt| donkey.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.has_chest());
    assert_eq!(loaded.inventory_columns(), 5);
}

#[test]
fn skeleton_horse_persists_trap_state() {
    init_vanilla_registry();
    let horse = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        1,
        DVec3::ZERO,
        Weak::new(),
    );
    horse.set_trap(true);

    let loaded = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        2,
        DVec3::ZERO,
        Weak::new(),
    );
    round_trip(
        |nbt| horse.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.is_trap());
}

fn shared<T: Entity + 'static>(entity: T) -> SharedEntity {
    Arc::new(entity)
}

#[test]
fn chested_donkey_menu_has_saddle_armor_storage_and_player_slots() {
    use steel_utils::locks::IntoShared as _;

    use crate::player::player_inventory::PlayerInventory;

    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    donkey.set_chest(true);
    donkey.create_inventory();
    let storage = donkey.horse_base().inventory();
    let columns = donkey.inventory_columns();
    let mount = shared(donkey);

    let menu = menu::horse_inventory_menu(
        &PlayerInventory::new().into_shared(),
        1,
        &mount,
        storage,
        columns,
    );

    assert_eq!(menu.behavior().slot_count(), 2 + 15 + 36);
}

#[test]
fn equipment_container_equips_the_mount() {
    use crate::inventory::container::Container as _;
    use crate::inventory::equipment::EquipmentSlot;

    init_vanilla_registry();
    let horse = shared(HorseEntity::new(
        &vanilla_entities::HORSE,
        1,
        DVec3::ZERO,
        Weak::new(),
    ));
    let mut saddle = menu::MountEquipmentContainer::new(&horse, EquipmentSlot::Saddle);

    saddle.set_item(0, ItemStack::new(&vanilla_items::SADDLE));

    let Some(horse) = horse.as_abstract_horse() else {
        panic!("a horse is an abstract horse");
    };
    assert!(horse.is_saddled());
    assert_eq!(saddle.get_max_stack_size(), 1);
}

#[test]
fn resizing_the_inventory_invalidates_open_menus() {
    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    let before = donkey.horse_base().inventory();
    assert!(!donkey.horse_base().has_inventory_changed(&before));

    donkey.set_chest(true);
    donkey.create_inventory();

    assert!(donkey.horse_base().has_inventory_changed(&before));
}
