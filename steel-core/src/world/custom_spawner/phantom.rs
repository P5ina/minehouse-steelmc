//! Vanilla `PhantomSpawner`: phantoms above players who have not slept.

use std::sync::Arc;

use glam::DVec3;
use rand::{Rng, RngExt as _};
use steel_registry::vanilla_custom_stats::TIME_SINCE_REST;
use steel_registry::vanilla_entities::PHANTOM;
use steel_registry::vanilla_game_rules::SPAWN_PHANTOMS;
use steel_utils::BlockPos;

use super::CustomSpawner;
use crate::entity::{
    Entity as _, EntitySpawnReason, SpawnGroupData, create_entity_instance,
    is_valid_empty_spawn_block,
};
use crate::player::Player;
use crate::world::{DifficultyInstance, World};

/// Vanilla `dayLength * 3`: phantoms need three days without rest.
const MIN_TICKS_SINCE_REST: i32 = 72_000;
/// Sky darkening from which the overworld counts as night for phantoms.
const MIN_SKY_DARKEN: u8 = 5;

/// Vanilla `PhantomSpawner`.
#[derive(Default)]
pub(crate) struct PhantomSpawner {
    next_tick: i32,
}

impl CustomSpawner for PhantomSpawner {
    fn tick(&mut self, world: &Arc<World>, spawn_enemies: bool) {
        if !spawn_enemies || !world.get_game_rule(&SPAWN_PHANTOMS) {
            return;
        }
        let mut random = rand::rng();
        self.next_tick -= 1;
        if self.next_tick > 0 {
            return;
        }
        self.next_tick += (60 + random.random_range(0..60)) * 20;
        if world.sky_darkening() < MIN_SKY_DARKEN && world.dimension_type.has_skylight {
            return;
        }

        let mut players = Vec::new();
        world.players.iter_players(|_, player| {
            players.push(Arc::clone(player));
            true
        });
        for player in players {
            if let Some((spawn_pos, difficulty)) = phantom_spawn_pos(world, &player, &mut random) {
                spawn_phantom_group(world, spawn_pos, difficulty, &mut random);
            }
        }
    }
}

/// The per-player half of vanilla `PhantomSpawner.tick`, up to the spawn
/// block check: the position a group appears at, with the regional difficulty
/// at the player.
pub(super) fn phantom_spawn_pos(
    world: &World,
    player: &Player,
    random: &mut impl Rng,
) -> Option<(BlockPos, DifficultyInstance)> {
    if player.is_spectator() {
        return None;
    }
    let player_pos = player.block_position();
    if world.dimension_type.has_skylight
        && (player_pos.y() < world.sea_level || !world.can_see_sky(player_pos))
    {
        return None;
    }
    let difficulty = world.current_difficulty_at(player_pos);
    if !difficulty.is_harder_than(random.random::<f32>() * 3.0) {
        return None;
    }
    let ticks_since_rest = player.custom_stat_value(&TIME_SINCE_REST).max(1);
    if random.random_range(0..ticks_since_rest) < MIN_TICKS_SINCE_REST {
        return None;
    }
    let above = 20 + random.random_range(0..15);
    let east = -10 + random.random_range(0..21);
    let south = -10 + random.random_range(0..21);
    let spawn_pos = player_pos.offset(east, above, south);
    is_valid_empty_spawn_block(world, spawn_pos, &PHANTOM).then_some((spawn_pos, difficulty))
}

/// Vanilla's group loop: `1 + nextInt(difficulty id + 1)` phantoms at one block.
fn spawn_phantom_group(
    world: &Arc<World>,
    spawn_pos: BlockPos,
    difficulty: DifficultyInstance,
    random: &mut impl Rng,
) {
    let mut group_data: Option<SpawnGroupData> = None;
    let group_size = 1 + random.random_range(0..=i32::from(difficulty.difficulty() as u8));
    let (x, y, z) = spawn_pos.get_bottom_center();
    let position = DVec3::new(x, y, z);
    for _ in 0..group_size {
        // Vanilla `EntityType.create` returns null for a type it cannot build;
        // Steel reports that as an error and the phantom is skipped the same way.
        let Ok(phantom) = create_entity_instance(world, &PHANTOM, position) else {
            continue;
        };
        phantom.snap_to(position, 0.0, 0.0);
        if let Some(mob) = phantom.as_mob() {
            group_data = mob.finalize_spawn(world, EntitySpawnReason::Natural, group_data.take());
        }
        if let Err(error) = world.try_add_fresh_entity_with_passengers(phantom) {
            log::debug!("Phantom spawn failed: {error:?}");
        }
    }
}
