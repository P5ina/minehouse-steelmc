use std::sync::Weak;

use glam::DVec3;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::AxolotlVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

use super::{AxolotlEntity, spawn_variant};
use crate::entity::ai::brain::memory_types::PLAY_DEAD_TICKS;
use crate::entity::{Animal, Entity, LivingEntity, Mob};

fn test_axolotl() -> AxolotlEntity {
    init_vanilla_registry();
    AxolotlEntity::new(&vanilla_entities::AXOLOTL, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn axolotl_starts_lucy_awake_and_holds_its_breath_long() {
    let axolotl = test_axolotl();
    assert_eq!(axolotl.variant(), AxolotlVariant::Lucy);
    assert!(!axolotl.is_playing_dead());
    assert!(!axolotl.is_from_bucket());
    assert_eq!(axolotl.max_air_supply(), 6000);
    assert!(!axolotl.is_pushed_by_fluid());
    assert!(axolotl.mob_can_be_leashed());
}

#[test]
fn axolotl_eats_tropical_fish_buckets() {
    let axolotl = test_axolotl();
    assert!(axolotl.is_food(&ItemStack::new(&vanilla_items::TROPICAL_FISH_BUCKET)));
    assert!(!axolotl.is_food(&ItemStack::new(&vanilla_items::COD)));
}

#[test]
fn rehydrating_adds_air_up_to_the_maximum() {
    let axolotl = test_axolotl();
    axolotl.set_air_supply(100);
    axolotl.rehydrate();
    assert_eq!(axolotl.air_supply(), 1900);
    axolotl.set_air_supply(5900);
    axolotl.rehydrate();
    assert_eq!(axolotl.air_supply(), 6000);
}

#[test]
fn playing_dead_hides_the_axolotl_from_enemies() {
    let axolotl = test_axolotl();
    assert!(axolotl.can_be_seen_as_enemy());
    axolotl.set_playing_dead(true);
    assert!(!axolotl.can_be_seen_as_enemy());
}

#[test]
fn play_dead_ticks_set_the_synced_flag() {
    let axolotl = test_axolotl();
    axolotl.brain_memories_lock().set(&PLAY_DEAD_TICKS, Some(5));
    axolotl.custom_server_ai_step();
    assert!(axolotl.is_playing_dead());
    axolotl.brain_memories_lock().erase(&PLAY_DEAD_TICKS);
    axolotl.custom_server_ai_step();
    assert!(!axolotl.is_playing_dead());
}

#[test]
fn axolotl_saves_variant_and_bucket_flag() {
    let axolotl = test_axolotl();
    axolotl.set_variant(AxolotlVariant::Gold);
    axolotl.set_from_bucket(true);
    let mut nbt = NbtCompound::new();
    axolotl.save_additional(&mut nbt);
    assert_eq!(nbt.int("Variant"), Some(2));
    assert_eq!(nbt.byte("FromBucket"), Some(1));
    assert!(!axolotl.remove_when_far_away(0.0));
}

#[test]
fn only_blue_is_rare() {
    for _ in 0..50 {
        assert!(spawn_variant(true).is_common());
        assert_eq!(spawn_variant(false), AxolotlVariant::Blue);
    }
}
