//! Regionized ticking: the parallel island phase of a world tick.
//!
//! Runs only with [`regionized_ticking_enabled`]. After scheduled ticks and the
//! chunk-source phase, the world's ticking chunks are split into
//! [`Islands`]. Each island then runs, on the tick pool and in Vanilla order for
//! its own chunks: natural spawning, random ticks, its block events and its
//! entities. Islands are far enough apart that none can reach another within a
//! tick, so only the interleaving between islands differs from the sequential
//! tick, and that interleaving is not observable.
//!
//! Work that is not local to an island stays sequential: the world's own block
//! event queue (events queued by packets and commands between ticks) runs before
//! the islands, and entities outside every island tick after them.

use rayon::iter::{IndexedParallelIterator as _, IntoParallelIterator as _, ParallelIterator as _};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{
    Arc, BlockRandomPositionGenerator, ChunkMap, ChunkPos, GameplayChunkLookupCacheScope,
    TickingChunkSnapshot,
};
use crate::entity::Entity as _;
use crate::world::World;
use crate::world::region::{Islands, RegionScope};
use crate::world::tick_pool::install_on_tick_pool;

pub(crate) use crate::world::region::regionized_ticking_enabled;

/// Per-island work collected before the parallel phase.
#[derive(Default)]
struct IslandWork {
    /// Indices into the snapshot's block list for random ticks, in snapshot order.
    random_chunks: Vec<usize>,
    /// Chunks a player is close enough to spawn mobs in.
    spawning_chunks: Vec<ChunkPos>,
}

/// `(entity id, position)` of every player, before any island moves them.
fn player_snapshot(world: &World) -> Vec<(i32, glam::DVec3)> {
    let mut players = Vec::new();
    world.players.iter_players(|_, player| {
        players.push((player.id(), player.position()));
        true
    });
    players
}

/// Chunks whose entities tick: the random-tick set of the snapshot.
fn entity_ticking_chunks(tickable_chunks: &TickingChunkSnapshot) -> FxHashSet<ChunkPos> {
    tickable_chunks
        .random_chunk_indices
        .iter()
        .map(|&index| tickable_chunks.block[index].pos)
        .collect()
}

impl ChunkMap {
    /// Runs spawning, random ticks, block events and entities island by island.
    ///
    /// Marks the chunks entity ticks dirtied, as the sequential entity phase does.
    pub(super) fn tick_islands(
        &self,
        world: &Arc<World>,
        tickable_chunks: &TickingChunkSnapshot,
        random_tick_speed: u32,
        runs_normally: bool,
    ) {
        let _span = tracing::trace_span!("tick_islands").entered();
        let islands = Islands::build(tickable_chunks.block.iter().map(|chunk| chunk.pos));

        // The world's own queue holds events queued outside any island (packets,
        // commands). They run first, as they would at the start of Vanilla's pass.
        if runs_normally {
            world.run_block_events();
        }

        let mut work: Vec<IslandWork> = (0..islands.len()).map(|_| IslandWork::default()).collect();
        let mut holders = FxHashMap::default();
        if runs_normally {
            let players = world.spawning_player_positions();
            for tickable_chunk in &tickable_chunks.block {
                let Some(island) = islands.island_of(tickable_chunk.pos) else {
                    continue;
                };
                if tickable_chunk.holder.try_full_chunk().is_some()
                    && World::any_player_close_enough_for_spawning(&players, tickable_chunk.pos)
                {
                    work[island].spawning_chunks.push(tickable_chunk.pos);
                    holders.insert(tickable_chunk.pos, &tickable_chunk.holder);
                }
            }
            for &index in &tickable_chunks.random_chunk_indices {
                if let Some(island) = islands.island_of(tickable_chunks.block[index].pos) {
                    work[island].random_chunks.push(index);
                }
            }
        }
        let entity_ticking = entity_ticking_chunks(tickable_chunks);
        let spawning = runs_normally
            .then(|| world.prepare_spawning(self.scheduling.natural_spawn_chunk_count()))
            .flatten();

        let entity_manager = world.entity_manager();
        let (island_entities, remainder) = entity_manager
            .partition_ticking_entities(islands.len(), |chunk| islands.island_of(chunk));

        let players = player_snapshot(world);

        let island_count = islands.len();
        let results: Vec<FxHashSet<ChunkPos>> = install_on_tick_pool(|| {
            work.into_par_iter()
                .zip(island_entities)
                .enumerate()
                .with_max_len(1)
                .map(|(island, (work, entities))| {
                    let queues = islands.queues(island, &players);
                    let _region = RegionScope::enter(world, Arc::clone(&queues));
                    let lookup_cache = GameplayChunkLookupCacheScope::enter(self);

                    queues.set_handling_tick(true);
                    if runs_normally {
                        let mut spawning_chunks = work.spawning_chunks;
                        let mut island_spawning = spawning.clone();
                        world.run_spawning_chunks(
                            &mut spawning_chunks,
                            island_spawning.as_mut(),
                            |pos| {
                                if let Some(chunk) =
                                    holders.get(&pos).and_then(|holder| holder.try_full_chunk())
                                {
                                    chunk.common().increment_inhabited_time();
                                }
                                if entity_ticking.contains(&pos) {
                                    world.tick_thunder(pos);
                                }
                            },
                        );

                        if random_tick_speed > 0 {
                            // As in the sequential pass, positions come from Vanilla's
                            // coordinate LCG seeded per tick from runtime RNG.
                            let mut random_positions =
                                BlockRandomPositionGenerator::from_runtime_rng();
                            for index in work.random_chunks {
                                let tickable_chunk = &tickable_chunks.block[index];
                                if tickable_chunk.randomly_ticking_sections.is_empty() {
                                    continue;
                                }
                                if let Some(chunk) = tickable_chunk.holder.try_full_chunk() {
                                    chunk.tick_random_blocks(
                                        world,
                                        random_tick_speed,
                                        &mut random_positions,
                                    );
                                }
                            }
                        }

                        world.run_block_events();
                    }
                    // Vanilla clears this before ticking entities and block entities.
                    queues.set_handling_tick(false);

                    let dirty = entity_manager.tick_entity_list(entities, runs_normally);
                    let _ = lookup_cache.finish();
                    world.requeue_island_block_events(&queues);
                    dirty
                })
                .collect()
        });

        let mut dirty_chunks = entity_manager.tick_entity_list(remainder, runs_normally);
        for island_dirty in results {
            dirty_chunks.extend(island_dirty);
        }
        tracing::trace!(island_count, "Ticked islands");
        for chunk in dirty_chunks {
            world.mark_chunk_dirty(chunk);
        }
    }
}
