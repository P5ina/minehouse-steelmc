use std::sync::Arc;

use glam::DVec3;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_entities,
    vanilla_mob_effects, vanilla_poi_types,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos};

use super::{
    ObtainRaidLeaderBannerGoal, PathfindToRaidGoal, RaiderCelebrationGoal,
    RaiderMoveThroughVillageGoal,
};
use crate::behavior::init_behaviors;
use crate::entity::ai::goal::selector::Goal as _;
use crate::entity::{
    Entity as _, EntitySpawnReason, LivingEntity as _, MobEffectInstance, PatrollingMonster,
    SharedEntity, create_entity_instance, init_entities, ominous_banner_instance,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::player::{Player, ResetReason};
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_entity_ticking_full_chunk,
};
use crate::world::World;
use crate::world::raid::Raid;
use steel_registry::item_stack::ItemStack;

const CENTER: BlockPos = BlockPos::new(8, 64, 8);

/// A floored, loaded area with one claimed bed at [`CENTER`].
fn village_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    world.set_difficulty(Difficulty::Normal);
    for x in -3..=3 {
        for z in -3..=3 {
            insert_entity_ticking_full_chunk(world, ChunkPos::new(x, z));
        }
    }
    for x in -40..56 {
        for z in -40..56 {
            assert!(world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            ));
        }
    }
    let home = vanilla_poi_types::HOME
        .try_id()
        .expect("vanilla home POI type should be registered");
    let mut storage = world.poi_storage.lock();
    storage.add(CENTER, home, vanilla_poi_types::HOME.ticket_count);
    assert!(storage.reserve_ticket(CENTER));
    drop(storage);
    fixture
}

/// Starts a raid at the village and spawns its first wave.
fn raid_with_first_wave(world: &Arc<World>) -> (Arc<Player>, Arc<Raid>) {
    let player = TestPlayerBuilder::new(Arc::clone(world), "Hero", 1).build();
    player.snap_to(DVec3::new(8.5, 64.0, 8.5), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    let raid = world
        .raids()
        .create_or_extend_raid(world, &player, CENTER)
        .expect("raid");
    while raid.groups_spawned() < 1 {
        world.raids().tick(world);
    }
    (player, raid)
}

fn first_member(raid: &Raid, world: &World) -> SharedEntity {
    raid.all_raiders(world)
        .into_iter()
        .find(|entity| {
            entity
                .as_raider()
                .is_some_and(PatrollingMonster::can_be_leader)
        })
        .expect("a raider that can lead")
}

fn head_item(entity: &SharedEntity) -> ItemStack {
    entity
        .as_living_entity()
        .expect("living")
        .living_base()
        .equipment()
        .lock()
        .get_ref(EquipmentSlot::Head)
        .clone()
}

#[test]
fn raider_without_a_wave_leader_takes_the_ominous_banner() {
    let fixture = village_world("raider_goal_banner_pickup");
    let world = &fixture.world;
    let (_player, raid) = raid_with_first_wave(world);
    let entity = first_member(&raid, world);
    let raider = entity.as_raider().expect("raider");
    let mob = entity.as_pathfinder_mob().expect("pathfinder");

    raid.remove_leader(1);
    raider.set_patrol_leader(false);
    let _ = raider
        .living_base()
        .equipment()
        .lock()
        .set(EquipmentSlot::Head, ItemStack::empty());
    assert!(
        !ObtainRaidLeaderBannerGoal::cannot_pick_up_banner(mob),
        "no leader and no banner: the goal may run"
    );

    let banner = world
        .spawn_item(entity.position(), ominous_banner_instance())
        .expect("banner item");
    mob.pick_up_item(world, &banner);
    assert!(banner.is_removed());
    assert!(ItemStack::matches(
        &head_item(&entity),
        &ominous_banner_instance()
    ));
    assert_eq!(raid.leader(1), Some(entity.uuid()));
    assert!(raider.is_patrol_leader());
    assert!(raider.is_captain());
    assert!(ObtainRaidLeaderBannerGoal::cannot_pick_up_banner(mob));
}

#[test]
fn raider_ignores_the_banner_while_its_wave_has_a_leader() {
    let fixture = village_world("raider_goal_banner_leader");
    let world = &fixture.world;
    let (_player, raid) = raid_with_first_wave(world);
    assert!(raid.leader(1).is_some());
    let follower = raid
        .all_raiders(world)
        .into_iter()
        .find(|entity| Some(entity.uuid()) != raid.leader(1))
        .expect("a follower");
    let banner = world
        .spawn_item(follower.position(), ominous_banner_instance())
        .expect("banner item");
    let raider = follower.as_raider().expect("raider");
    assert!(!raider.pick_up_item_raider(world, &banner));
    assert!(!banner.is_removed());
    assert_ne!(raid.leader(1), Some(follower.uuid()));
}

#[test]
fn pathfind_to_raid_recruits_raiders_nearby() {
    let fixture = village_world("raider_goal_recruit");
    let world = &fixture.world;
    let (_player, raid) = raid_with_first_wave(world);
    let member = first_member(&raid, world);
    let recruit = create_entity_instance(
        world,
        &vanilla_entities::PILLAGER,
        member.position() + DVec3::new(2.0, 0.0, 0.0),
    )
    .expect("pillager");
    recruit
        .as_mob()
        .expect("mob")
        .finalize_spawn(world, EntitySpawnReason::Patrol, None);
    assert!(
        world
            .try_add_fresh_entity_with_passengers(Arc::clone(&recruit))
            .is_ok()
    );
    let recruit_raider = recruit.as_raider().expect("raider");
    assert!(recruit_raider.current_raid().is_none());

    PathfindToRaidGoal::recruit_nearby(member.as_pathfinder_mob().expect("pathfinder"), &raid);
    assert!(
        recruit_raider
            .current_raid()
            .is_some_and(|joined| Arc::ptr_eq(&joined, &raid))
    );
    assert_eq!(recruit_raider.wave(), raid.groups_spawned());
}

#[test]
fn move_through_village_picks_unvisited_homes() {
    let fixture = village_world("raider_goal_village");
    let world = &fixture.world;
    let (_player, raid) = raid_with_first_wave(world);
    let member = first_member(&raid, world);
    // Waves spawn up to 64 blocks out; the HOME search only reaches 48.
    member.snap_to(DVec3::new(20.5, 64.0, 20.5), 0.0, 0.0);
    let mob = member.as_pathfinder_mob().expect("pathfinder");
    let mut goal = RaiderMoveThroughVillageGoal::new(1.05, 1);
    assert!(goal.has_suitable_poi(mob));
    assert_eq!(goal.poi_pos, Some(CENTER));
    goal.visited.push(CENTER);
    assert!(!goal.has_suitable_poi(mob), "the only home was visited");
    goal.visited.extend([BlockPos::new(0, 0, 0); 2]);
    goal.update_visited();
    assert_eq!(goal.visited.len(), 2, "keeps the last two homes");
    assert!(goal.has_suitable_poi(mob), "the oldest visit was forgotten");
}

#[test]
fn raiders_celebrate_a_lost_raid() {
    let fixture = village_world("raider_goal_celebrate");
    let world = &fixture.world;
    let (_player, raid) = raid_with_first_wave(world);
    let member = first_member(&raid, world);
    let mob = member.as_pathfinder_mob().expect("pathfinder");
    let raider = member.as_raider().expect("raider");
    let mut goal = RaiderCelebrationGoal;
    assert!(!goal.can_use(mob));

    world.poi_storage.lock().remove(CENTER);
    raid.tick(world);
    assert!(raid.is_loss());
    let _ = mob.set_target(None);
    assert!(goal.can_use(mob));
    goal.start(mob);
    assert!(raider.is_celebrating());
    goal.stop(mob);
    assert!(!raider.is_celebrating());
}
