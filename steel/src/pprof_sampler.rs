//! Optional in-process sampling profiler (`--features pprof`).
//!
//! Set `STEEL_PPROF="<delay_s>:<duration_s>"`; after the delay the process is
//! sampled for the given duration and a self/inclusive time summary per function,
//! grouped by thread name, is written to `steel-pprof.txt`.

use rustc_hash::{FxHashMap as HashMap, FxHashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::thread;
use std::time::Duration;

pub fn start_from_env() {
    let Ok(spec) = env::var("STEEL_PPROF") else {
        return;
    };
    let mut parts = spec.split(':');
    let delay = parts.next().and_then(|v| v.parse().ok()).unwrap_or(60);
    let duration = parts.next().and_then(|v| v.parse().ok()).unwrap_or(30);
    let spawned = thread::Builder::new()
        .name("pprof".to_owned())
        .spawn(move || run(delay, duration));
    if let Err(err) = spawned {
        log::warn!("pprof: failed to spawn sampler thread: {err}");
    }
}

/// Samples, self counts, and inclusive counts for one thread.
type ThreadProfile = (isize, HashMap<String, isize>, HashMap<String, isize>);

fn run(delay: u64, duration: u64) {
    thread::sleep(Duration::from_secs(delay));
    log::info!("pprof: sampling for {duration}s");
    let guard = match pprof::ProfilerGuardBuilder::default()
        .frequency(997)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
    {
        Ok(guard) => guard,
        Err(err) => {
            log::warn!("pprof: {err}");
            return;
        }
    };
    thread::sleep(Duration::from_secs(duration));
    let report = match guard.report().build() {
        Ok(report) => report,
        Err(err) => {
            log::warn!("pprof: {err}");
            return;
        }
    };

    // thread -> (total, self per fn, inclusive per fn)
    let mut threads: HashMap<String, ThreadProfile> = HashMap::default();
    for (frames, count) in &report.data {
        let thread_name = frames
            .thread_name
            .trim_end_matches(char::is_numeric)
            .to_owned();
        let entry = threads.entry(thread_name).or_default();
        entry.0 += *count;
        let names: Vec<String> = frames
            .frames
            .iter()
            .flat_map(|symbols| symbols.iter().map(pprof::Symbol::name))
            .collect();
        if let Some(first) = names.first() {
            *entry.1.entry(short(first)).or_default() += *count;
        }
        let mut seen = FxHashSet::default();
        for name in &names {
            let name = short(name);
            if seen.insert(name.clone()) {
                *entry.2.entry(name).or_default() += *count;
            }
        }
    }

    let mut folded = String::new();
    for (frames, count) in &report.data {
        let mut names: Vec<String> = frames
            .frames
            .iter()
            .flat_map(|symbols| symbols.iter().map(|symbol| short(&symbol.name())))
            .collect();
        names.reverse();
        let _ = writeln!(
            folded,
            "{};{} {count}",
            frames.thread_name.trim_end_matches(char::is_numeric),
            names.join(";")
        );
    }
    if let Err(err) = fs::write("steel-pprof.folded", folded) {
        log::warn!("pprof: {err}");
    }

    let mut out = String::new();
    let mut by_total: Vec<_> = threads.into_iter().collect();
    by_total.sort_by_key(|(_, (total, _, _))| -*total);
    for (thread_name, (total, self_counts, incl)) in by_total {
        let _ = writeln!(out, "=== thread {thread_name}: {total} samples");
        let mut self_counts: Vec<_> = self_counts.into_iter().collect();
        self_counts.sort_by_key(|(_, c)| -*c);
        let _ = writeln!(out, "--- self");
        for (name, c) in self_counts.iter().take(60) {
            let _ = writeln!(out, "{:6.2}% {name}", pct(*c, total));
        }
        let mut incl: Vec<_> = incl.into_iter().collect();
        incl.sort_by_key(|(_, c)| -*c);
        let _ = writeln!(out, "--- inclusive");
        for (name, c) in incl.iter().take(200) {
            let _ = writeln!(out, "{:6.2}% {name}", pct(*c, total));
        }
    }
    match fs::write("steel-pprof.txt", out) {
        Ok(()) => log::info!("pprof: wrote steel-pprof.txt"),
        Err(err) => log::warn!("pprof: {err}"),
    }
}

#[expect(clippy::cast_precision_loss, reason = "sample counts are small")]
fn pct(count: isize, total: isize) -> f64 {
    count as f64 * 100.0 / total.max(1) as f64
}

/// Drops the hash suffix of a demangled Rust symbol and caps its length.
fn short(name: &str) -> String {
    let name = name.rsplit_once("::h").map_or(name, |(head, tail)| {
        if tail.len() == 16 && tail.bytes().all(|b| b.is_ascii_hexdigit()) {
            head
        } else {
            name
        }
    });
    name.chars().take(220).collect()
}
