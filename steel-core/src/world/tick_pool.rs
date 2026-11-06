//! Worker pool for read-only work fanned out from a world tick.
//!
//! World ticks run on their own `world-tick-N` threads. Work that only reads the
//! world (and produces results the tick thread then applies in vanilla order) can
//! be split across this pool. It is separate from the generation and chunk
//! encoding pools so tick work never waits behind queued chunk jobs.

use std::sync::OnceLock;

use rayon::{ThreadPool, ThreadPoolBuilder};

/// Below this many items per split, rayon overhead outweighs the parallel work.
pub(crate) const TICK_POOL_MIN_SPLIT: usize = 64;

/// Runs `op` on the tick pool, so rayon iterators inside it use the tick workers.
///
/// If the pool could not start, `op` runs here and its rayon iterators fall back
/// to rayon's global pool.
pub(crate) fn install_on_tick_pool<R: Send>(op: impl FnOnce() -> R + Send) -> R {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        ThreadPoolBuilder::new()
            .thread_name(|i| format!("rayon-tick-{i}"))
            .build()
            .inspect_err(|error| log::warn!("Failed to start the tick pool: {error}"))
            .ok()
    });
    match pool {
        Some(pool) => pool.install(op),
        None => op(),
    }
}
