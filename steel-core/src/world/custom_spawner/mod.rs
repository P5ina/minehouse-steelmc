//! Vanilla `CustomSpawner`: spawners that run once per level tick after the
//! natural spawner, outside the per-chunk mob caps.

mod cat;
mod patrol;
mod phantom;
#[cfg(test)]
mod tests;
mod village_siege;
mod wandering_trader;

use std::sync::Arc;

use steel_registry::dimension_type::DimensionTypeRef;
use steel_registry::vanilla_dimension_types;
use steel_registry::vanilla_game_rules::SPAWN_MOBS;

pub(crate) use cat::CatSpawner;
pub(crate) use patrol::PatrolSpawner;
pub(crate) use phantom::PhantomSpawner;
pub(crate) use village_siege::VillageSiege;
pub(crate) use wandering_trader::{WanderingTraderData, WanderingTraderSpawner};

use steel_utils::{BlockPos, ChunkPos};

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
            Box::new(CatSpawner::default()),
            Box::new(VillageSiege::default()),
            Box::new(WanderingTraderSpawner::default()),
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

/// Vanilla `LevelReader.hasChunksAt(x0, z0, x1, z1)`: every chunk in the block
/// rectangle is loaded.
pub(super) fn has_chunks_at(world: &World, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
    let min = ChunkPos::from_block_pos(BlockPos::new(min_x, 0, min_z));
    let max = ChunkPos::from_block_pos(BlockPos::new(max_x, 0, max_z));
    (min.0.x..=max.0.x).all(|chunk_x| {
        (min.0.y..=max.0.y).all(|chunk_z| {
            world.is_full_chunk_loaded_at(BlockPos::new(chunk_x << 4, 0, chunk_z << 4))
        })
    })
}
