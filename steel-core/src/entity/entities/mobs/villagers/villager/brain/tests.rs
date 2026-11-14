use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BedPart, BlockStateProperties, DoubleBlockHalf};
use steel_registry::item_stack::ItemStack;
use steel_registry::poi::PointOfInterestType;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_entities,
    vanilla_game_rules, vanilla_items, vanilla_mob_effects, vanilla_poi_types,
    vanilla_villager_professions, vanilla_villager_types, vanilla_world_clocks,
};
use steel_utils::types::InteractionHand;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, ChunkPos, GlobalPos};

use super::farm::{HarvestFarmland, UseBonemeal};
use super::poi::{
    GoToPotentialJobSite, acquirable_job_site, assign_profession_from_job_site, held_job_site,
    poi_competitor_scan, reset_profession,
};
use super::sharing::{GiveGiftToHero, gift_loot_table, throw_half_stack};
use super::sleep::{JumpOnBed, SleepInBed};
use super::work::{SecondaryPoiSensor, WorkAtPoi, stroll_to_poi_list};
use super::{
    ShowTradesToPlayer, VillagerBabiesSensor, VillagerHostilesSensor, VillagerPanicTrigger,
    hostile_distance, villager_calm_down,
};
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    go_to_wanted_item, interact_with_door, set_look_and_interact, stroll_to_poi,
    validate_nearby_poi,
};
use crate::entity::ai::brain::memory_types::{
    DOORS_TO_CLOSE, GOLEM_DETECTED_RECENTLY, HOME, INTERACTION_TARGET, JOB_SITE, LAST_SLEPT,
    LAST_WOKEN, LAST_WORKED_AT_POI, LOOK_TARGET, MEETING_POINT, NEAREST_BED, NEAREST_HOSTILE,
    NEAREST_LIVING_ENTITIES, NEAREST_VISIBLE_LIVING_ENTITIES, NEAREST_VISIBLE_PLAYER,
    NEAREST_VISIBLE_WANTED_ITEM, PATH, POTENTIAL_JOB_SITE, SECONDARY_JOB_SITE, WALK_TARGET,
    WalkTarget,
};
use crate::entity::ai::brain::nearest_visible::NearestVisibleLivingEntities;
use crate::entity::ai::brain::sensors::NearestItemSensor;
use crate::entity::ai::brain::{Activity, BehaviorControl as _, Sensor as _};
use crate::entity::ai::gossip::ReputationEventType;
use crate::entity::ai::node::Node;
use crate::entity::ai::path::Path;
use crate::entity::entities::ItemEntity;
use crate::entity::entities::ZombieEntity;
use crate::entity::entities::mobs::villagers::VillagerEntity;
use crate::entity::entities::mobs::villagers::trading::MerchantOffer;
use crate::entity::{
    AgeableMob as _, Entity as _, LivingEntity as _, Mob as _, MobEffectInstance, SharedEntity,
    next_entity_id,
};
use crate::inventory::container::Container as _;
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};
use crate::world::World;
use steel_utils::{Downcast as _, WorldAabb};

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
    mob.brain
        .replace_with(super::make_brain(false, mob.profession()));
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

fn poi_id(poi_type: &'static PointOfInterestType) -> usize {
    poi_type.try_id().expect("vanilla POI types are registered")
}

/// Adds a `poi_type` POI at `pos` to the world.
fn add_poi(world: &World, pos: BlockPos, poi_type: &'static PointOfInterestType) {
    world
        .poi_storage
        .lock()
        .add(pos, poi_id(poi_type), poi_type.ticket_count);
}

fn global(world: &World, pos: BlockPos) -> GlobalPos {
    GlobalPos::new(world.key.clone(), pos)
}

#[test]
fn professions_hold_the_workstation_of_their_name() {
    init_vanilla_registry();
    let composter = poi_id(&vanilla_poi_types::FARMER);
    let lectern = poi_id(&vanilla_poi_types::LIBRARIAN);
    let bed = poi_id(&vanilla_poi_types::HOME);
    let farmer = &vanilla_villager_professions::FARMER;
    let none = &vanilla_villager_professions::NONE;
    let nitwit = &vanilla_villager_professions::NITWIT;

    assert!(held_job_site(farmer, composter));
    assert!(!held_job_site(farmer, lectern));
    assert!(acquirable_job_site(farmer, composter));
    assert!(!acquirable_job_site(farmer, lectern));
    assert!(acquirable_job_site(none, composter) && acquirable_job_site(none, lectern));
    assert!(!acquirable_job_site(none, bed), "beds are no job");
    assert!(!held_job_site(none, composter));
    assert!(!acquirable_job_site(nitwit, composter) && !held_job_site(nitwit, composter));
}

#[test]
fn unemployed_villagers_take_the_job_at_the_site_they_reach() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_assign_profession");
    let world = &fixture.world;
    let mob = villager_in(world);
    let site = mob.block_position();
    add_poi(world, site, &vanilla_poi_types::LIBRARIAN);
    mob.brain
        .memories()
        .lock()
        .set(&POTENTIAL_JOB_SITE, Some(global(world, site)));

    let mut assign = assign_profession_from_job_site();
    assert!(assign.try_start(world, &mob, world.game_time()));

    assert_eq!(
        mob.profession().key,
        vanilla_villager_professions::LIBRARIAN.key
    );
    assert_eq!(
        mob.brain.memories().lock().get(&JOB_SITE),
        Some(global(world, site))
    );
    assert!(!mob.brain.memories().lock().has_value(&POTENTIAL_JOB_SITE));
    assert!(mob.state.lock().brain_refresh_pending);
}

#[test]
fn villagers_stay_put_until_they_reach_the_job_site() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_assign_far");
    let world = &fixture.world;
    let mob = villager_in(world);
    let site = mob.block_position().offset(5, 0, 0);
    add_poi(world, site, &vanilla_poi_types::LIBRARIAN);
    mob.brain
        .memories()
        .lock()
        .set(&POTENTIAL_JOB_SITE, Some(global(world, site)));

    let mut assign = assign_profession_from_job_site();
    assert!(!assign.try_start(world, &mob, world.game_time()));
    assert_eq!(mob.profession().key, vanilla_villager_professions::NONE.key);
}

#[test]
fn villagers_who_never_traded_lose_their_job_with_the_site() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_reset_profession");
    let world = &fixture.world;
    let mob = villager_in(world);
    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );

    let mut reset = reset_profession();
    assert!(reset.try_start(world, &mob, world.game_time()));
    assert_eq!(mob.profession().key, vanilla_villager_professions::NONE.key);

    let veteran = villager_in(world);
    veteran.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    veteran.set_villager_xp(10);
    let mut reset = reset_profession();
    assert!(!reset.try_start(world, &veteran, world.game_time()));
    assert_eq!(
        veteran.profession().key,
        vanilla_villager_professions::FARMER.key
    );
}

#[test]
fn the_more_experienced_villager_keeps_a_shared_job_site() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_competitors");
    let world = &fixture.world;
    let site = BlockPos::new(1, 64, 1);
    add_poi(world, site, &vanilla_poi_types::FARMER);
    let rookie = villager_in(world);
    let veteran = Arc::new(villager_in(world));
    for villager in [&rookie, &*veteran] {
        villager.set_villager_data(
            &vanilla_villager_types::PLAINS,
            &vanilla_villager_professions::FARMER,
            1,
        );
        villager
            .brain
            .memories()
            .lock()
            .set(&JOB_SITE, Some(global(world, site)));
    }
    veteran.set_villager_xp(10);
    let veteran_entity: SharedEntity = Arc::clone(&veteran) as SharedEntity;
    rookie
        .brain
        .memories()
        .lock()
        .set(&NEAREST_LIVING_ENTITIES, Some(vec![veteran_entity]));

    let mut scan = poi_competitor_scan();
    assert!(scan.try_start(world, &rookie, world.game_time()));

    assert!(!rookie.brain.memories().lock().has_value(&JOB_SITE));
    assert!(veteran.brain.memories().lock().has_value(&JOB_SITE));
}

#[test]
fn giving_up_on_a_potential_job_site_frees_it() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_go_to_job_site");
    let world = &fixture.world;
    let mob = villager_in(world);
    let site = BlockPos::new(8, 64, 8);
    add_poi(world, site, &vanilla_poi_types::FARMER);
    assert!(world.poi_storage.lock().reserve_ticket(site));
    mob.brain
        .memories()
        .lock()
        .set(&POTENTIAL_JOB_SITE, Some(global(world, site)));

    let mut go = GoToPotentialJobSite {
        speed_modifier: 0.5,
    };
    Behavior::tick(&mut go, world, &mob, 0);
    assert!(
        mob.brain.memories().lock().has_value(&WALK_TARGET),
        "walks to the job site"
    );
    Behavior::stop(&mut go, world, &mob, 1);

    assert!(!mob.brain.memories().lock().has_value(&POTENTIAL_JOB_SITE));
    assert!(!world.poi_storage.lock().is_occupied(site));
}

#[test]
fn nearby_pois_that_vanished_are_forgotten() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_validate_poi");
    let world = &fixture.world;
    let mob = villager_in(world);
    let bell = BlockPos::new(3, 64, 3);
    mob.brain
        .memories()
        .lock()
        .set(&HOME, Some(global(world, bell)));

    let mut validate = validate_nearby_poi(super::is_home, &HOME);
    assert!(validate.try_start(world, &mob, world.game_time()));
    assert!(!mob.brain.memories().lock().has_value(&HOME));

    let far = BlockPos::new(40, 64, 40);
    mob.brain
        .memories()
        .lock()
        .set(&HOME, Some(global(world, far)));
    let mut validate = validate_nearby_poi(super::is_home, &HOME);
    assert!(
        !validate.try_start(world, &mob, world.game_time()),
        "too far away to check"
    );
    assert!(mob.brain.memories().lock().has_value(&HOME));
}

#[test]
fn villagers_free_their_pois_and_remember_them_across_refreshes() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_release_pois");
    let world = &fixture.world;
    let mob = villager_in(world);
    let bed = BlockPos::new(2, 64, 2);
    add_poi(world, bed, &vanilla_poi_types::HOME);
    assert!(world.poi_storage.lock().reserve_ticket(bed));
    mob.brain
        .memories()
        .lock()
        .set(&HOME, Some(global(world, bed)));

    mob.refresh_brain();
    assert_eq!(
        mob.brain.memories().lock().get(&HOME),
        Some(global(world, bed)),
        "homes are saved with the brain"
    );

    mob.release_all_pois();
    assert!(!world.poi_storage.lock().is_occupied(bed));
}

/// A villager standing at `(x, 64, 1)` with a path along +x whose next
/// node index is `next`.
fn walk_path(villager: &VillagerEntity, next: usize) {
    let nodes = (0..6).map(|x| Node::new(x, 64, 1)).collect();
    let mut path = Path::new(nodes, BlockPos::new(5, 64, 1), true);
    path.set_next_node_index(next);
    villager.brain.memories().lock().set(&PATH, Some(path));
}

#[test]
fn villagers_open_doors_on_their_path_and_close_them_behind() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_doors");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let door = BlockPos::new(2, 64, 1);
    let half = &BlockStateProperties::DOUBLE_BLOCK_HALF;
    let lower = vanilla_blocks::OAK_DOOR
        .default_state()
        .set_value(half, DoubleBlockHalf::Lower);
    world.set_block(door, lower, UpdateFlags::UPDATE_CLIENTS);
    world.set_block(
        door.above(),
        lower.set_value(half, DoubleBlockHalf::Upper),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let is_open = || {
        world
            .get_block_state(door)
            .get_value(&BlockStateProperties::OPEN)
    };
    let mob = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        DVec3::new(1.5, 64.0, 1.5),
        Arc::downgrade(world),
    );
    mob.brain
        .replace_with(super::make_brain(false, mob.profession()));

    // Walking from (1, 64, 1) into the door.
    walk_path(&mob, 2);
    let mut doors = interact_with_door();
    assert!(doors.try_start(world, &mob, 0));
    assert!(is_open(), "the door on the next node opens");
    assert_eq!(
        mob.brain
            .memories()
            .lock()
            .get(&DOORS_TO_CLOSE)
            .map(|doors| doors.len()),
        Some(1)
    );

    // Through it, from (3, 64, 1) on.
    mob.base().set_position_local(DVec3::new(3.5, 64.0, 1.5));
    walk_path(&mob, 4);
    assert!(doors.try_start(world, &mob, 1));
    assert!(!is_open(), "the door it passed closes again");
    assert_eq!(
        mob.brain
            .memories()
            .lock()
            .get(&DOORS_TO_CLOSE)
            .map(|doors| doors.len()),
        Some(0)
    );
}

#[test]
fn villagers_stroll_back_to_the_poi_they_are_near() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_stroll_to_poi");
    let world = &fixture.world;
    let mob = villager_in(world);
    mob.brain
        .replace_with(super::make_brain(false, mob.profession()));
    let site = BlockPos::new(4, 64, 1);
    mob.brain
        .memories()
        .lock()
        .set(&JOB_SITE, Some(global(world, site)));

    let mut stroll = stroll_to_poi(&JOB_SITE, 0.4, 1, 10);
    assert!(stroll.try_start(world, &mob, 100));
    let target = mob
        .brain
        .memories()
        .lock()
        .get(&WALK_TARGET)
        .map(|target| target.target.current_block_position());
    assert_eq!(target, Some(site));

    mob.brain.memories().lock().erase(&WALK_TARGET);
    let mut stroll = stroll_to_poi(&JOB_SITE, 0.4, 1, 2);
    assert!(
        !stroll.try_start(world, &mob, 200),
        "too far from the job site"
    );
}

#[test]
fn employed_villagers_work_and_meet_at_the_bell_by_day() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_work_meet");
    let world = &fixture.world;
    let mob = villager_in(world);
    {
        let mut memories = mob.brain.memories().lock();
        memories.set(&JOB_SITE, Some(global(world, BlockPos::new(3, 64, 3))));
        memories.set(
            &MEETING_POINT,
            Some(global(world, BlockPos::new(-3, 64, -3))),
        );
    }

    set_time(world, 2_000);
    mob.refresh_brain();
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::WORK));

    set_time(world, 9_500);
    mob.refresh_brain();
    assert_eq!(mob.brain.active_non_core_activity(), Some(Activity::MEET));
}

fn farmer_in(world: &Arc<World>) -> VillagerEntity {
    let mob = villager_in(world);
    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    mob
}

fn use_up_first_offer(mob: &VillagerEntity) {
    mob.with_offers(|offers| {
        let offer = offers.iter_mut().next().expect("farmers have trades");
        while !offer.is_out_of_stock() {
            offer.increase_uses();
        }
    });
}

fn first_offer_uses(mob: &VillagerEntity) -> i32 {
    mob.with_offers(|offers| offers.iter().next().map_or(0, MerchantOffer::uses))
}

#[test]
fn villagers_restock_twice_a_day_and_again_the_next_day() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_restock");
    let world = &fixture.world;
    let mob = farmer_in(world);
    set_time(world, 24_000);

    assert!(
        !mob.should_restock(world),
        "nothing sold, nothing to restock"
    );
    use_up_first_offer(&mob);
    assert!(mob.should_restock(world));
    mob.restock(world.game_time());
    assert_eq!(first_offer_uses(&mob), 0);

    use_up_first_offer(&mob);
    assert!(
        !mob.should_restock(world),
        "the second restock of a day waits two minutes"
    );

    set_time(world, 48_000);
    assert!(
        !mob.should_restock(world),
        "a new day catches up on the missed restock, so nothing is left to refill"
    );
    assert_eq!(first_offer_uses(&mob), 0);
    use_up_first_offer(&mob);
    assert!(
        mob.should_restock(world),
        "the new day's count starts again from zero"
    );
}

#[test]
fn farmers_bake_bread_at_the_composter() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_bake_bread");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let composter = BlockPos::new(2, 64, 1);
    world.set_block(
        composter,
        vanilla_blocks::COMPOSTER.default_state(),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let mob = farmer_in(world);
    mob.brain
        .memories()
        .lock()
        .set(&JOB_SITE, Some(global(world, composter)));
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::WHEAT, 10));

    let mut work = WorkAtPoi::at_composter();
    work.start(world, &mob, 5);

    let inventory = mob.inventory().lock();
    assert_eq!(inventory.count_item(&vanilla_items::BREAD), 3);
    assert_eq!(inventory.count_item(&vanilla_items::WHEAT), 1);
    drop(inventory);
    let memories = mob.brain.memories().lock();
    assert_eq!(
        memories.get(&LAST_WORKED_AT_POI),
        None,
        "like vanilla, no villager behavior or sensor registers this memory"
    );
    assert_eq!(
        memories
            .get(&LOOK_TARGET)
            .map(|target| target.current_block_position()),
        Some(composter)
    );
}

#[test]
fn farmers_find_farmland_and_stroll_between_it() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_secondary_poi");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let farmland = BlockPos::new(3, 63, 2);
    world.set_block(
        farmland,
        vanilla_blocks::FARMLAND.default_state(),
        UpdateFlags::UPDATE_CLIENTS,
    );

    let librarian = villager_in(world);
    librarian.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::LIBRARIAN,
        1,
    );
    SecondaryPoiSensor.do_tick(world, &librarian);
    assert!(
        !librarian
            .brain
            .memories()
            .lock()
            .has_value(&SECONDARY_JOB_SITE)
    );

    let mob = farmer_in(world);
    SecondaryPoiSensor.do_tick(world, &mob);
    assert_eq!(
        mob.brain.memories().lock().get(&SECONDARY_JOB_SITE),
        Some(vec![global(world, farmland)])
    );

    mob.brain
        .memories()
        .lock()
        .set(&JOB_SITE, Some(global(world, BlockPos::new(2, 64, 1))));
    let mut stroll = stroll_to_poi_list(&SECONDARY_JOB_SITE, 0.5, 1, 6, &JOB_SITE);
    assert!(stroll.try_start(world, &mob, 200));
    let target = mob
        .brain
        .memories()
        .lock()
        .get(&WALK_TARGET)
        .map(|target| target.target.current_block_position());
    assert_eq!(target, Some(farmland));
}

#[test]
fn farmers_empty_a_ready_composter_and_compost_spare_seeds() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_compost");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let composter = BlockPos::new(2, 64, 1);
    let level = &BlockStateProperties::LEVEL_COMPOSTER;
    world.set_block(
        composter,
        vanilla_blocks::COMPOSTER
            .default_state()
            .set_value(level, 8),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let mob = farmer_in(world);
    mob.brain
        .memories()
        .lock()
        .set(&JOB_SITE, Some(global(world, composter)));
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::WHEAT_SEEDS, 15));

    WorkAtPoi::at_composter().start(world, &mob, 5);

    assert_eq!(
        mob.inventory()
            .lock()
            .count_item(&vanilla_items::WHEAT_SEEDS),
        10,
        "farmers keep ten seeds of each kind"
    );
    let filled = world.get_block_state(composter).get_value(level);
    assert!(
        (1..=5).contains(&filled),
        "the emptied composter takes the five spare seeds, level {filled}"
    );
}

/// Farmland at `(1, 63, 1)` under the villager, with `crop` on it.
fn farm_world(name: &'static str, crop: BlockStateId) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture.world.set_block(
        BlockPos::new(1, 63, 1),
        vanilla_blocks::FARMLAND.default_state(),
        UpdateFlags::UPDATE_CLIENTS,
    );
    fixture
        .world
        .set_block(BlockPos::new(1, 64, 1), crop, UpdateFlags::UPDATE_CLIENTS);
    fixture
}

fn wheat(age: u8) -> BlockStateId {
    vanilla_blocks::WHEAT
        .default_state()
        .set_value(&BlockStateProperties::AGE_7, age)
}

fn main_hand(mob: &VillagerEntity) -> ItemStack {
    let mut held = ItemStack::empty();
    mob.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| held = stack.clone());
    held
}

#[test]
fn farmers_harvest_grown_wheat_and_plant_seeds() {
    let fixture = farm_world("villager_brain_harvest", wheat(7));
    let world = &fixture.world;
    let crop = BlockPos::new(1, 64, 1);
    let mob = farmer_in(world);
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::WHEAT_SEEDS, 5));
    let mut harvest = HarvestFarmland::new();

    assert!(harvest.check_extra_start_conditions(world, &mob));
    harvest.start(world, &mob, 100);
    assert!(mob.brain.memories().lock().has_value(&WALK_TARGET));

    harvest.tick(world, &mob, 100);
    assert!(
        world.get_block_state(crop).is_air(),
        "the grown wheat broke"
    );
    harvest.tick(world, &mob, 101);
    assert_eq!(world.get_block_state(crop), wheat(0), "seeds went in");
    assert_eq!(
        mob.inventory()
            .lock()
            .count_item(&vanilla_items::WHEAT_SEEDS),
        4
    );

    harvest.stop(world, &mob, 102);
    assert!(!mob.brain.memories().lock().has_value(&WALK_TARGET));
    assert!(
        !harvest.check_extra_start_conditions(world, &mob),
        "nothing left to harvest or plant"
    );
}

#[test]
fn only_farmers_harvest_and_only_with_mob_griefing() {
    let fixture = farm_world("villager_brain_harvest_rules", wheat(7));
    let world = &fixture.world;
    let mut harvest = HarvestFarmland::new();

    assert!(!harvest.check_extra_start_conditions(world, &villager_in(world)));
    assert!(world.set_game_rule(&vanilla_game_rules::MOB_GRIEFING, false));
    assert!(!harvest.check_extra_start_conditions(world, &farmer_in(world)));
}

#[test]
fn villagers_use_bone_meal_on_growing_crops() {
    let fixture = farm_world("villager_brain_bone_meal", wheat(0));
    let world = &fixture.world;
    let crop = BlockPos::new(1, 64, 1);
    let mob = villager_in(world);
    let mut bone_meal = UseBonemeal::new();
    assert!(
        !bone_meal.check_extra_start_conditions(world, &mob),
        "no bone meal to use"
    );
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::BONE_MEAL, 3));

    assert!(bone_meal.check_extra_start_conditions(world, &mob));
    bone_meal.start(world, &mob, 10);
    assert!(main_hand(&mob).is(&vanilla_items::BONE_MEAL));

    bone_meal.tick(world, &mob, 10);
    assert!(
        world
            .get_block_state(crop)
            .get_value(&BlockStateProperties::AGE_7)
            > 0,
        "the wheat grew"
    );
    assert_eq!(
        mob.inventory().lock().count_item(&vanilla_items::BONE_MEAL),
        2
    );
    bone_meal.tick(world, &mob, 20);
    assert_eq!(
        mob.inventory().lock().count_item(&vanilla_items::BONE_MEAL),
        2,
        "forty ticks between uses"
    );

    bone_meal.stop(world, &mob, 21);
    assert!(main_hand(&mob).is_empty());
}

#[test]
fn villagers_see_and_walk_to_wanted_items() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_wanted_item");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let mob = villager_in(world);
    let _stone = world
        .spawn_item(
            DVec3::new(3.5, 64.0, 1.5),
            ItemStack::new(&vanilla_items::STONE),
        )
        .expect("the stone spawns");
    NearestItemSensor.do_tick(world, &mob);
    assert!(
        !mob.brain
            .memories()
            .lock()
            .has_value(&NEAREST_VISIBLE_WANTED_ITEM),
        "villagers do not want stone"
    );

    let _wheat = world
        .spawn_item(
            DVec3::new(4.5, 64.0, 1.5),
            ItemStack::new(&vanilla_items::WHEAT),
        )
        .expect("the wheat spawns");
    NearestItemSensor.do_tick(world, &mob);
    assert!(
        mob.brain
            .memories()
            .lock()
            .has_value(&NEAREST_VISIBLE_WANTED_ITEM)
    );
    let mut go = go_to_wanted_item(|_: &VillagerEntity| true, 0.5, false, 4);
    assert!(go.try_start(world, &mob, 1));
    assert!(mob.brain.memories().lock().has_value(&WALK_TARGET));
}

/// Runs `check` on its own thread and fails instead of hanging the suite if it deadlocks.
fn assert_finishes(name: &str, check: impl FnOnce() + Send + 'static) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        check();
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(10)).is_ok(),
        "{name} deadlocked on the villager's own brain memories"
    );
}

/// Seeds the visible entities with a zombie next to the villager. Its visibility is not
/// cached yet, so a search runs the targeting test, which reads the villager's memories.
fn villager_seeing_a_zombie(world: &Arc<World>) -> Arc<VillagerEntity> {
    let mob = Arc::new(villager_in(world));
    mob.refresh_brain();
    let body: SharedEntity = Arc::clone(&mob) as _;
    let zombie: SharedEntity = Arc::new(ZombieEntity::new(
        &vanilla_entities::ZOMBIE,
        next_entity_id(),
        DVec3::new(2.0, 64.0, 2.0),
        Arc::downgrade(world),
    ));
    mob.brain.memories().lock().set(
        &NEAREST_VISIBLE_LIVING_ENTITIES,
        Some(NearestVisibleLivingEntities::new(
            world,
            &body,
            vec![zombie],
        )),
    );
    mob
}

#[test]
fn visible_entity_searches_do_not_hold_the_memories_lock() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_visible_search_lock");
    let world = Arc::clone(&fixture.world);

    let mob = villager_seeing_a_zombie(&world);
    let sensor_world = Arc::clone(&world);
    assert_finishes("VillagerHostilesSensor", move || {
        VillagerHostilesSensor.do_tick(&sensor_world, &mob);
    });

    let mob = villager_seeing_a_zombie(&world);
    let sensor_world = Arc::clone(&world);
    assert_finishes("VillagerBabiesSensor", move || {
        VillagerBabiesSensor.do_tick(&sensor_world, &mob);
    });

    let mob = villager_seeing_a_zombie(&world);
    let behavior_world = Arc::clone(&world);
    assert_finishes("set_look_and_interact", move || {
        let mut look = set_look_and_interact::<VillagerEntity>(|_| true, 8);
        let _ = look.try_start(&behavior_world, &mob, behavior_world.game_time());
    });
}

fn item_entities(world: &Arc<World>) -> Vec<ItemStack> {
    world
        .get_entities_in_aabb(&WorldAabb::new(-64.0, -64.0, -64.0, 64.0, 320.0, 64.0))
        .into_iter()
        .filter_map(|entity| {
            entity
                .downcast_ref::<ItemEntity>()
                .map(ItemEntity::get_item)
        })
        .collect()
}

#[test]
fn villagers_gossip_at_most_once_a_minute_each() {
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_gossip");
    let world = &fixture.world;
    let player = uuid::Uuid::from_u128(7);
    let speaker = villager_in(world);
    let listener = villager_in(world);
    speaker.on_reputation_event_from(ReputationEventType::VillagerHurt, player);

    // Like vanilla, a fresh villager first gossips a minute into the game.
    listener.gossip(&speaker, 1_199);
    assert_eq!(listener.player_reputation(player), 0);
    listener.gossip(&speaker, 2_000);
    let heard = listener.player_reputation(player);
    assert!(heard < 0, "the listener heard about the attack");

    speaker.on_reputation_event_from(ReputationEventType::VillagerKilled, player);
    listener.gossip(&speaker, 2_000 + 1_199);
    assert_eq!(
        listener.player_reputation(player),
        heard,
        "too soon to talk"
    );
    listener.gossip(&speaker, 2_000 + 1_200);
    assert!(listener.player_reputation(player) < heard);
}

#[test]
fn villagers_throw_half_a_big_stack_or_what_is_over_24() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_throw_half");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let mob = villager_in(world);
    let bread = |count| ItemStack::with_count(&vanilla_items::BREAD, count);
    let count = |mob: &VillagerEntity| mob.inventory().lock().count_item(&vanilla_items::BREAD);
    let target = DVec3::new(4.0, 64.0, 1.0);

    let _ = mob.inventory().lock().add_item(&bread(20));
    throw_half_stack(&mob, VillagerEntity::is_villager_food, target);
    assert_eq!(count(&mob), 20, "20 bread is neither over half nor over 24");

    let _ = mob.inventory().lock().add_item(&bread(10));
    throw_half_stack(&mob, VillagerEntity::is_villager_food, target);
    assert_eq!(count(&mob), 24, "keeps 24 of 30");

    let _ = mob.inventory().lock().add_item(&bread(16));
    throw_half_stack(&mob, VillagerEntity::is_villager_food, target);
    assert_eq!(count(&mob), 20, "halves a stack of 40");
    let thrown: i32 = item_entities(world).iter().map(ItemStack::count).sum();
    assert_eq!(thrown, 26);

    assert!(mob.has_excess_food(), "20 bread is 80 food points");
    assert!(!mob.wants_more_food());
    assert!(villager_in(world).wants_more_food());
}

#[test]
fn gifts_come_from_the_profession_table() {
    use std::ptr;
    use steel_registry::loot_table::LootTableRef;
    use steel_registry::vanilla_loot_tables::{
        GAMEPLAY_HERO_OF_THE_VILLAGE_BABY_GIFT, GAMEPLAY_HERO_OF_THE_VILLAGE_FARMER_GIFT,
        GAMEPLAY_HERO_OF_THE_VILLAGE_UNEMPLOYED_GIFT,
    };
    let same = |a: LootTableRef, b: LootTableRef| ptr::eq(a, b);
    init_vanilla_registry();
    let fixture = fresh_test_world("villager_brain_gift_tables");
    let world = &fixture.world;
    let unemployed = villager_in(world);
    assert!(same(
        gift_loot_table(&unemployed),
        &GAMEPLAY_HERO_OF_THE_VILLAGE_UNEMPLOYED_GIFT
    ));
    assert!(same(
        gift_loot_table(&farmer_in(world)),
        &GAMEPLAY_HERO_OF_THE_VILLAGE_FARMER_GIFT
    ));
    unemployed.set_age(-24_000);
    assert!(same(
        gift_loot_table(&unemployed),
        &GAMEPLAY_HERO_OF_THE_VILLAGE_BABY_GIFT
    ));
}

#[test]
fn villagers_wait_then_toss_a_gift_to_a_hero() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_hero_gift");
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Hero", 22).build();
    let player_entity: SharedEntity = Arc::clone(&player) as SharedEntity;
    let near = player.position() + DVec3::new(2.0, 0.0, 0.0);
    insert_ready_full_chunk(
        world,
        ChunkPos::new(near.x.floor() as i32 >> 4, near.z.floor() as i32 >> 4),
    );
    let mob = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        near,
        Arc::downgrade(world),
    );
    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    mob.brain
        .memories()
        .lock()
        .set(&NEAREST_VISIBLE_PLAYER, Some(Arc::clone(&player_entity)));
    let mut gift = GiveGiftToHero::new();

    assert!(
        !gift.check_extra_start_conditions(world, &mob),
        "only heroes get gifts"
    );
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::HERO_OF_THE_VILLAGE,
        48_000,
        0
    )));
    for _ in 0..600 {
        assert!(!gift.check_extra_start_conditions(world, &mob));
    }
    assert!(gift.check_extra_start_conditions(world, &mob));

    gift.start(world, &mob, 1_000);
    assert!(mob.brain.memories().lock().has_value(&INTERACTION_TARGET));
    gift.tick(world, &mob, 1_020);
    assert!(item_entities(world).is_empty(), "the head is still turning");
    gift.tick(world, &mob, 1_021);
    assert!(!item_entities(world).is_empty(), "a farmer's gift flew out");
    assert!(!gift.can_still_use(world, &mob, 1_021));

    gift.stop(world, &mob, 1_021);
    assert!(!mob.brain.memories().lock().has_value(&INTERACTION_TARGET));
    assert!(
        !gift.check_extra_start_conditions(world, &mob),
        "waits again"
    );
}

#[test]
fn villagers_sleep_in_their_free_home_bed() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_sleep");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let bed = BlockPos::new(1, 64, 1);
    world.set_block(
        bed,
        vanilla_blocks::WHITE_BED
            .default_state()
            .set_value(&BlockStateProperties::BED_PART, BedPart::Head),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let sleeper = villager_in(world);
    let neighbor = villager_in(world);
    for villager in [&sleeper, &neighbor] {
        villager
            .brain
            .memories()
            .lock()
            .set(&HOME, Some(global(world, bed)));
    }
    let mut sleep = TimedBehavior::new(SleepInBed::new());

    assert!(sleep.try_start(world, &sleeper, 100));
    assert!(sleeper.is_sleeping());
    assert_eq!(sleeper.brain.memories().lock().get(&LAST_SLEPT), Some(100));
    assert!(
        world
            .get_block_state(bed)
            .get_value(&BlockStateProperties::OCCUPIED)
    );
    assert!(
        !SleepInBed::new().check_extra_start_conditions(world, &neighbor),
        "the bed is taken"
    );

    sleep.do_stop(world, &sleeper, 200);
    assert!(!sleeper.is_sleeping());
    assert!(sleeper.brain.memories().lock().has_value(&LAST_WOKEN));
    // Like vanilla, the behavior starts but lies down only 40 ticks later.
    assert!(sleep.try_start(world, &sleeper, 240));
    assert!(
        !sleeper.is_sleeping(),
        "forty ticks pass before it lies down again"
    );
    sleep.do_stop(world, &sleeper, 240);
    assert!(sleep.try_start(world, &sleeper, 241));
    assert!(sleeper.is_sleeping());
}

#[test]
fn villager_babies_jump_on_the_nearest_bed() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_brain_jump_on_bed");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let bed = BlockPos::new(1, 64, 1);
    world.set_block(
        bed,
        vanilla_blocks::WHITE_BED
            .default_state()
            .set_value(&BlockStateProperties::BED_PART, BedPart::Head),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let adult = villager_in(world);
    adult.brain.memories().lock().set(&NEAREST_BED, Some(bed));
    assert!(
        !JumpOnBed::new(0.5).check_extra_start_conditions(world, &adult),
        "only babies jump on beds"
    );

    let baby = villager_in(world);
    baby.set_age(baby.get_baby_start_age());
    baby.brain.memories().lock().set(&NEAREST_BED, Some(bed));
    let mut jump = TimedBehavior::new(JumpOnBed::new(0.5));
    assert!(jump.try_start(world, &baby, 0));
    assert!(baby.brain.memories().lock().has_value(&WALK_TARGET));

    let mut jumps = 0;
    let mut time = 1;
    while time < 100 {
        jump.tick_or_stop(world, &baby, time);
        let mut controls = baby.mob_base().controls().lock();
        if controls.jump_control.wants_jump() {
            jumps += 1;
            controls.jump_control.tick();
        }
        drop(controls);
        time += 1;
    }
    assert!(
        (3..=6).contains(&jumps),
        "three to six jumps, five ticks apart, then it stops: {jumps}"
    );
}
