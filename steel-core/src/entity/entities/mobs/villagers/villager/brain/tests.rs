use std::sync::Arc;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_entities, vanilla_items, vanilla_villager_professions,
    vanilla_villager_types, vanilla_world_clocks,
};
use steel_utils::types::InteractionHand;

use super::{ShowTradesToPlayer, VillagerPanicTrigger, hostile_distance, villager_calm_down};
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::memory_types::{
    GOLEM_DETECTED_RECENTLY, INTERACTION_TARGET, NEAREST_HOSTILE, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{Activity, BehaviorControl as _};
use crate::entity::entities::ZombieEntity;
use crate::entity::entities::mobs::villagers::VillagerEntity;
use crate::entity::entities::mobs::villagers::trading::MerchantOffer;
use crate::entity::{
    AgeableMob as _, Entity as _, LivingEntity as _, SharedEntity, next_entity_id,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestPlayerBuilder, fresh_test_world};
use crate::world::World;

fn villager_in(world: &Arc<World>) -> VillagerEntity {
    VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        DVec3::new(1.0, 64.0, 1.0),
        Arc::downgrade(world),
    )
}

fn set_time(world: &World, ticks: i64) {
    assert_eq!(
        world.set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, ticks),
        Some(())
    );
}

#[test]
fn adults_idle_without_a_job_and_rest_at_night() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_schedule");
    let world = &fixture.world;
    let mob = villager_in(world);

    set_time(world, 2_000);
    mob.refresh_brain();
    // Work needs a job site, so the villager falls back to idling.
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::IDLE));
    assert!(mob.brain.is_active(Activity::CORE));

    set_time(world, 13_000);
    mob.refresh_brain();
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::REST));
}

#[test]
fn babies_follow_the_baby_schedule() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_baby");
    let world = &fixture.world;
    let mob = villager_in(world);

    set_time(world, 3_000);
    mob.set_age(mob.get_baby_start_age());
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::PLAY));

    set_time(world, 6_000);
    mob.set_age(0);
    assert_eq!(
        mob.brain.active_non_core_activity(),
        Some(Activity::IDLE),
        "growing up swaps play for the adult schedule"
    );
}

#[test]
fn refreshing_the_brain_keeps_only_saved_memories() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_refresh");
    let world = &fixture.world;
    let mob = villager_in(world);
    let zombie: SharedEntity = Arc::new(ZombieEntity::new(
        &vanilla_entities::ZOMBIE,
        next_entity_id(),
        DVec3::new(2.0, 64.0, 2.0),
        Arc::downgrade(world),
    ));
    {
        let mut memories = mob.brain.memories().lock();
        memories.set(&GOLEM_DETECTED_RECENTLY, Some(true));
        memories.set(&NEAREST_HOSTILE, Some(zombie));
    }

    mob.refresh_brain();

    let memories = mob.brain.memories().lock();
    assert_eq!(memories.get(&GOLEM_DETECTED_RECENTLY), Some(true));
    assert!(!memories.has_value(&NEAREST_HOSTILE));
}

#[test]
fn hostiles_make_villagers_panic_until_they_calm_down() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_panic");
    let world = &fixture.world;
    set_time(world, 2_000);
    let mob = villager_in(world);
    mob.refresh_brain();
    let zombie: SharedEntity = Arc::new(ZombieEntity::new(
        &vanilla_entities::ZOMBIE,
        next_entity_id(),
        DVec3::new(2.0, 64.0, 2.0),
        Arc::downgrade(world),
    ));
    {
        let mut memories = mob.brain.memories().lock();
        memories.set(&NEAREST_HOSTILE, Some(zombie));
        memories.set(
            &WALK_TARGET,
            Some(WalkTarget::to_point(DVec3::new(5.0, 64.0, 5.0), 0.5, 0)),
        );
    }

    let mut trigger = TimedBehavior::new(VillagerPanicTrigger);
    assert!(trigger.try_start(world, &mob, world.game_time()));
    assert!(mob.brain.is_active(Activity::PANIC));
    assert!(
        !mob.brain.memories().lock().has_value(&WALK_TARGET),
        "starting to panic drops the old walk target"
    );

    let mut calm_down = villager_calm_down();
    assert!(calm_down.try_start(world, &mob, world.game_time()));
    assert!(mob.brain.is_active(Activity::PANIC), "still scared");

    mob.brain.memories().lock().erase(&NEAREST_HOSTILE);
    // Vanilla only rereads the schedule once a second.
    let mut calm_down = villager_calm_down();
    mob.brain.replace_with(super::make_brain(false));
    mob.brain.set_schedule(super::schedule_for(false));
    mob.brain.set_active_activity_if_possible(Activity::PANIC);
    assert!(calm_down.try_start(world, &mob, world.game_time()));
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::IDLE));
}

#[test]
fn villagers_fear_hostiles_from_their_own_distance() {
    init_vanilla_registry();
    assert_eq!(hostile_distance(&vanilla_entities::ZOMBIE), Some(8.0));
    assert_eq!(hostile_distance(&vanilla_entities::PILLAGER), Some(15.0));
    assert_eq!(hostile_distance(&vanilla_entities::EVOKER), Some(12.0));
    assert_eq!(hostile_distance(&vanilla_entities::CREEPER), None);
    assert_eq!(hostile_distance(&vanilla_entities::SKELETON), None);
}

#[test]
fn villagers_show_the_trades_a_player_can_pay_for() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_show_trades");
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Shopper", 21).build();
    let mob = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        player.position() + DVec3::new(1.0, 0.0, 1.0),
        Arc::downgrade(world),
    );
    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    let held = ItemStack::with_count(&vanilla_items::WHEAT, 20);
    player
        .inventory
        .lock()
        .set_item_in_hand(InteractionHand::MainHand, held.clone());
    let expected = mob.with_offers(|offers| {
        offers
            .iter()
            .find(|offer| {
                !offer.is_out_of_stock()
                    && (ItemStack::is_same_item(&held, &offer.cost_a())
                        || ItemStack::is_same_item(&held, &offer.cost_b()))
            })
            .map(MerchantOffer::assemble)
    });
    let player_entity: SharedEntity = player;
    mob.brain
        .memories()
        .lock()
        .set(&INTERACTION_TARGET, Some(player_entity));

    let mut show = ShowTradesToPlayer::new();
    assert!(show.check_extra_start_conditions(world, &mob));
    show.start(world, &mob, 0);
    show.tick(world, &mob, 1);

    let mut shown = ItemStack::empty();
    mob.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| shown = stack.clone());
    match expected {
        Some(expected) => {
            assert!(ItemStack::is_same_item(&shown, &expected));
            assert_eq!(show.look_time, 899, "a matching trade holds the gaze");
        }
        None => assert!(shown.is_empty()),
    }

    show.stop(world, &mob, 2);
    let mut cleared = ItemStack::empty();
    mob.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| {
        cleared = stack.clone();
    });
    assert!(cleared.is_empty());
    assert!(!mob.brain.memories().lock().has_value(&INTERACTION_TARGET));
}
