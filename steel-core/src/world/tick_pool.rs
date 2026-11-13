//! Worker pool for read-only work fanned out from a world tick.
//!
//! World ticks run on their own `world-tick-N` threads. Work that only reads the
//! world (and produces results the tick thread then applies in vanilla order) can
//! be split across this pool. It is separate from the generation and chunk
//! encoding pools so tick work never waits behind queued chunk jobs.

use std::env;
use std::process::Command;
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
        let mut builder = ThreadPoolBuilder::new().thread_name(|i| format!("rayon-tick-{i}"));
        if let Some(threads) = tick_pool_threads() {
            log::info!("Tick pool: {threads} threads");
            builder = builder.num_threads(threads);
        }
        builder
            .build()
            .inspect_err(|error| log::warn!("Failed to start the tick pool: {error}"))
            .ok()
    });
    match pool {
        Some(pool) => pool.install(op),
        None => op(),
    }
}

/// Thread count for the tick pool, or `None` for rayon's default (one per logical core).
///
/// `STEEL_TICK_THREADS` wins when set. Otherwise, on Apple silicon, the pool uses only as many
/// threads as there are performance cores. Regionized ticking waits for the slowest island,
/// and an island that lands on an efficiency core makes the whole tick wait: on an M4 Pro
/// (8P + 4E, 20 bots) 12 threads gave 52 ms per tick and 4 threads 20 ms.
fn tick_pool_threads() -> Option<usize> {
    if let Some(threads) = env::var("STEEL_TICK_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|&threads| threads > 0)
    {
        return Some(threads);
    }
    performance_cores()
}

/// Performance cores on macOS (`hw.perflevel0.physicalcpu`); `None` elsewhere or when unknown.
fn performance_cores() -> Option<usize> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("sysctl")
        .args(["-n", "hw.perflevel0.physicalcpu"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|&cores| cores > 0)
}
