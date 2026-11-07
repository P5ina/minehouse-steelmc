use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, DoubleBlockHalf};
use steel_registry::item_stack::ItemStack;
use steel_registry::poi::PointOfInterestType;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items,
    vanilla_poi_types, vanilla_villager_professions, vanilla_villager_types, vanilla_world_clocks,
};
use steel_utils::types::InteractionHand;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, GlobalPos};

use super::poi::{
    GoToPotentialJobSite, acquirable_job_site, assign_profession_from_job_site, held_job_site,
    poi_competitor_scan, reset_profession,
};
use super::work::{SecondaryPoiSensor, WorkAtPoi, stroll_to_poi_list};
use super::{ShowTradesToPlayer, VillagerPanicTrigger, hostile_distance, villager_calm_down};
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::behavior::{Behavior, TimedBehavior};
use crate::entity::ai::brain::behaviors::{interact_with_door, stroll_to_poi, validate_nearby_poi};
use crate::entity::ai::brain::memory_types::{
    DOORS_TO_CLOSE, GOLEM_DETECTED_RECENTLY, HOME, INTERACTION_TARGET, JOB_SITE,
    LAST_WORKED_AT_POI, LOOK_TARGET, MEETING_POINT, NEAREST_HOSTILE, NEAREST_LIVING_ENTITIES, PATH,
    POTENTIAL_JOB_SITE, SECONDARY_JOB_SITE, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{Activity, BehaviorControl as _, Sensor as _};
use crate::entity::ai::node::Node;
use crate::entity::ai::path::Path;
use crate::entity::entities::ZombieEntity;
use crate::entity::entities::mobs::villagers::VillagerEntity;
use crate::entity::entities::mobs::villagers::trading::MerchantOffer;
use crate::entity::{
    AgeableMob as _, Entity as _, LivingEntity as _, SharedEntity, next_entity_id,
};
use crate::inventory::container::Container as _;
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestPlayerBuilder, fresh_test_world, insert_ready_full_chunk};
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
