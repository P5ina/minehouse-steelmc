//! Vanilla `CustomSpawner`: spawners that run once per level tick after the
//! natural spawner, outside the per-chunk mob caps.

mod patrol;
mod phantom;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use steel_registry::dimension_type::DimensionTypeRef;
use steel_registry::vanilla_dimension_types;
use steel_registry::vanilla_game_rules::SPAWN_MOBS;

pub(crate) use patrol::PatrolSpawner;
pub(crate) use phantom::PhantomSpawner;

use crate::world::World;

/// Vanilla `CustomSpawner`.
pub(crate) trait CustomSpawner: Send {
    /// Vanilla `CustomSpawner.tick`.
    fn tick(&mut self, world: &Arc<World>, spawn_enemies: bool);
}

/// Vanilla `MinecraftServer.createLevels` gives the custom spawners to the
/// overworld only. Steel worlds are keyed by domain, so the overworld
/// dimension type stands in for the overworld level key.
pub(crate) fn custom_spawners_for(dimension_type: DimensionTypeRef) -> Vec<Box<dyn CustomSpawner>> {
    if dimension_type == &vanilla_dimension_types::OVERWORLD {
        vec![
            Box::new(PhantomSpawner::default()),
            Box::new(PatrolSpawner::default()),
        ]
    } else {
        Vec::new()
    }
}

impl World {
    /// Vanilla `ServerChunkCache.tickChunks` tail: custom spawners tick only
    /// while mob spawning is enabled.
    pub(crate) fn tick_custom_spawners(self: &Arc<Self>) {
        if !self.get_game_rule(&SPAWN_MOBS) {
            return;
        }
        let spawn_enemies = self.is_spawning_monsters();
        for spawner in self.custom_spawners.lock().iter_mut() {
            spawner.tick(self, spawn_enemies);
        }
    }
}
