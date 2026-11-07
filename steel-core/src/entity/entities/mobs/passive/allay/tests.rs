use std::io::Cursor;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};
use steel_utils::BlockPos;

use super::{AllayEntity, DUPLICATION_COOLDOWN_TICKS};
use crate::entity::{Entity, EquipmentSlot, Mob};
use crate::inventory::container::Container as _;

fn test_allay() -> AllayEntity {
    init_vanilla_registry();
    AllayEntity::new(&vanilla_entities::ALLAY, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn duplication_cooldown_drives_can_duplicate() {
    let allay = test_allay();
    assert!(allay.can_duplicate());
    allay.reset_duplication_cooldown();
    assert!(!allay.can_duplicate());
    for _ in 0..DUPLICATION_COOLDOWN_TICKS {
        allay.update_duplication_cooldown();
    }
    assert!(allay.can_duplicate());
}

#[test]
fn jukebox_starts_and_stops_dancing() {
    let allay = test_allay();
    let jukebox = BlockPos::new(3, 64, 3);
    allay.set_jukebox_playing(jukebox, true);
    assert!(allay.is_dancing());
    // A different jukebox stopping does not stop the dance.
    allay.set_jukebox_playing(BlockPos::new(9, 64, 9), false);
    assert!(allay.is_dancing());
    allay.set_jukebox_playing(jukebox, false);
    assert!(!allay.is_dancing());
}

#[test]
fn items_are_equal_only_for_the_same_item() {
    init_vanilla_registry();
    let diamond = ItemStack::new(&vanilla_items::DIAMOND);
    let more_diamonds = ItemStack::with_count(&vanilla_items::DIAMOND, 5);
    let emerald = ItemStack::new(&vanilla_items::EMERALD);
    assert!(AllayEntity::allay_considers_item_equal(
        &diamond,
        &more_diamonds
    ));
    assert!(!AllayEntity::allay_considers_item_equal(&diamond, &emerald));
}

#[test]
fn empty_handed_allay_cannot_pick_up_loot() {
    let allay = test_allay();
    assert!(!allay.can_pick_up_loot());
    allay.set_item_slot(
        EquipmentSlot::MainHand,
        ItemStack::new(&vanilla_items::DIAMOND),
    );
    assert!(allay.has_item_in_hand());
    assert!(allay.can_pick_up_loot());
}

#[test]
fn inventory_and_cooldown_round_trip() {
    let allay = test_allay();
    allay
        .inventory
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::DIAMOND, 7));
    allay.set_duplication_cooldown(1234);
    let mut nbt = NbtCompound::new();
    allay.save_additional(&mut nbt);

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let parsed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("saved allay NBT should parse: {error}"));

    let loaded = test_allay();
    loaded.load_additional((&parsed).into());
    assert_eq!(loaded.state.lock().duplication_cooldown, 1234);
    assert!(!loaded.can_duplicate());
    let item = loaded.inventory.lock().get_item(0).clone();
    assert_eq!(item.count(), 7);
}

#[test]
fn removing_inventory_items_empties_it() {
    let allay = test_allay();
    allay
        .inventory
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::DIAMOND, 3));
    assert!(!allay.inventory_is_empty());
    let one = allay.remove_one_from_inventory();
    assert_eq!(one.count(), 1);
    let rest = allay.remove_all_inventory_items();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].count(), 2);
    assert!(allay.inventory_is_empty());
}

mod vibrations {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::{vanilla_entities, vanilla_game_events};
    use steel_utils::{BlockPos, ChunkPos, Downcast as _};

    use super::super::AllayEntity;
    use crate::bootstrap::init_globals;
    use crate::entity::ai::brain::memory_types::LIKED_NOTEBLOCK_POSITION;
    use crate::entity::{SharedEntity, next_entity_id};
    use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
    use crate::world::game_event::GameEventContext;
    use crate::world::game_event::vibrations::{VibrationSystem as _, tick_vibrations};

    fn spawned_allay(key: &'static str) -> (TestWorld, SharedEntity) {
        init_globals();
        let fixture = fresh_test_world(key);
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let allay: SharedEntity = Arc::new(AllayEntity::new(
            &vanilla_entities::ALLAY,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::clone(&allay))
            .unwrap_or_else(|error| panic!("allay should join the loaded chunk: {error}"));
        (fixture, allay)
    }

    fn as_allay(entity: &SharedEntity) -> &AllayEntity {
        entity
            .downcast_ref::<AllayEntity>()
            .unwrap_or_else(|| panic!("the entity is an allay"))
    }

    #[test]
    fn note_block_vibration_reaches_the_allay() {
        let (fixture, entity) = spawned_allay("allay_hears_note_block");
        let world = &fixture.world;
        let allay = as_allay(&entity);
        let note_block = BlockPos::new(11, 65, 8);
        let source = DVec3::new(11.5, 65.5, 8.5);

        world.game_event_at(
            &vanilla_game_events::NOTE_BLOCK_PLAY,
            source,
            &GameEventContext::default(),
        );
        let Some(candidate) = allay
            .vibration_data()
            .lock()
            .selection_strategy()
            .chosen_candidate(world.game_time() + 1)
        else {
            panic!("the allay listener should schedule the note block vibration");
        };
        assert_eq!(
            candidate.game_event().key,
            vanilla_game_events::NOTE_BLOCK_PLAY.key
        );

        allay
            .vibration_data()
            .lock()
            .set_current_vibration(Some(candidate));
        allay.vibration_data().lock().set_travel_time_in_ticks(1);
        tick_vibrations(world, allay);

        assert!(allay.vibration_data().lock().current_vibration().is_none());
        let liked = allay
            .brain_memories_lock()
            .get_ref(&LIKED_NOTEBLOCK_POSITION)
            .cloned();
        assert_eq!(liked.map(|global| global.pos), Some(note_block));
    }

    #[test]
    fn other_events_are_not_scheduled() {
        let (fixture, entity) = spawned_allay("allay_ignores_block_place");
        let world = &fixture.world;
        let allay = as_allay(&entity);
        world.game_event_at(
            &vanilla_game_events::BLOCK_PLACE,
            DVec3::new(11.5, 65.5, 8.5),
            &GameEventContext::default(),
        );
        assert!(
            allay
                .vibration_data()
                .lock()
                .selection_strategy()
                .chosen_candidate(i64::MAX)
                .is_none()
        );
    }

    #[test]
    fn jukebox_events_start_and_stop_the_dance() {
        let (fixture, entity) = spawned_allay("allay_dances_to_jukebox");
        let world = &fixture.world;
        let allay = as_allay(&entity);
        let jukebox = DVec3::new(10.5, 64.5, 8.5);

        world.game_event_at(
            &vanilla_game_events::JUKEBOX_PLAY,
            jukebox,
            &GameEventContext::default(),
        );
        assert!(allay.is_dancing());
        world.game_event_at(
            &vanilla_game_events::JUKEBOX_STOP_PLAY,
            jukebox,
            &GameEventContext::default(),
        );
        assert!(!allay.is_dancing());
    }
}
