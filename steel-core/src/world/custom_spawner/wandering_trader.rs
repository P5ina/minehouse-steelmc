//! Vanilla `WanderingTraderSpawner`: a wandering trader with two llamas near a
//! player, rolled once a day.

use std::sync::Arc;

use rand::{Rng, RngExt as _};
use serde::{Deserialize, Serialize};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_entities::{TRADER_LLAMA, WANDERING_TRADER};
use steel_registry::vanilla_game_rules::SPAWN_WANDERING_TRADERS;
use steel_registry::{RegistryEntry as _, vanilla_poi_types};
use steel_utils::saved_data::{SavedDataManager, names as saved_data_names};
use steel_utils::{BlockPos, Downcast as _, Identifier};

use super::CustomSpawner;
use crate::entity::entities::WanderingTraderEntity;
use crate::entity::{
    Entity as _, EntitySpawnPlacement, EntitySpawnReason, EntitySpawnRequest, SharedEntity,
    SpawnPlacements, spawn_entity,
};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `DEFAULT_TICK_DELAY`: the spawn delay counts down once a minute.
const TICK_DELAY: i32 = 1_200;
/// Vanilla `DEFAULT_SPAWN_DELAY`: one roll a day.
pub(crate) const DEFAULT_SPAWN_DELAY: i32 = 24_000;
/// Vanilla `MIN_SPAWN_CHANCE`, `MAX_SPAWN_CHANCE` and `SPAWN_CHANCE_INCREASE`,
/// in percent.
pub(crate) const MIN_SPAWN_CHANCE: i32 = 25;
const MAX_SPAWN_CHANCE: i32 = 75;
const SPAWN_CHANCE_INCREASE: i32 = 25;
/// Vanilla `SPAWN_ONE_IN_X_CHANCE`.
const SPAWN_ONE_IN_X_CHANCE: i32 = 10;
/// Vanilla `NUMBER_OF_SPAWN_ATTEMPTS`.
const NUMBER_OF_SPAWN_ATTEMPTS: usize = 10;
/// The trader lands within this many blocks of the meeting point or player.
const TRADER_RADIUS: i32 = 48;
/// Llamas land within this many blocks of the trader.
const LLAMA_RADIUS: i32 = 4;
const LLAMAS_PER_TRADER: usize = 2;
/// Vanilla `WanderingTrader.setDespawnDelay(48000)`: two days.
const TRADER_DESPAWN_DELAY: i32 = 48_000;
/// Vanilla `setHomeTo(referencePos, 16)`.
const TRADER_HOME_RADIUS: i32 = 16;

/// Vanilla `WanderingTraderData`, saved as `data/wandering_trader.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WanderingTraderData {
    #[serde(default = "default_spawn_delay")]
    spawn_delay: i32,
    #[serde(default = "default_spawn_chance")]
    spawn_chance: i32,
    /// Vanilla `SavedData.dirty`: only changed data is written back.
    #[serde(skip)]
    dirty: bool,
}

const fn default_spawn_delay() -> i32 {
    DEFAULT_SPAWN_DELAY
}

const fn default_spawn_chance() -> i32 {
    MIN_SPAWN_CHANCE
}

impl Default for WanderingTraderData {
    fn default() -> Self {
        Self {
            spawn_delay: DEFAULT_SPAWN_DELAY,
            spawn_chance: MIN_SPAWN_CHANCE,
            dirty: false,
        }
    }
}

impl WanderingTraderData {
    /// Loads the saved data, starting fresh if it cannot be read.
    pub(crate) async fn load(saved_data: &SavedDataManager, world: &Identifier) -> Self {
        saved_data
            .load_or_default::<Self>(saved_data_names::WANDERING_TRADER)
            .await
            .unwrap_or_else(|error| {
                log::warn!(
                    "Could not load wandering trader data for world {world}; starting fresh: {error}"
                );
                Self::default()
            })
    }

    /// Vanilla `spawnDelay`.
    #[must_use]
    pub(crate) const fn spawn_delay(&self) -> i32 {
        self.spawn_delay
    }

    /// Vanilla `setSpawnDelay`.
    pub(crate) const fn set_spawn_delay(&mut self, spawn_delay: i32) {
        if self.spawn_delay != spawn_delay {
            self.spawn_delay = spawn_delay;
            self.dirty = true;
        }
    }

    /// Vanilla `spawnChance`.
    #[must_use]
    pub(crate) const fn spawn_chance(&self) -> i32 {
        self.spawn_chance
    }

    /// Vanilla `setSpawnChance`.
    pub(crate) const fn set_spawn_chance(&mut self, spawn_chance: i32) {
        if self.spawn_chance != spawn_chance {
            self.spawn_chance = spawn_chance;
            self.dirty = true;
        }
    }

    /// Vanilla `SavedData.isDirty`.
    #[must_use]
    pub(crate) const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Vanilla `SavedData.setDirty(false)` after a save.
    pub(crate) const fn mark_saved(&mut self) {
        self.dirty = false;
    }
}

/// Vanilla `WanderingTraderSpawner`. The trader data lives on the world so it
/// is saved with it, like vanilla's overworld `SavedDataStorage`.
pub(crate) struct WanderingTraderSpawner {
    tick_delay: i32,
}

impl Default for WanderingTraderSpawner {
    fn default() -> Self {
        Self {
            tick_delay: TICK_DELAY,
        }
    }
}

impl CustomSpawner for WanderingTraderSpawner {
    fn tick(&mut self, world: &Arc<World>, _spawn_enemies: bool) {
        self.tick_with(world, &mut rand::rng());
    }
}

impl WanderingTraderSpawner {
    /// Vanilla `tick`, with the spawner's random passed in.
    pub(super) fn tick_with(&mut self, world: &Arc<World>, random: &mut impl Rng) {
        if !world.get_game_rule(&SPAWN_WANDERING_TRADERS) {
            return;
        }
        self.tick_delay -= 1;
        if self.tick_delay > 0 {
            return;
        }
        self.tick_delay = TICK_DELAY;
        let chance_to_spawn = {
            let mut data = world.wandering_trader_data.lock();
            let spawn_delay = data.spawn_delay() - TICK_DELAY;
            data.set_spawn_delay(spawn_delay);
            if spawn_delay > 0 {
                return;
            }
            data.set_spawn_delay(DEFAULT_SPAWN_DELAY);
            let chance_to_spawn = data.spawn_chance();
            data.set_spawn_chance(
                (chance_to_spawn + SPAWN_CHANCE_INCREASE).clamp(MIN_SPAWN_CHANCE, MAX_SPAWN_CHANCE),
            );
            chance_to_spawn
        };
        if random.random_range(0..100) <= chance_to_spawn && spawn(world, random) {
            world
                .wandering_trader_data
                .lock()
                .set_spawn_chance(MIN_SPAWN_CHANCE);
        }
    }
}

/// Vanilla `spawn`. Without a living player the roll counts as spent, so the
/// chance resets as if a trader had come.
pub(super) fn spawn(world: &Arc<World>, random: &mut impl Rng) -> bool {
    // Vanilla `ServerLevel.getRandomPlayer`: a uniform pick among living players.
    let mut players = Vec::new();
    world.players.iter_players(|_, player| {
        if player.is_alive() {
            players.push(Arc::clone(player));
        }
        true
    });
    if players.is_empty() {
        return true;
    }
    let player = &players[random.random_range(0..players.len())];
    if random.random_range(0..SPAWN_ONE_IN_X_CHANCE) != 0 {
        return false;
    }
    let player_pos = player.block_position();
    let reference_pos = find_meeting_point(world, player_pos).unwrap_or(player_pos);
    let Some(spawn_pos) = find_spawn_position_near(world, reference_pos, TRADER_RADIUS, random)
    else {
        return false;
    };
    if !has_enough_space(world, spawn_pos) {
        return false;
    }
    if world
        .biome_at(spawn_pos)
        .is_some_and(|biome| biome.has_tag(&BiomeTag::WITHOUT_WANDERING_TRADER_SPAWNS))
    {
        return false;
    }
    let Some(trader) = spawn_at(world, &WANDERING_TRADER, spawn_pos) else {
        return false;
    };
    for _ in 0..LLAMAS_PER_TRADER {
        try_to_spawn_llama_for(world, &trader, random);
    }
    if let Some(wandering_trader) = trader.downcast_ref::<WanderingTraderEntity>() {
        wandering_trader.set_despawn_delay(TRADER_DESPAWN_DELAY);
        wandering_trader.set_wander_target(Some(reference_pos));
    }
    if let Some(mob) = trader.as_mob() {
        mob.set_home_to(reference_pos, TRADER_HOME_RADIUS);
    }
    true
}

/// Vanilla `poiManager.find(MEETING, any, playerPos, 48, ANY)`: the first bell
/// within 48 blocks, in chunk order.
fn find_meeting_point(world: &World, center: BlockPos) -> Option<BlockPos> {
    let meeting = vanilla_poi_types::MEETING.try_id()?;
    let radius_sq = i64::from(TRADER_RADIUS) * i64::from(TRADER_RADIUS);
    world
        .poi_storage
        .lock()
        .get_in_horizontal_square(
            &|poi_type| poi_type == meeting,
            center,
            TRADER_RADIUS,
            OccupationStatus::Any,
        )
        .into_iter()
        .map(|(pos, _)| pos)
        .find(|pos| {
            let (dx, dy, dz) = (
                i64::from(pos.x() - center.x()),
                i64::from(pos.y() - center.y()),
                i64::from(pos.z() - center.z()),
            );
            dx * dx + dy * dy + dz * dz <= radius_sq
        })
}

/// Vanilla `tryToSpawnLlamaFor`: a trader llama leashed to the trader.
fn try_to_spawn_llama_for(world: &Arc<World>, trader: &SharedEntity, random: &mut impl Rng) {
    let Some(spawn_pos) =
        find_spawn_position_near(world, trader.block_position(), LLAMA_RADIUS, random)
    else {
        return;
    };
    let Some(llama) = spawn_at(world, &TRADER_LLAMA, spawn_pos) else {
        return;
    };
    if let Some(leashable) = llama.as_leashable() {
        let _ = leashable.set_leashed_to(trader);
    }
}

/// Vanilla `EntityType.spawn(level, pos, EntitySpawnReason.EVENT)`.
fn spawn_at(world: &Arc<World>, entity_type: EntityTypeRef, pos: BlockPos) -> Option<SharedEntity> {
    spawn_entity(
        world,
        EntitySpawnRequest {
            entity_type,
            placement: EntitySpawnPlacement::Block {
                pos,
                try_move_down: false,
                moved_up: false,
            },
            reason: EntitySpawnReason::Event,
            finalize_spawn: true,
            play_ambient_sound: true,
            item_stack: None,
            user_is_operator: false,
        },
    )
    .map_err(|error| log::debug!("Wandering trader spawn failed: {error:?}"))
    .ok()
}

/// Vanilla `findSpawnPositionNear`: ten tries at the surface within `radius`
/// blocks, placed by the wandering trader's spawn rules.
pub(super) fn find_spawn_position_near(
    world: &World,
    reference_pos: BlockPos,
    radius: i32,
    random: &mut impl Rng,
) -> Option<BlockPos> {
    let heightmap = SpawnPlacements::heightmap_type(&WANDERING_TRADER);
    let placement = SpawnPlacements::placement_type(&WANDERING_TRADER);
    (0..NUMBER_OF_SPAWN_ATTEMPTS).find_map(|_| {
        let x = reference_pos.x() + random.random_range(0..radius * 2) - radius;
        let z = reference_pos.z() + random.random_range(0..radius * 2) - radius;
        let y = world.level_height_at(heightmap, x, z);
        let pos = BlockPos::new(x, y, z);
        placement
            .is_spawn_position_ok(world, pos, &WANDERING_TRADER)
            .then_some(pos)
    })
}

/// Vanilla `hasEnoughSpace`: the 2x3x2 box from the spawn position has no
/// collision.
pub(super) fn has_enough_space(world: &World, spawn_pos: BlockPos) -> bool {
    (0..=1).all(|dx| {
        (0..=2).all(|dy| {
            (0..=1).all(|dz| {
                let pos = spawn_pos.offset(dx, dy, dz);
                world
                    .get_block_state(pos)
                    .get_collision_shape_at(pos)
                    .is_empty()
            })
        })
    })
}
