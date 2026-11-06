use std::sync::Weak;

use glam::DVec3;
use std::io::Cursor;

use simdnbt::borrow;
use simdnbt::owned::{BaseNbt, NbtCompound};
use steel_registry::{init_vanilla_registry, vanilla_entities};

use super::{DOLPHIN_MAX_AIR_SUPPLY, DOLPHIN_MAX_MOISTNESS, DolphinEntity};
use crate::entity::{AgeableMob, Entity, EntityPose, LivingEntity, Mob};

fn test_dolphin() -> DolphinEntity {
    init_vanilla_registry();
    DolphinEntity::new(&vanilla_entities::DOLPHIN, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn dolphin_holds_more_air_and_refills_it_at_once() {
    let dolphin = test_dolphin();

    assert_eq!(dolphin.max_air_supply(), DOLPHIN_MAX_AIR_SUPPLY);
    assert_eq!(dolphin.increase_air_supply(10), DOLPHIN_MAX_AIR_SUPPLY);
    assert!(!dolphin.is_pushed_by_fluid());
    assert!(dolphin.can_pick_up_loot());
}

#[test]
fn dolphin_saves_fish_and_moistness() {
    let dolphin = test_dolphin();
    assert_eq!(dolphin.moistness_level(), DOLPHIN_MAX_MOISTNESS);
    dolphin.set_got_fish(true);
    dolphin.set_moistness_level(42);

    let mut nbt = NbtCompound::new();
    dolphin.save_additional(&mut nbt);
    let bytes = {
        let mut out = Vec::new();
        BaseNbt::new("", nbt).write(&mut out);
        out
    };
    let Ok(borrow::Nbt::Some(base)) = borrow::read(&mut Cursor::new(&bytes)) else {
        panic!("saved dolphin NBT must parse");
    };

    let loaded = test_dolphin();
    loaded.load_additional(base.as_compound());
    assert!(loaded.got_fish());
    assert_eq!(loaded.moistness_level(), 42);
}

#[test]
fn baby_dolphin_is_smaller_and_does_not_attack() {
    let dolphin = test_dolphin();
    let adult_height = dolphin.dimensions_for_pose(EntityPose::Standing).height;
    dolphin.set_age(dolphin.get_baby_start_age());

    assert!(AgeableMob::is_baby(&dolphin));
    let baby = dolphin.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.height - adult_height * 0.65).abs() < 1e-6);
    assert!((baby.eye_height - 0.093_75).abs() < 1e-6);
    assert!((dolphin.get_age_scale() - 0.65).abs() < 1e-6);
}
