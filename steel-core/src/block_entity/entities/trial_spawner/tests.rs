use std::io::Cursor;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
use steel_utils::ChunkPos;
use steel_utils::Downcast as _;

use super::*;
use crate::behavior::init_behaviors;
use crate::block_entity::{SharedBlockEntity, init_block_entities};
use crate::entity::init_entities;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, dropped_items, fresh_test_world, insert_ready_full_chunk,
};

const SPAWNER: BlockPos = BlockPos::new(8, 64, 8);

fn arena(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_block_entities();
    init_entities();
    let fixture = fresh_test_world(name);
    fixture.world.set_difficulty(Difficulty::Normal);
    insert_ready_full_chunk(&fixture.world, ChunkPos::from_block_pos(SPAWNER));
    assert!(fixture.world.set_block(
        SPAWNER,
        vanilla_blocks::TRIAL_SPAWNER.default_state(),
        UpdateFlags::UPDATE_ALL,
    ));
    fixture
}

fn block_entity(world: &World) -> SharedBlockEntity {
    world
        .get_block_entity(SPAWNER)
        .expect("a trial spawner block entity")
}

fn spawner(block_entity: &SharedBlockEntity) -> &TrialSpawnerBlockEntity {
    block_entity
        .downcast_ref::<TrialSpawnerBlockEntity>()
        .expect("a trial spawner")
}

fn state(world: &World) -> TrialSpawnerState {
    world.get_block_state(SPAWNER).get_value(STATE)
}

fn ominous(world: &World) -> bool {
    world
        .get_block_state(SPAWNER)
        .get_value(&BlockStateProperties::OMINOUS)
}

/// One game tick for the spawner.
fn tick(world: &Arc<World>) {
    block_entity(world).tick(world);
    world.level_data.write().advance_game_time();
}

/// Ticks until `done` holds, at most `limit` times.
fn tick_until(world: &Arc<World>, limit: u32, done: impl Fn(&Arc<World>) -> bool) -> bool {
    for _ in 0..limit {
        if done(world) {
            return true;
        }
        tick(world);
    }
    done(world)
}

fn player_near(world: &Arc<World>, name: &'static str, id: i32) -> Arc<Player> {
    let player = TestPlayerBuilder::new(Arc::clone(world), name, id).build();
    player.snap_to(DVec3::new(10.5, 64.0, 8.5), 0.0, 0.0);
    assert!(world.players.insert(Arc::clone(&player)));
    player
}

/// A spawner of breezes, which spawn at any light, waiting for players.
fn breeze_spawner(world: &Arc<World>) {
    let block_entity = block_entity(world);
    spawner(&block_entity).set_entity_id(&vanilla_entities::BREEZE);
    assert_eq!(state(world), TrialSpawnerState::Inactive);
    tick(world);
    assert_eq!(state(world), TrialSpawnerState::WaitingForPlayers);
}

fn kill_current_mobs(world: &World) {
    for uuid in spawner(&block_entity(world)).current_mobs() {
        if let Some(mob) = world.get_entity_by_uuid(&uuid) {
            mob.set_removed(RemovalReason::Killed);
        }
    }
}

#[test]
fn a_player_starts_a_wave_that_ends_in_a_reward_and_a_cooldown() {
    let fixture = arena("trial_spawner_wave");
    let world = &fixture.world;
    breeze_spawner(world);
    let player = player_near(world, "Challenger", 1);

    assert!(tick_until(world, 40, |world| state(world) == TrialSpawnerState::Active));
    let block_entity = block_entity(world);
    let trial = spawner(&block_entity);
    assert_eq!(trial.detected_players(), [player.uuid()]);

    // One player: two mobs at a time, six in the wave.
    assert!(tick_until(world, 200, |_| trial.current_mobs().len() == 2));
    for _ in 0..100 {
        tick(world);
    }
    assert_eq!(trial.current_mobs().len(), 2);
    assert_eq!(trial.total_mobs_spawned(), 2);

    // Each kill frees a slot; the sixth death ends the wave.
    let mut kills = 0;
    while state(world) == TrialSpawnerState::Active {
        kill_current_mobs(world);
        tick(world);
        assert!(tick_until(world, 200, |world| {
            state(world) != TrialSpawnerState::Active || !trial.current_mobs().is_empty()
        }));
        kills += 1;
        assert!(kills < 10, "the wave never ended");
    }
    assert_eq!(state(world), TrialSpawnerState::WaitingForRewardEjection);
    assert_eq!(trial.total_mobs_spawned(), 0);
    let cooldown_ends_at = trial.cooldown_ends_at();
    assert!(cooldown_ends_at > world.game_time());

    assert!(tick_until(world, 60, |world| state(world)
        == TrialSpawnerState::EjectingReward));
    assert!(tick_until(world, 100, |world| state(world)
        == TrialSpawnerState::Cooldown));
    assert_eq!(trial.detected_players().len(), 0);
    assert_eq!(trial.cooldown_ends_at(), cooldown_ends_at);
    let rewards = dropped_items(world, block_center(SPAWNER));
    assert_ne!(rewards.len(), 0);

    // The same player standing by does not restart it during the cooldown.
    for _ in 0..40 {
        tick(world);
    }
    assert_eq!(state(world), TrialSpawnerState::Cooldown);
}

#[test]
fn bad_omen_turns_into_trial_omen_and_makes_the_spawner_ominous() {
    let fixture = arena("trial_spawner_ominous");
    let world = &fixture.world;
    breeze_spawner(world);
    let player = player_near(world, "Omened", 1);
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::BAD_OMEN,
        1200,
        1
    )));

    assert!(tick_until(world, 40, |world| state(world) == TrialSpawnerState::Active));
    assert!(ominous(world));
    assert!(!player.has_mob_effect(vanilla_mob_effects::BAD_OMEN));
    let trial_omen = player
        .mob_effect(vanilla_mob_effects::TRIAL_OMEN)
        .expect("trial omen");
    assert_eq!(trial_omen.duration(), 2 * TRIAL_OMEN_PER_BAD_OMEN_LEVEL);
    assert_eq!(trial_omen.amplifier(), 0);
}

#[test]
fn creative_players_and_peaceful_worlds_do_not_start_a_wave() {
    let fixture = arena("trial_spawner_no_wave");
    let world = &fixture.world;
    breeze_spawner(world);
    let player = player_near(world, "Builder", 1);
    player.restore_game_modes(GameType::Creative, None);
    for _ in 0..40 {
        tick(world);
    }
    assert_eq!(state(world), TrialSpawnerState::WaitingForPlayers);

    player.restore_game_modes(GameType::Survival, None);
    world.set_difficulty(Difficulty::Peaceful);
    for _ in 0..40 {
        tick(world);
    }
    assert_eq!(state(world), TrialSpawnerState::WaitingForPlayers);
    assert_eq!(spawner(&block_entity(world)).detected_players().len(), 0);
}

#[test]
fn an_empty_spawner_stays_inactive() {
    let fixture = arena("trial_spawner_empty");
    let world = &fixture.world;
    for _ in 0..5 {
        tick(world);
    }
    assert_eq!(state(world), TrialSpawnerState::Inactive);
}

fn reload(saved: &NbtCompound) -> TrialSpawnerBlockEntity {
    let mut bytes = Vec::new();
    saved.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(bytes.as_slice())).expect("valid nbt");
    let reloaded = TrialSpawnerBlockEntity::new(
        Weak::new(),
        SPAWNER,
        vanilla_blocks::TRIAL_SPAWNER.default_state(),
    );
    reloaded.load_additional(&borrowed);
    reloaded
}

#[test]
fn a_fresh_spawner_saves_nothing() {
    init_vanilla_registry();
    let fresh = TrialSpawnerBlockEntity::new(
        Weak::new(),
        SPAWNER,
        vanilla_blocks::TRIAL_SPAWNER.default_state(),
    );
    let mut nbt = NbtCompound::new();
    fresh.save_additional(&mut nbt);
    assert_eq!(nbt.len(), 0);
}

#[test]
fn the_wave_and_a_registry_config_survive_a_reload() {
    init_vanilla_registry();
    let key = Identifier::vanilla_static("trial_chamber/breeze/normal");
    let config = FullConfig {
        normal: TrialSpawnerConfigHolder::reference(&key).expect("breeze config"),
        target_cooldown_length: 100,
        ..FullConfig::default()
    };
    let mut saved = NbtCompound::new();
    {
        let mut spawner = TrialSpawner::new();
        spawner.config = config.clone();
        spawner.data.detected_players = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        spawner.data.total_mobs_spawned = 3;
        spawner.data.cooldown_ends_at = 77;
        spawner.data.ejecting_loot_table = Some(Identifier::vanilla_static("a/b"));
        spawner.data.save(&mut saved);
        spawner.config.save(&mut saved);
    }
    assert_eq!(
        saved
            .string("normal_config")
            .map(|key| key.to_str().into_owned()),
        Some("minecraft:trial_chamber/breeze/normal".to_owned())
    );

    let reloaded = reload(&saved);
    assert_eq!(reloaded.config(), config);
    assert_eq!(
        reloaded.detected_players(),
        [Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    assert_eq!(reloaded.total_mobs_spawned(), 3);
    assert_eq!(reloaded.cooldown_ends_at(), 77);
    let mut resaved = NbtCompound::new();
    reloaded.save_additional(&mut resaved);
    assert_eq!(resaved, saved);
}

#[test]
fn an_out_of_range_player_range_falls_back_to_the_default_config() {
    init_vanilla_registry();
    let mut saved = NbtCompound::new();
    saved.insert("required_player_range", 200);
    saved.insert("target_cooldown_length", 5);
    assert_eq!(reload(&saved).config(), FullConfig::default());
}

#[test]
fn the_update_tag_sends_the_spawn_time_only_while_active() {
    let mut data = StateData::default();
    data.next_mob_spawns_at = 50;
    assert_eq!(
        data.update_tag(TrialSpawnerState::Active)
            .long("next_mob_spawns_at"),
        Some(50)
    );
    assert_eq!(data.update_tag(TrialSpawnerState::Cooldown).len(), 0);
    data.next_spawn_data = Some(super::super::SpawnData::default());
    assert!(
        data.update_tag(TrialSpawnerState::Cooldown)
            .compound("spawn_data")
            .is_some()
    );
}
