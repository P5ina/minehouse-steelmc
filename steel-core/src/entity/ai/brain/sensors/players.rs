use std::sync::Arc;

use steel_registry::vanilla_attributes;

use crate::entity::ai::brain::memory_types::{
    NEAREST_PLAYERS, NEAREST_VISIBLE_ATTACKABLE_PLAYER, NEAREST_VISIBLE_ATTACKABLE_PLAYERS,
    NEAREST_VISIBLE_PLAYER,
};
use crate::entity::ai::brain::nearest_visible::{is_entity_attackable, is_entity_targetable};
use crate::entity::ai::brain::{Memories, Sensor};
use crate::entity::{Entity as _, LivingEntity, SharedEntity};
use crate::world::World;

/// Remembers the players within follow range, closest first, and which of
/// them the body can see and attack (vanilla `PlayerSensor`).
pub struct PlayerSensor;

impl<E: LivingEntity> Sensor<E> for PlayerSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_PLAYERS);
        memories.register(&NEAREST_VISIBLE_PLAYER);
        memories.register(&NEAREST_VISIBLE_ATTACKABLE_PLAYER);
        memories.register(&NEAREST_VISIBLE_ATTACKABLE_PLAYERS);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let follow_range = body
            .attributes()
            .lock()
            .required_value(vanilla_attributes::FOLLOW_RANGE);
        let position = body.position();
        let mut players: Vec<(SharedEntity, f64)> = Vec::new();
        world.players.iter_players(|_, player| {
            let distance = player.position().distance_squared(position);
            if !player.is_spectator() && distance < follow_range * follow_range {
                players.push((Arc::clone(player) as SharedEntity, distance));
            }
            true
        });
        players.sort_by(|a, b| a.1.total_cmp(&b.1));
        let players: Vec<SharedEntity> = players.into_iter().map(|(player, _)| player).collect();
        let visible: Vec<SharedEntity> = players
            .iter()
            .filter(|player| {
                player
                    .as_living_entity()
                    .is_some_and(|living| is_entity_targetable(world, body, living))
            })
            .cloned()
            .collect();
        let attackable: Vec<SharedEntity> = visible
            .iter()
            .filter(|player| {
                player
                    .as_living_entity()
                    .is_some_and(|living| is_entity_attackable(world, body, living))
            })
            .cloned()
            .collect();
        let mut memories = memories.lock();
        memories.set(&NEAREST_PLAYERS, Some(players));
        memories.set(&NEAREST_VISIBLE_PLAYER, visible.first().cloned());
        memories.set(
            &NEAREST_VISIBLE_ATTACKABLE_PLAYER,
            attackable.first().cloned(),
        );
        memories.set(&NEAREST_VISIBLE_ATTACKABLE_PLAYERS, Some(attackable));
    }
}
