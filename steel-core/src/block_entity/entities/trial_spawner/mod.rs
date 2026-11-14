//! Vanilla `TrialSpawnerBlockEntity`, `TrialSpawner`, `TrialSpawnerStateData`
//! and `TrialSpawnerState.tickAndGetNext`: the trial chamber spawner that
//! fights each nearby player with a wave of mobs and rewards them after.

mod config;
mod state_data;
#[cfg(test)]
mod tests;

use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::{Rng, RngExt as _, rng};
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, Direction, EnumProperty, TrialSpawnerState,
};
use steel_registry::data_components::components::EntityData;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::loot_table::LootContext;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{
    REGISTRY, RegistryExt as _, level_events, sound_events, vanilla_block_entity_types,
    vanilla_game_events, vanilla_game_rules, vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::{Difficulty, GameType, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, Identifier, WorldAabb};
use uuid::Uuid;

use super::spawner::{
    Spawner, configured_or_random_position, entity_type_from_owned_nbt, equip_from_table,
};
use crate::behavior::dispense::spawn_item;
use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::block_entity::player_detector::{
    EntitySelector, PlayerDetector, block_center, in_line_of_sight,
};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::entities::OminousItemSpawnerEntity;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity as _, MobEffectInstance, RemovalReason, SharedEntity,
    SpawnPlacements, load_entity_recursive_owned,
};
use crate::physics::{WorldCollisionProvider, has_collision};
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{ClipBlockShape, ClipFluid, World};

pub use config::{
    DEFAULT_PLAYER_SCAN_RANGE, DEFAULT_TARGET_COOLDOWN_LENGTH, FullConfig, TrialSpawnerConfig,
    TrialSpawnerConfigHolder, WeightedLootTable,
};
use state_data::StateData;

/// Vanilla `TrialSpawner.DETECT_PLAYER_SPAWN_BUFFER`.
const DETECT_PLAYER_SPAWN_BUFFER: i64 = 40;
/// Vanilla `TrialSpawner.MAX_MOB_TRACKING_DISTANCE`.
const MAX_MOB_TRACKING_DISTANCE: i32 = 47;
/// Vanilla `TrialSpawnerStateData.DELAY_BETWEEN_PLAYER_SCANS`.
const DELAY_BETWEEN_PLAYER_SCANS: i64 = 20;
/// Vanilla `TrialSpawnerStateData.TRIAL_OMEN_PER_BAD_OMEN_LEVEL`.
const TRIAL_OMEN_PER_BAD_OMEN_LEVEL: i32 = 18_000;
/// Vanilla `TrialSpawnerState.DELAY_BEFORE_EJECT_AFTER_KILLING_LAST_MOB`.
const DELAY_BEFORE_EJECT_AFTER_KILLING_LAST_MOB: f32 = 40.0;
/// Vanilla `TrialSpawnerState.TIME_BETWEEN_EACH_EJECTION`.
const TIME_BETWEEN_EACH_EJECTION: f32 = 30.0;

const STATE: &EnumProperty<TrialSpawnerState> = &BlockStateProperties::TRIAL_SPAWNER_STATE;

/// Vanilla `TrialSpawner.FlameParticle`, sent as level event data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlameParticle {
    Normal = 0,
    Ominous = 1,
}

impl FlameParticle {
    const fn of(ominous: bool) -> Self {
        if ominous { Self::Ominous } else { Self::Normal }
    }
}

/// Vanilla `TrialSpawner`: the config, the state data and the player
/// detector of one trial spawner.
struct TrialSpawner {
    data: StateData,
    config: FullConfig,
    player_detector: PlayerDetector,
    is_ominous: bool,
    /// Whether vanilla `markUpdated` ran this tick and the client needs the
    /// update tag again.
    updated: bool,
}

impl TrialSpawner {
    fn new() -> Self {
        Self {
            data: StateData::default(),
            config: FullConfig::default(),
            player_detector: PlayerDetector::NoCreativePlayers,
            is_ominous: false,
            updated: false,
        }
    }

    /// Vanilla `activeConfig`.
    fn active_config(&self) -> &TrialSpawnerConfig {
        if self.is_ominous {
            self.config.ominous.value()
        } else {
            self.config.normal.value()
        }
    }

    /// Vanilla `markUpdated`: the block entity is saved and re-sent.
    const fn mark_updated(&mut self) {
        self.updated = true;
    }

    /// Vanilla `canSpawnInLevel`.
    fn can_spawn_in_level(world: &World) -> bool {
        world.get_game_rule(&vanilla_game_rules::SPAWNER_BLOCKS_WORK)
            && world.difficulty() != Difficulty::Peaceful
            && world.get_game_rule(&vanilla_game_rules::SPAWN_MOBS)
    }

    /// Vanilla `TrialSpawnerStateData.getOrCreateNextSpawnData`.
    fn get_or_create_next_spawn_data(&mut self, random: &mut impl Rng) -> super::SpawnData {
        if let Some(data) = &self.data.next_spawn_data {
            return data.clone();
        }
        let potentials = &self.active_config().spawn_potentials;
        let selected = super::spawner::choose_weighted(potentials, random)
            .map_or_else(super::SpawnData::default, |index| {
                potentials[index].data.clone()
            });
        self.data.next_spawn_data = Some(selected.clone());
        self.mark_updated();
        selected
    }

    /// Vanilla `TrialSpawnerStateData.hasMobToSpawn`.
    fn has_mob_to_spawn(&mut self, random: &mut impl Rng) -> bool {
        let next = self.get_or_create_next_spawn_data(random);
        next.entity_to_spawn.string("id").is_some()
            || !self.active_config().spawn_potentials.is_empty()
    }

    /// Vanilla `TrialSpawnerStateData.getOrCreateDisplayEntity` on the
    /// server: the display entity only gates leaving `Inactive`.
    fn has_display_entity(&mut self, world: &Arc<World>, random: &mut impl Rng) -> bool {
        if !self.data.has_display_entity {
            let entity = self.get_or_create_next_spawn_data(random).entity_to_spawn;
            self.data.has_display_entity = entity.string("id").is_some()
                && load_entity_recursive_owned(
                    world,
                    &entity,
                    EntitySpawnReason::TrialSpawner,
                    |_| {},
                )
                .is_some();
        }
        self.data.has_display_entity
    }

    /// Vanilla `applyOminous`.
    fn apply_ominous(&mut self, world: &Arc<World>, pos: BlockPos) {
        let state = world
            .get_block_state(pos)
            .set_value(&BlockStateProperties::OMINOUS, true);
        world.set_block(pos, state, UpdateFlags::UPDATE_ALL);
        world.level_event(
            level_events::PARTICLES_TRIAL_SPAWNER_BECOME_OMINOUS,
            pos,
            1,
            None,
        );
        self.is_ominous = true;
        self.reset_after_becoming_ominous(world);
    }

    /// Vanilla `removeOminous`.
    fn remove_ominous(&mut self, world: &Arc<World>, pos: BlockPos) {
        let state = world
            .get_block_state(pos)
            .set_value(&BlockStateProperties::OMINOUS, false);
        world.set_block(pos, state, UpdateFlags::UPDATE_ALL);
        self.is_ominous = false;
    }

    /// Vanilla `TrialSpawnerStateData.resetAfterBecomingOminous`: the normal
    /// wave vanishes and the ominous one starts over.
    fn reset_after_becoming_ominous(&mut self, world: &World) {
        for uuid in &self.data.current_mobs {
            let Some(entity) = world.get_entity_by_uuid(uuid) else {
                continue;
            };
            world.level_event(
                level_events::PARTICLES_TRIAL_SPAWNER_SPAWN_MOB_AT,
                entity.block_position(),
                FlameParticle::Normal as i32,
                None,
            );
            if let Some(mob) = entity.as_mob() {
                mob.drop_preserved_equipment(&|_| true);
            }
            entity.set_removed(RemovalReason::Discarded);
        }
        let ominous = self.config.ominous.clone();
        let ominous = ominous.value();
        if !ominous.spawn_potentials.is_empty() {
            self.data.next_spawn_data = None;
        }
        let game_time = world.game_time();
        self.data.total_mobs_spawned = 0;
        self.data.current_mobs.clear();
        self.data.next_mob_spawns_at = game_time + i64::from(ominous.ticks_between_spawn);
        self.mark_updated();
        self.data.cooldown_ends_at = game_time + ominous.ticks_between_item_spawners();
    }

    /// Vanilla `TrialSpawnerStateData.tryDetectPlayers`.
    fn try_detect_players(&mut self, world: &Arc<World>, pos: BlockPos, state: TrialSpawnerState) {
        let game_time = world.game_time();
        let scan_offset = steel_utils::PackedBlockPos::from(pos)
            .as_raw()
            .wrapping_add(game_time);
        if scan_offset % DELAY_BETWEEN_PLAYER_SCANS != 0 {
            return;
        }
        if state == TrialSpawnerState::Cooldown && self.is_ominous {
            return;
        }
        let range = f64::from(self.config.required_player_range);
        let in_line_of_sight_players =
            self.player_detector
                .detect(world, EntitySelector::SelectFromLevel, pos, range, true);
        let mut became_ominous = false;
        if !self.is_ominous
            && !in_line_of_sight_players.is_empty()
            && let Some((player, effect)) =
                find_player_with_ominous_effect(world, &in_line_of_sight_players)
        {
            if effect == OminousEffect::BadOmen {
                transform_bad_omen_into_trial_omen(&player);
            }
            let eye = DVec3::new(player.position().x, player.get_eye_y(), player.position().z);
            world.level_event(
                level_events::PARTICLES_TRIAL_SPAWNER_BECOME_OMINOUS,
                BlockPos::containing(eye.x, eye.y, eye.z),
                0,
                None,
            );
            self.apply_ominous(world, pos);
            became_ominous = true;
        }

        if state != TrialSpawnerState::Cooldown || became_ominous {
            let found = if self.data.detected_players.is_empty() {
                in_line_of_sight_players
            } else {
                self.player_detector.detect(
                    world,
                    EntitySelector::SelectFromLevel,
                    pos,
                    range,
                    false,
                )
            };
            let mut added = false;
            for uuid in found {
                if !self.data.detected_players.contains(&uuid) {
                    self.data.detected_players.push(uuid);
                    added = true;
                }
            }
            if added {
                self.data.next_mob_spawns_at = self
                    .data
                    .next_mob_spawns_at
                    .max(game_time + DETECT_PLAYER_SPAWN_BUFFER);
                if !became_ominous {
                    let event = if self.is_ominous {
                        level_events::PARTICLES_TRIAL_SPAWNER_DETECT_PLAYER_OMINOUS
                    } else {
                        level_events::PARTICLES_TRIAL_SPAWNER_DETECT_PLAYER
                    };
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_possible_wrap,
                        reason = "vanilla sends the player set's int size"
                    )]
                    let count = self.data.detected_players.len() as i32;
                    world.level_event(event, pos, count, None);
                }
            }
        }
    }

    /// Vanilla `spawnMob`: one mob from the next spawn data, or `None` when
    /// the spot or the data does not allow it.
    fn spawn_mob(&mut self, world: &Arc<World>, pos: BlockPos) -> Option<Uuid> {
        let mut random = rng();
        let next = self.get_or_create_next_spawn_data(&mut random);
        let entity_type = entity_type_from_owned_nbt(&next.entity_to_spawn)?;
        let spawn_position = configured_or_random_position(
            &next.entity_to_spawn,
            pos,
            self.active_config().spawn_range,
            &mut random,
        );
        let dimensions = entity_type.dimensions;
        let spawn_box = WorldAabb::entity_box(
            spawn_position.x,
            spawn_position.y,
            spawn_position.z,
            f64::from(dimensions.half_width()),
            f64::from(dimensions.height),
        );
        if has_collision(&WorldCollisionProvider::new(world), spawn_box)
            || !in_line_of_sight(world, block_center(pos), spawn_position)
        {
            return None;
        }
        let spawn_block_pos =
            BlockPos::containing(spawn_position.x, spawn_position.y, spawn_position.z);
        if !SpawnPlacements::check_spawn_rules(
            entity_type,
            world.as_ref(),
            EntitySpawnReason::TrialSpawner,
            spawn_block_pos,
            &mut random,
        ) {
            return None;
        }
        if let Some(rules) = &next.custom_spawn_rules
            && !rules.is_valid_position(world, spawn_block_pos)
        {
            return None;
        }

        let yaw = random.random::<f32>() * 360.0;
        let loaded = load_entity_recursive_owned(
            world,
            &next.entity_to_spawn,
            EntitySpawnReason::TrialSpawner,
            |entity| entity.snap_to(spawn_position, yaw, 0.0),
        )?;
        let entity = &loaded.root;
        if let Some(mob) = entity.as_mob() {
            if !mob.check_spawn_obstruction(world) {
                return None;
            }
            let has_no_configuration =
                next.entity_to_spawn.len() == 1 && next.entity_to_spawn.string("id").is_some();
            if has_no_configuration {
                let _ = mob.finalize_spawn(world, EntitySpawnReason::TrialSpawner, None);
            }
            mob.set_persistence_required();
            if let Some(equipment) = &next.equipment {
                equip_from_table(mob, equipment);
            }
        }
        if world
            .try_add_fresh_entity_with_passengers(Arc::clone(entity))
            .is_err()
        {
            return None;
        }

        let flame = FlameParticle::of(self.is_ominous) as i32;
        world.level_event(
            level_events::PARTICLES_TRIAL_SPAWNER_SPAWN,
            pos,
            flame,
            None,
        );
        world.level_event(
            level_events::PARTICLES_TRIAL_SPAWNER_SPAWN_MOB_AT,
            spawn_block_pos,
            flame,
            None,
        );
        world.game_event(
            &vanilla_game_events::ENTITY_PLACE,
            spawn_block_pos,
            &GameEventContext::new(Some(entity.as_ref()), None),
        );
        Some(entity.uuid())
    }

    /// Vanilla `TrialSpawnerState.tickAndGetNext`.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per state, in the order of vanilla tickAndGetNext"
    )]
    fn tick_and_get_next(
        &mut self,
        world: &Arc<World>,
        pos: BlockPos,
        state: TrialSpawnerState,
    ) -> TrialSpawnerState {
        // Vanilla reads the active config once, before a player can turn the
        // spawner ominous during this tick.
        let config = if self.is_ominous {
            self.config.ominous.clone()
        } else {
            self.config.normal.clone()
        };
        let config = config.value();
        let mut random = rng();
        let game_time = world.game_time();
        let cooldown_length = self.config.target_cooldown_length;

        match state {
            TrialSpawnerState::Inactive => {
                if self.has_display_entity(world, &mut random) {
                    TrialSpawnerState::WaitingForPlayers
                } else {
                    state
                }
            }
            TrialSpawnerState::WaitingForPlayers => {
                if !Self::can_spawn_in_level(world) {
                    self.data.reset_statistics();
                    state
                } else if !self.has_mob_to_spawn(&mut random) {
                    TrialSpawnerState::Inactive
                } else {
                    self.try_detect_players(world, pos, state);
                    if self.data.detected_players.is_empty() {
                        state
                    } else {
                        TrialSpawnerState::Active
                    }
                }
            }
            TrialSpawnerState::Active => {
                if !Self::can_spawn_in_level(world) {
                    self.data.reset_statistics();
                    return TrialSpawnerState::WaitingForPlayers;
                }
                if !self.has_mob_to_spawn(&mut random) {
                    return TrialSpawnerState::Inactive;
                }
                let additional_players = self.data.detected_players.len().saturating_sub(1);
                self.try_detect_players(world, pos, state);
                if self.is_ominous {
                    self.spawn_ominous_item_spawner(world, pos);
                }

                if self.data.total_mobs_spawned
                    >= config.calculate_target_total_mobs(additional_players)
                {
                    if self.data.current_mobs.is_empty() {
                        self.data.cooldown_ends_at = game_time + i64::from(cooldown_length);
                        self.data.total_mobs_spawned = 0;
                        self.data.next_mob_spawns_at = 0;
                        return TrialSpawnerState::WaitingForRewardEjection;
                    }
                } else if game_time >= self.data.next_mob_spawns_at
                    && self.data.current_mobs.len()
                        < usize::try_from(
                            config.calculate_target_simultaneous_mobs(additional_players),
                        )
                        .unwrap_or(0)
                    && let Some(uuid) = self.spawn_mob(world, pos)
                {
                    self.data.current_mobs.push(uuid);
                    self.data.total_mobs_spawned += 1;
                    self.data.next_mob_spawns_at =
                        game_time + i64::from(config.ticks_between_spawn);
                    if let Some(index) =
                        super::spawner::choose_weighted(&config.spawn_potentials, &mut random)
                    {
                        self.data.next_spawn_data =
                            Some(config.spawn_potentials[index].data.clone());
                        self.mark_updated();
                    }
                }
                state
            }
            TrialSpawnerState::WaitingForRewardEjection => {
                if self.data.is_ready_to_open_shutter(
                    game_time,
                    DELAY_BEFORE_EJECT_AFTER_KILLING_LAST_MOB,
                    cooldown_length,
                ) {
                    play_sound(world, pos, &sound_events::BLOCK_TRIAL_SPAWNER_OPEN_SHUTTER);
                    TrialSpawnerState::EjectingReward
                } else {
                    state
                }
            }
            TrialSpawnerState::EjectingReward => {
                if !self.data.is_ready_to_eject_items(
                    game_time,
                    TIME_BETWEEN_EACH_EJECTION,
                    cooldown_length,
                ) {
                    state
                } else if self.data.detected_players.is_empty() {
                    play_sound(world, pos, &sound_events::BLOCK_TRIAL_SPAWNER_CLOSE_SHUTTER);
                    self.data.ejecting_loot_table = None;
                    TrialSpawnerState::Cooldown
                } else {
                    if self.data.ejecting_loot_table.is_none() {
                        self.data.ejecting_loot_table =
                            choose_loot_table(&config.loot_tables_to_eject, &mut random);
                    }
                    if let Some(table) = &self.data.ejecting_loot_table {
                        eject_reward(world, pos, table);
                    }
                    self.data.detected_players.remove(0);
                    state
                }
            }
            TrialSpawnerState::Cooldown => {
                self.try_detect_players(world, pos, state);
                if !self.data.detected_players.is_empty() {
                    self.data.total_mobs_spawned = 0;
                    self.data.next_mob_spawns_at = 0;
                    TrialSpawnerState::Active
                } else if game_time >= self.data.cooldown_ends_at {
                    self.remove_ominous(world, pos);
                    self.data.reset();
                    TrialSpawnerState::WaitingForPlayers
                } else {
                    state
                }
            }
        }
    }

    /// Vanilla `TrialSpawnerState.spawnOminousOminousItemSpawner`: drops an
    /// item spawner above a player or a mob of the wave.
    fn spawn_ominous_item_spawner(&mut self, world: &Arc<World>, pos: BlockPos) {
        let config = if self.is_ominous {
            self.config.ominous.clone()
        } else {
            self.config.normal.clone()
        };
        let item = self.data.dispensing_item(world, config.value(), pos);
        let Some(item) = item else {
            return;
        };
        if world.game_time() < self.data.cooldown_ends_at {
            return;
        }
        let Some(position) = self.position_to_spawn_item_spawner(world, pos) else {
            return;
        };
        let spawner = OminousItemSpawnerEntity::create(world, position, item);
        let spawner: SharedEntity = Arc::new(spawner);
        let _ = world.try_add_entity(spawner);
        let mut random = rng();
        let pitch = (random.random::<f32>() - random.random::<f32>()) * 0.2 + 1.0;
        world.play_sound(
            &sound_events::BLOCK_TRIAL_SPAWNER_SPAWN_ITEM_BEGIN,
            SoundSource::Blocks,
            BlockPos::containing(position.x, position.y, position.z),
            1.0,
            pitch,
            None,
        );
        self.data.cooldown_ends_at =
            world.game_time() + self.config.ominous.value().ticks_between_item_spawners();
    }

    /// Vanilla `TrialSpawnerState.calculatePositionToSpawnSpawner`.
    fn position_to_spawn_item_spawner(&self, world: &World, pos: BlockPos) -> Option<DVec3> {
        let center = block_center(pos);
        let range = f64::from(self.config.required_player_range);
        let range_sqr = range * range;
        let nearby_players = self
            .data
            .detected_players
            .iter()
            .filter_map(|uuid| world.players.get_by_uuid(uuid))
            .filter(|player| {
                player.game_mode() != GameType::Creative
                    && !player.is_spectator()
                    && Entity::is_alive(player.as_ref())
                    && player.position().distance_squared(center) <= range_sqr
            })
            .map(|player| player as SharedEntity)
            .collect::<Vec<_>>();
        if nearby_players.is_empty() {
            return None;
        }
        let mut random = rng();
        let eligible = if random.random::<bool>() {
            self.data
                .current_mobs
                .iter()
                .filter_map(|uuid| world.get_entity_by_uuid(uuid))
                .filter(|mob| {
                    mob.is_alive() && mob.position().distance_squared(center) <= range_sqr
                })
                .collect::<Vec<_>>()
        } else {
            nearby_players
        };
        if eligible.is_empty() {
            return None;
        }
        let entity = &eligible[random.random_range(0..eligible.len())];
        position_above(world, entity.as_ref(), &mut random)
    }
}

/// Vanilla `TrialSpawnerState.calculatePositionAbove`: the first open block a
/// few blocks above the entity's head.
fn position_above(world: &World, entity: &dyn Entity, random: &mut impl Rng) -> Option<DVec3> {
    let entity_pos = entity.position();
    let height = entity.bounding_box().height();
    let try_spawn_pos = entity_pos
        + DVec3::new(
            0.0,
            height + 2.0 + f64::from(random.random_range(0..4_i32)),
            0.0,
        );
    let hit = world.clip(
        entity_pos,
        try_spawn_pos,
        ClipBlockShape::Visual,
        ClipFluid::None,
    );
    let down = block_center(hit.block_pos) - DVec3::Y;
    let block_pos_down = BlockPos::containing(down.x, down.y, down.z);
    let state = world.get_block_state(block_pos_down);
    let shape = BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, block_pos_down, BlockCollisionContext::empty());
    shape.is_empty().then_some(down)
}

/// Which omen the first matching player in line of sight carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OminousEffect {
    TrialOmen,
    BadOmen,
}

/// Vanilla `TrialSpawnerStateData.findPlayerWithOminousEffect`: the first
/// player with Trial Omen, otherwise the last one with Bad Omen.
fn find_player_with_ominous_effect(
    world: &World,
    players: &[Uuid],
) -> Option<(Arc<Player>, OminousEffect)> {
    let mut with_bad_omen = None;
    for uuid in players {
        let Some(player) = world.players.get_by_uuid(uuid) else {
            continue;
        };
        if player.has_mob_effect(vanilla_mob_effects::TRIAL_OMEN) {
            return Some((player, OminousEffect::TrialOmen));
        }
        if player.has_mob_effect(vanilla_mob_effects::BAD_OMEN) {
            with_bad_omen = Some(player);
        }
    }
    with_bad_omen.map(|player| (player, OminousEffect::BadOmen))
}

/// Vanilla `TrialSpawnerStateData.transformBadOmenIntoTrialOmen`: 15 minutes
/// of Trial Omen per level of Bad Omen.
fn transform_bad_omen_into_trial_omen(player: &Player) {
    let Some(bad_omen) = player.mob_effect(vanilla_mob_effects::BAD_OMEN) else {
        return;
    };
    let amplifier = bad_omen.amplifier() + 1;
    player.remove_mob_effect(vanilla_mob_effects::BAD_OMEN);
    player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::TRIAL_OMEN,
        TRIAL_OMEN_PER_BAD_OMEN_LEVEL * amplifier,
        0,
    ));
}

/// Vanilla `WeightedList.getRandom` over `loot_tables_to_eject`.
fn choose_loot_table(tables: &[WeightedLootTable], random: &mut impl Rng) -> Option<Identifier> {
    let total = tables
        .iter()
        .map(|table| i64::from(table.weight))
        .sum::<i64>();
    if total <= 0 {
        return None;
    }
    let mut selection = random.random_range(0..total);
    for table in tables {
        let weight = i64::from(table.weight);
        if selection < weight {
            return Some(table.table.clone());
        }
        selection -= weight;
    }
    None
}

/// Vanilla `TrialSpawner.ejectReward`: one roll of the table, shot up out of
/// the spawner.
fn eject_reward(world: &Arc<World>, pos: BlockPos, table: &Identifier) {
    let Some(table) = REGISTRY.loot_tables.by_key(table) else {
        return;
    };
    let mut random = rng();
    let items = table.get_random_items(&mut LootContext::new(&mut random));
    if items.is_empty() {
        return;
    }
    let (x, y, z) = pos.get_bottom_center();
    let position = DVec3::new(x, y + 1.2, z);
    for item in items {
        spawn_item(world, item, 2.0, Direction::Up, position);
    }
    world.level_event(
        level_events::ANIMATION_TRIAL_SPAWNER_EJECT_ITEM,
        pos,
        0,
        None,
    );
}

fn play_sound(world: &World, pos: BlockPos, sound: SoundEventRef) {
    world.play_sound(sound, SoundSource::Blocks, pos, 1.0, 1.0, None);
}

/// Vanilla `TrialSpawner.shouldMobBeUntracked`.
fn should_mob_be_untracked(world: &World, pos: BlockPos, uuid: &Uuid) -> bool {
    world.get_entity_by_uuid(uuid).is_none_or(|entity| {
        let mob_pos = entity.block_position();
        let dx = f64::from(mob_pos.x() - pos.x());
        let dy = f64::from(mob_pos.y() - pos.y());
        let dz = f64::from(mob_pos.z() - pos.z());
        !entity.is_alive()
            || dx * dx + dy * dy + dz * dz
                > f64::from(MAX_MOB_TRACKING_DISTANCE * MAX_MOB_TRACKING_DISTANCE)
    })
}

/// Vanilla `TrialSpawnerBlockEntity`.
pub struct TrialSpawnerBlockEntity {
    base: BlockEntityBase,
    spawner: SyncMutex<TrialSpawner>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TrialSpawnerBlockEntity`.
unsafe impl DowncastType for TrialSpawnerBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/trial_spawner");
}

impl TrialSpawnerBlockEntity {
    /// Creates a trial spawner with the default config.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::TRIAL_SPAWNER,
                level,
                pos,
                state,
            ),
            spawner: SyncMutex::new(TrialSpawner::new()),
        }
    }

    /// The spawner's normal, ominous, cooldown and range config.
    #[must_use]
    pub fn config(&self) -> FullConfig {
        self.spawner.lock().config.clone()
    }

    /// Replaces the spawner's config.
    pub fn set_config(&self, config: FullConfig) {
        self.spawner.lock().config = config;
        self.set_changed();
    }

    /// The players taking part in the current wave.
    #[must_use]
    pub fn detected_players(&self) -> Vec<Uuid> {
        self.spawner.lock().data.detected_players.clone()
    }

    /// The living mobs of the current wave.
    #[must_use]
    pub fn current_mobs(&self) -> Vec<Uuid> {
        self.spawner.lock().data.current_mobs.clone()
    }

    /// Mobs spawned so far in the current wave.
    #[must_use]
    pub fn total_mobs_spawned(&self) -> i32 {
        self.spawner.lock().data.total_mobs_spawned
    }

    /// The tick the cooldown, or the next ominous item spawner, is due at.
    #[must_use]
    pub fn cooldown_ends_at(&self) -> i64 {
        self.spawner.lock().data.cooldown_ends_at
    }

    /// Vanilla `TrialSpawnerBlockEntity.setState`: saves and sets the block.
    fn set_state(&self, world: &Arc<World>, state: TrialSpawnerState) {
        self.set_changed();
        let pos = self.base.pos();
        let block_state = world.get_block_state(pos).set_value(STATE, state);
        world.set_block(pos, block_state, UpdateFlags::UPDATE_ALL);
    }

    /// Vanilla `overrideEntityToSpawn`: a new mob means a fresh spawner.
    fn override_entity(&self, config: impl FnOnce(&FullConfig) -> FullConfig) {
        {
            let mut spawner = self.spawner.lock();
            spawner.data.reset();
            spawner.config = config(&spawner.config);
        }
        if let Some(world) = self.get_level() {
            self.set_state(&world, TrialSpawnerState::Inactive);
            // The spawn egg's `sendBlockUpdated`: the client shows the new mob.
            world.send_block_updated(self.base.pos());
        }
        self.set_changed();
    }
}

impl Spawner for TrialSpawnerBlockEntity {
    fn set_entity_id(&self, entity_type: EntityTypeRef) {
        self.override_entity(|config| config.override_entity(entity_type));
    }

    fn set_entity_data(&self, entity_data: &EntityData) {
        self.override_entity(|config| config.override_entity_data(entity_data));
    }
}

impl BlockEntity for TrialSpawnerBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let mut spawner = self.spawner.lock();
        spawner.data.load(&nbt);
        spawner.config = FullConfig::from_nbt(&nbt).unwrap_or_default();
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let spawner = self.spawner.lock();
        spawner.data.save(nbt);
        spawner.config.save(nbt);
    }

    /// Vanilla `getUpdateTag`: the next spawn time while active, and the mob
    /// to show.
    fn get_update_tag(&self) -> Option<NbtCompound> {
        let state = self.get_block_state().get_value(STATE);
        Some(self.spawner.lock().data.update_tag(state))
    }

    fn as_spawner(&self) -> Option<&dyn Spawner> {
        Some(self)
    }

    /// Vanilla `TrialSpawner.tickServer`.
    fn tick(&self, world: &Arc<World>) {
        let pos = self.base.pos();
        let block_state = world.get_block_state(pos);
        let current = block_state.get_value(STATE);
        let (next, updated) = {
            let mut spawner = self.spawner.lock();
            spawner.is_ominous = block_state.get_value(&BlockStateProperties::OMINOUS);
            let before = spawner.data.current_mobs.len();
            spawner
                .data
                .current_mobs
                .retain(|uuid| !should_mob_be_untracked(world, pos, uuid));
            if spawner.data.current_mobs.len() != before {
                spawner.data.next_mob_spawns_at =
                    world.game_time() + i64::from(spawner.active_config().ticks_between_spawn);
            }
            let next = spawner.tick_and_get_next(world, pos, current);
            (next, mem::take(&mut spawner.updated))
        };
        if updated {
            self.set_changed();
            world.send_block_updated(pos);
        }
        if next != current {
            self.set_state(world, next);
        }
    }
}
