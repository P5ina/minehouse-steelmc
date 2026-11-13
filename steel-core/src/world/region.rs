//! Per-island tick context, the groundwork for regionized ticking.
//!
//! With regionized ticking, a world's ticking chunks are split every tick into
//! "islands": groups of chunks far enough apart that nothing in one island can
//! reach another within a tick. Islands then tick in parallel, each in Vanilla
//! order. Vanilla's per-level queues that order work inside a tick (the
//! collecting neighbor updater, block events and `isHandlingTick`) must belong
//! to the island, or two islands would interleave their chains in one queue.
//!
//! A [`RegionScope`] binds an island's [`RegionQueues`] to the current thread for
//! one world. While it is active, that world's neighbor updates, block events and
//! handling-tick flag use the island's queues; everywhere else (the sequential
//! phase, packet handling, commands, tests) the world's own queues are used, so
//! behavior without regions is unchanged.
//!
//! In debug builds an island also records the chunks it owns, and block writes
//! outside them panic: Rust prevents data races here, but not one island writing
//! into another, which would break Vanilla ordering silently.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::env;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};
use steel_utils::ChunkPos;
use steel_utils::locks::SyncMutex;

use super::World;
use super::block_event::BlockEventQueue;
use super::block_updates::NeighborUpdateState;

/// The tick-ordering queues owned by one island (or by the world itself).
#[derive(Default)]
pub(crate) struct RegionQueues {
    /// Vanilla `CollectingNeighborUpdater` stack and layer state.
    pub(super) neighbor_updates: NeighborUpdateState,
    /// Vanilla `ServerLevel.blockEvents`.
    pub(super) block_events: SyncMutex<BlockEventQueue>,
    /// Vanilla `ServerLevel.handlingTick`.
    pub(super) handling_tick: AtomicBool,
    /// Island cells this island may write to. `None` allows every chunk.
    owned_cells: Option<Arc<FxHashSet<IslandCell>>>,
}

impl RegionQueues {
    /// Queues for an island that may write only inside `owned_cells`.
    #[must_use]
    pub(crate) fn for_island(owned_cells: Arc<FxHashSet<IslandCell>>) -> Self {
        Self {
            owned_cells: Some(owned_cells),
            ..Self::default()
        }
    }

    /// Whether this island may write to `chunk`.
    #[must_use]
    pub(crate) fn owns(&self, chunk: ChunkPos) -> bool {
        self.owned_cells
            .as_ref()
            .is_none_or(|owned| owned.contains(&IslandCell::of(chunk)))
    }

    pub(super) fn is_handling_tick(&self) -> bool {
        self.handling_tick.load(Ordering::Relaxed)
    }

    pub(crate) fn set_handling_tick(&self, handling: bool) {
        self.handling_tick.store(handling, Ordering::Relaxed);
    }
}

struct ActiveRegion {
    /// Address of the world the queues belong to; queues never leak to other worlds.
    world: usize,
    queues: Arc<RegionQueues>,
}

thread_local! {
    static ACTIVE_REGION: RefCell<Option<ActiveRegion>> = const { RefCell::new(None) };
}

/// Binds an island's queues to the current thread until dropped.
///
/// Scopes nest: dropping one restores the binding that was active before it.
#[must_use = "the island's queues are only active while the scope is alive"]
pub(crate) struct RegionScope {
    previous: Option<ActiveRegion>,
}

impl RegionScope {
    /// Makes `queues` the current thread's queues for `world`.
    pub(crate) fn enter(world: &World, queues: Arc<RegionQueues>) -> Self {
        let previous = ACTIVE_REGION.with(|active| {
            active.borrow_mut().replace(ActiveRegion {
                world: world_key(world),
                queues,
            })
        });
        Self { previous }
    }
}

impl Drop for RegionScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        ACTIVE_REGION.with(|active| *active.borrow_mut() = previous);
    }
}

fn world_key(world: &World) -> usize {
    ptr::from_ref(world) as usize
}

impl World {
    /// The island queues bound to this thread for this world, if any.
    pub(super) fn active_region(&self) -> Option<Arc<RegionQueues>> {
        let key = world_key(self);
        ACTIVE_REGION.with(|active| {
            active
                .borrow()
                .as_ref()
                .filter(|region| region.world == key)
                .map(|region| Arc::clone(&region.queues))
        })
    }

    /// Runs `f` with the queues that order this thread's work in this world.
    pub(super) fn with_region_queues<R>(&self, f: impl FnOnce(&RegionQueues) -> R) -> R {
        match self.active_region() {
            Some(region) => f(&region),
            None => f(&self.region_queues),
        }
    }

    /// Panics in debug builds when the current island writes outside its chunks.
    #[inline]
    pub(super) fn debug_assert_region_owns(&self, chunk: ChunkPos) {
        if cfg!(debug_assertions)
            && let Some(region) = self.active_region()
        {
            assert!(
                region.owns(chunk),
                "island wrote to chunk {chunk:?} it does not own; cross-island work must be deferred"
            );
        }
    }
}

/// Whether worlds tick their islands in parallel.
///
/// Experimental and off by default: set `STEEL_REGIONIZED_TICKING=1`. When off,
/// the world tick is exactly the sequential Vanilla order.
#[must_use]
pub(crate) fn regionized_ticking_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let enabled = env::var("STEEL_REGIONIZED_TICKING")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        if enabled {
            log::info!("Regionized ticking is enabled (STEEL_REGIONIZED_TICKING)");
        }
        enabled
    })
}

/// Width of an island cell in chunks.
///
/// Ticking chunks are grouped into cells of `CELL_CHUNKS`×`CELL_CHUNKS`. Cells
/// within [`ISLAND_LINK_CELLS`] of each other join one island, and an island owns
/// its cells plus a one-cell ring. Islands are therefore separated by at least
/// two empty cells (8 chunks, 128 blocks) of non-ticking chunks, and their owned
/// areas never overlap.
pub(crate) const CELL_CHUNKS: i32 = 4;

/// Chebyshev distance, in cells, at which two occupied cells join one island.
const ISLAND_LINK_CELLS: i32 = 2;

/// A `CELL_CHUNKS`-wide square of chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct IslandCell {
    x: i32,
    z: i32,
}

impl IslandCell {
    /// The cell containing `chunk`.
    #[must_use]
    pub(crate) const fn of(chunk: ChunkPos) -> Self {
        Self {
            x: chunk.0.x.div_euclid(CELL_CHUNKS),
            z: chunk.0.y.div_euclid(CELL_CHUNKS),
        }
    }
}

/// This tick's islands: connected groups of ticking chunks.
#[derive(Default)]
pub(crate) struct Islands {
    /// Owned cells (occupied cells and their one-cell ring) to island index.
    owner: FxHashMap<IslandCell, usize>,
    /// Owned cells of each island.
    owned: Vec<Arc<FxHashSet<IslandCell>>>,
}

impl Islands {
    /// Groups `ticking_chunks` into islands.
    #[must_use]
    pub(crate) fn build(ticking_chunks: impl IntoIterator<Item = ChunkPos>) -> Self {
        let occupied: FxHashSet<IslandCell> =
            ticking_chunks.into_iter().map(IslandCell::of).collect();
        // Deterministic island numbering: flood from cells in coordinate order.
        let mut seeds: Vec<IslandCell> = occupied.iter().copied().collect();
        seeds.sort_unstable_by_key(|cell| (cell.x, cell.z));

        let mut island_of_cell: FxHashMap<IslandCell, usize> = FxHashMap::default();
        let mut island_cells: Vec<Vec<IslandCell>> = Vec::new();
        let mut queue = VecDeque::new();
        for seed in seeds {
            if island_of_cell.contains_key(&seed) {
                continue;
            }
            let island = island_cells.len();
            let mut cells = Vec::new();
            island_of_cell.insert(seed, island);
            queue.push_back(seed);
            while let Some(cell) = queue.pop_front() {
                cells.push(cell);
                for dx in -ISLAND_LINK_CELLS..=ISLAND_LINK_CELLS {
                    for dz in -ISLAND_LINK_CELLS..=ISLAND_LINK_CELLS {
                        let next = IslandCell {
                            x: cell.x + dx,
                            z: cell.z + dz,
                        };
                        if occupied.contains(&next) && !island_of_cell.contains_key(&next) {
                            island_of_cell.insert(next, island);
                            queue.push_back(next);
                        }
                    }
                }
            }
            island_cells.push(cells);
        }

        let mut owner_of_cell = FxHashMap::default();
        let mut owned = Vec::with_capacity(island_cells.len());
        for (island, cells) in island_cells.into_iter().enumerate() {
            let mut island_owned = FxHashSet::default();
            for cell in cells {
                for dx in -1..=1 {
                    for dz in -1..=1 {
                        let ring = IslandCell {
                            x: cell.x + dx,
                            z: cell.z + dz,
                        };
                        island_owned.insert(ring);
                        let previous = owner_of_cell.insert(ring, island);
                        debug_assert!(
                            previous.is_none_or(|previous| previous == island),
                            "island owned areas must not overlap"
                        );
                    }
                }
            }
            owned.push(Arc::new(island_owned));
        }
        Self {
            owner: owner_of_cell,
            owned,
        }
    }

    /// Number of islands.
    #[must_use]
    pub(crate) const fn len(&self) -> usize {
        self.owned.len()
    }

    /// The island owning `chunk`, if any.
    #[must_use]
    pub(crate) fn island_of(&self, chunk: ChunkPos) -> Option<usize> {
        self.owner.get(&IslandCell::of(chunk)).copied()
    }

    /// Fresh queues for `island`, limited to its owned cells.
    #[must_use]
    pub(crate) fn queues(&self, island: usize) -> Arc<RegionQueues> {
        Arc::new(RegionQueues::for_island(Arc::clone(&self.owned[island])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(center_x: i32, center_z: i32, radius: i32) -> Vec<ChunkPos> {
        let mut chunks = Vec::new();
        for x in -radius..=radius {
            for z in -radius..=radius {
                chunks.push(ChunkPos::new(center_x + x, center_z + z));
            }
        }
        chunks
    }

    #[test]
    fn far_groups_are_separate_islands() {
        let mut chunks = square(0, 0, 10);
        chunks.extend(square(40, 0, 10));
        let islands = Islands::build(chunks);
        assert_eq!(islands.len(), 2);
        assert_ne!(
            islands.island_of(ChunkPos::new(0, 0)),
            islands.island_of(ChunkPos::new(40, 0))
        );
    }

    #[test]
    fn close_groups_join_one_island() {
        let mut chunks = square(0, 0, 10);
        chunks.extend(square(25, 0, 10));
        let islands = Islands::build(chunks);
        assert_eq!(islands.len(), 1);
    }

    #[test]
    fn island_owns_a_ring_around_its_chunks() {
        let islands = Islands::build(square(0, 0, 2));
        let queues = islands.queues(0);
        assert!(queues.owns(ChunkPos::new(5, 5)));
        assert!(!queues.owns(ChunkPos::new(20, 0)));
        assert_eq!(islands.island_of(ChunkPos::new(-6, 0)), Some(0));
        assert_eq!(islands.island_of(ChunkPos::new(30, 0)), None);
    }

    #[test]
    fn world_queues_own_everything() {
        assert!(RegionQueues::default().owns(ChunkPos::new(1_000, -1_000)));
    }
}
