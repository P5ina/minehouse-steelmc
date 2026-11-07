use std::io::Cursor;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::data_components::vanilla_components::OMINOUS_BOTTLE_AMPLIFIER;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_game_rules::RAIDS;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_damage_types,
    vanilla_enchantments, vanilla_entities, vanilla_items, vanilla_mob_effects, vanilla_poi_types,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};
use uuid::Uuid;

use super::{Raid, RaidStatus, Raids, SavedRaids, num_groups};
use crate::behavior::finish_consuming_stack;
use crate::behavior::init_behaviors;
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::{
    Entity as _, LivingEntity, MobEffectInstance, SharedEntity, create_entity_instance,
    init_entities, next_entity_id, ominous_banner_instance,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::player::{Player, ResetReason};
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_entity_ticking_full_chunk,
};
use crate::world::World;

const CENTER: BlockPos = BlockPos::new(8, 64, 8);

/// A loaded, floored overworld area with one claimed bed at [`CENTER`].
fn village_world(name: &'static str, difficulty: Difficulty) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    world.set_difficulty(difficulty);
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

fn player_at(world: &Arc<World>, pos: DVec3) -> Arc<Player> {
    let player = TestPlayerBuilder::new(Arc::clone(world), "Hero", next_entity_id()).build();
    player.snap_to(pos, 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    player
}

fn raider_entities(raid: &Raid, world: &World) -> Vec<SharedEntity> {
    raid.all_raiders(world)
}

fn kill_all(raid: &Raid, world: &World, player: &Arc<Player>) {
    for entity in raider_entities(raid, world) {
        let living = entity.as_living_entity().expect("raiders are living");
        let source = DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK)
            .with_causing_entity(Arc::clone(player) as SharedEntity);
        living.die(&source);
    }
}

#[test]
fn wave_counts_and_enchant_odds_follow_vanilla() {
    assert_eq!(num_groups(Difficulty::Peaceful), 0);
    assert_eq!(num_groups(Difficulty::Easy), 3);
    assert_eq!(num_groups(Difficulty::Normal), 5);
    assert_eq!(num_groups(Difficulty::Hard), 7);
    let raid = Raid::new(CENTER, Difficulty::Normal);
    assert_eq!(raid.num_groups(), 5);
    assert_eq!(raid.status(), RaidStatus::Ongoing);
    let odds: Vec<f32> = (0..=5)
        .map(|level| {
            raid.set_raid_omen_level(level);
            raid.enchant_odds()
        })
        .collect();
    assert_eq!(odds, [0.0, 0.0, 0.1, 0.25, 0.5, 0.75]);
}

#[test]
fn bad_omen_in_a_village_turns_into_raid_omen() {
    let fixture = village_world("raid_bad_omen", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::BAD_OMEN,
        1200,
        2
    )));
    player.tick_mob_effects();
    assert!(!player.has_mob_effect(vanilla_mob_effects::BAD_OMEN));
    let omen = player
        .living_base()
        .mob_effect(vanilla_mob_effects::RAID_OMEN)
        .expect("raid omen");
    assert_eq!(omen.amplifier(), 2);
    assert_eq!(player.raid_omen_position(), Some(BlockPos::new(8, 64, 8)));
}

#[test]
fn bad_omen_outside_a_village_stays() {
    let fixture = village_world("raid_bad_omen_far", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    world.poi_storage.lock().remove(CENTER);
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::BAD_OMEN,
        1200,
        0
    )));
    player.tick_mob_effects();
    assert!(player.has_mob_effect(vanilla_mob_effects::BAD_OMEN));
    assert!(!player.has_mob_effect(vanilla_mob_effects::RAID_OMEN));
    assert_eq!(player.raid_omen_position(), None);
}

#[test]
fn raid_omen_ending_starts_a_raid_at_the_village() {
    let fixture = village_world("raid_omen_start", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(10.5, 64.0, 10.5));
    player.set_raid_omen_position(BlockPos::new(10, 64, 10));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        1,
        1
    )));
    player.tick_mob_effects();
    assert_eq!(player.raid_omen_position(), None);
    let raid = world.get_raid_at(CENTER).expect("a raid started");
    assert_eq!(raid.center(), CENTER, "centered on the village POIs");
    assert_eq!(raid.raid_omen_level(), 2);
    assert_eq!(
        world.raids().get_id(&raid),
        Some(2),
        "vanilla ids start at 2"
    );
    assert!(world.is_raided(CENTER));
}

#[test]
fn raids_need_the_gamerule() {
    let fixture = village_world("raid_gamerule", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    assert!(world.set_game_rule(&RAIDS, false));
    assert!(
        world
            .raids()
            .create_or_extend_raid(world, &player, CENTER)
            .is_none()
    );

    assert!(world.set_game_rule(&RAIDS, true));
    let raid = world
        .raids()
        .create_or_extend_raid(world, &player, CENTER)
        .expect("raid");
    assert!(world.set_game_rule(&RAIDS, false));
    world.raids().tick(world);
    assert!(raid.is_stopped());
    world.raids().tick(world);
    assert!(world.raids().all().is_empty());
}

#[test]
fn peaceful_stops_a_raid() {
    let fixture = village_world("raid_peaceful", Difficulty::Normal);
    let world = &fixture.world;
    let raid = Arc::new(Raid::new(CENTER, Difficulty::Normal));
    world.set_difficulty(Difficulty::Peaceful);
    raid.tick(world);
    assert!(raid.is_stopped());
    assert!(!raid.is_active());
}

#[test]
fn first_wave_spawns_after_the_countdown() {
    let fixture = village_world("raid_first_wave", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    let raid = world
        .raids()
        .create_or_extend_raid(world, &player, CENTER)
        .expect("raid");
    for _ in 0..299 {
        world.raids().tick(world);
    }
    assert!(!raid.is_started(), "five seconds of horn countdown first");
    assert!(
        raid.boss_event()
            .players()
            .iter()
            .any(|p| Arc::ptr_eq(p, &player))
    );
    for _ in 0..3 {
        world.raids().tick(world);
    }
    assert!(raid.is_started());
    assert_eq!(raid.groups_spawned(), 1);
    let raiders = raider_entities(&raid, world);
    // Wave 1 on normal: four pillagers plus up to one bonus pillager and vindicator.
    assert!((4..=6).contains(&raiders.len()), "{}", raiders.len());
    assert_eq!(raiders.len(), raid.total_raiders_alive());
    let mut leaders = 0;
    for entity in &raiders {
        let raider = entity.as_raider().expect("raider");
        assert!(
            raider
                .current_raid()
                .is_some_and(|r| Arc::ptr_eq(&r, &raid))
        );
        assert_eq!(raider.wave(), 1);
        assert!(raider.can_join_raid());
        assert!(raider.requires_custom_persistence());
        assert!(!raider.remove_when_far_away(1.0e9));
        if raider.is_patrol_leader() {
            leaders += 1;
            let head = raider
                .living_base()
                .equipment()
                .lock()
                .get_ref(EquipmentSlot::Head)
                .clone();
            assert!(ItemStack::matches(&head, &ominous_banner_instance()));
        }
    }
    assert_eq!(leaders, 1);
    assert!(raid.leader(1).is_some());
    let total = raid.total_health();
    assert!((raid.health_of_living_raiders(world) - total).abs() < 1.0e-3);
}

#[test]
fn killing_raiders_records_heroes_and_easy_raids_end_in_victory() {
    let fixture = village_world("raid_victory", Difficulty::Easy);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    let raid = world
        .raids()
        .create_or_extend_raid(world, &player, CENTER)
        .expect("raid");
    assert_eq!(raid.num_groups(), 3);

    for wave in 1..=3 {
        let mut ticks = 0;
        while raid.groups_spawned() < wave {
            world.raids().tick(world);
            ticks += 1;
            assert!(ticks < 400, "wave {wave} never spawned");
        }
        kill_all(&raid, world, &player);
        assert_eq!(raid.total_raiders_alive(), 0);
        assert_eq!(raid.leader(wave), None);
    }
    assert_eq!(raid.heroes_of_the_village(), vec![player.uuid()]);

    for _ in 0..41 {
        world.raids().tick(world);
    }
    assert!(raid.is_victory());
    let hero = player
        .living_base()
        .mob_effect(vanilla_mob_effects::HERO_OF_THE_VILLAGE)
        .expect("hero of the village");
    assert_eq!(hero.amplifier(), 0);

    for _ in 0..600 {
        world.raids().tick(world);
    }
    assert!(raid.is_stopped(), "celebration lasts 30 seconds");
}

#[test]
fn saved_raids_round_trip() {
    init_vanilla_registry();
    let raids = Raids::default();
    let raid = Arc::new(Raid::new(BlockPos::new(1, 2, 3), Difficulty::Hard));
    raid.set_raid_omen_level(3);
    raid.add_hero_of_the_village(Uuid::from_u128(7));
    raids.state.lock().raids.push((4, Arc::clone(&raid)));
    raids.state.lock().next_id = 4;
    let saved = raids.to_saved();
    let text = toml::to_string_pretty(&saved).expect("toml");
    let parsed: SavedRaids = toml::from_str(&text).expect("parse");
    assert_eq!(parsed, saved);
    let loaded = Raids::from_saved(&parsed);
    let loaded_raid = loaded.get(4).expect("raid 4");
    assert_eq!(loaded_raid.center(), BlockPos::new(1, 2, 3));
    assert_eq!(loaded_raid.raid_omen_level(), 3);
    assert_eq!(loaded_raid.num_groups(), 7);
    assert_eq!(
        loaded_raid.heroes_of_the_village(),
        vec![Uuid::from_u128(7)]
    );
    assert!(text.contains("status = \"ongoing\""), "{text}");
}

#[test]
fn raid_buffs_arm_vindicators() {
    let fixture = village_world("raid_buffs", Difficulty::Normal);
    let world = &fixture.world;
    let vindicator = create_entity_instance(
        world,
        &vanilla_entities::VINDICATOR,
        DVec3::new(8.5, 64.0, 8.5),
    )
    .expect("vindicator");
    let raider = vindicator.as_raider().expect("raider");
    raider.apply_raid_buffs(world, 6, 1.0);
    let axe = raider
        .living_base()
        .equipment()
        .lock()
        .get_ref(EquipmentSlot::MainHand)
        .clone();
    assert!(axe.is(&vanilla_items::IRON_AXE));
    assert_eq!(
        axe.get_enchantment_level(&vanilla_enchantments::SHARPNESS.key),
        2
    );
}

#[test]
fn raiders_save_their_raid_id_and_rejoin_on_load() {
    let fixture = village_world("raid_raider_save", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
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
    let raid_id = world.raids().get_id(&raid).expect("id");
    let entity = raider_entities(&raid, world)
        .into_iter()
        .next()
        .expect("a raider");
    let mut nbt = NbtCompound::new();
    entity.save_additional(&mut nbt);
    assert_eq!(nbt.int("RaidId"), Some(raid_id));
    assert_eq!(nbt.int("Wave"), Some(1));

    let alive = raid.total_raiders_alive();
    raid.remove_from_raid(world, entity.as_raider().expect("raider"), false);
    assert_eq!(raid.total_raiders_alive(), alive - 1);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    entity.load_additional((&borrowed).into());
    let raider = entity.as_raider().expect("raider");
    assert!(
        raider
            .current_raid()
            .is_some_and(|r| Arc::ptr_eq(&r, &raid))
    );
    assert_eq!(raid.total_raiders_alive(), alive);
}

/// Kills a fresh pillager at `pos` and returns the ominous bottles it dropped.
fn bottles_dropped_by_pillager(world: &Arc<World>, pos: DVec3, captain: bool) -> Vec<ItemStack> {
    let entity = create_entity_instance(world, &vanilla_entities::PILLAGER, pos).expect("pillager");
    assert!(
        world
            .try_add_fresh_entity_with_passengers(Arc::clone(&entity))
            .is_ok()
    );
    let raider = entity.as_raider().expect("raider");
    if captain {
        raider.set_patrol_leader(true);
        let _ = raider
            .living_base()
            .equipment()
            .lock()
            .set(EquipmentSlot::Head, ominous_banner_instance());
        assert!(raider.is_captain());
    }
    entity
        .as_living_entity()
        .expect("living")
        .die(&DamageSource::environment(&vanilla_damage_types::GENERIC));
    let search = entity.bounding_box().inflate(4.0);
    world
        .get_entities_in_aabb_matching(&search, |found| {
            found
                .downcast_ref::<ItemEntity>()
                .is_some_and(|item| item.get_item().is(&vanilla_items::OMINOUS_BOTTLE))
        })
        .into_iter()
        .filter_map(|found| found.downcast_ref::<ItemEntity>().map(ItemEntity::get_item))
        .collect()
}

#[test]
fn only_a_captain_outside_a_raid_drops_an_ominous_bottle() {
    let fixture = village_world("raid_captain_bottle", Difficulty::Normal);
    let world = &fixture.world;
    // Far from any raid: the village center has none yet.
    let captain_bottles = bottles_dropped_by_pillager(world, DVec3::new(30.5, 64.0, 30.5), true);
    assert_eq!(captain_bottles.len(), 1, "{captain_bottles:?}");
    let amplifier = captain_bottles[0]
        .get(OMINOUS_BOTTLE_AMPLIFIER)
        .expect("amplifier")
        .value();
    assert!((0..=4).contains(&amplifier), "{amplifier}");

    assert!(
        bottles_dropped_by_pillager(world, DVec3::new(-20.5, 64.0, -20.5), false).is_empty(),
        "a plain pillager drops no bottle"
    );

    let player = player_at(world, DVec3::new(8.5, 64.0, 8.5));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    let _raid = world
        .raids()
        .create_or_extend_raid(world, &player, CENTER)
        .expect("raid");
    assert!(
        bottles_dropped_by_pillager(world, DVec3::new(10.5, 64.0, 10.5), true).is_empty(),
        "a captain killed inside a raid drops no bottle"
    );
}

#[test]
fn drinking_an_ominous_bottle_gives_bad_omen() {
    let fixture = village_world("raid_drink_bottle", Difficulty::Normal);
    let world = &fixture.world;
    let player = player_at(world, DVec3::new(30.5, 64.0, 30.5));
    let mut bottle = ItemStack::new(&vanilla_items::OMINOUS_BOTTLE);
    bottle.set_ominous_bottle_amplifier(3);
    let remainder = finish_consuming_stack(&bottle, world, player.as_ref());
    assert!(remainder.is_empty());
    let omen = player
        .living_base()
        .mob_effect(vanilla_mob_effects::BAD_OMEN)
        .expect("bad omen");
    assert_eq!(omen.amplifier(), 3);
    assert_eq!(omen.duration(), 120_000);
    assert!(!omen.is_ambient());
    assert!(!omen.is_visible());
}
