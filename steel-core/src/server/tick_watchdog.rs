//! Detects a stalled game tick, like vanilla `ServerWatchdog`.
//!
//! Vanilla halts the server when one tick runs longer than `max-tick-time` (60 s by default).
//! Steel does the same, after logging which step of the game loop stalled and the state of
//! the packet queue, so a hang leaves a diagnosis and the supervisor can restart the server.

use std::{
    process,
    sync::{
        Arc, Weak,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::Server;

/// Vanilla `max-tick-time` default.
const MAX_TICK_TIME: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The step of the game loop that is currently running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum TickStep {
    /// Waiting for the next tick to be due. Never reported as a stall.
    Sleeping,
    DrainPackets,
    Commands,
    Worlds,
    Connections,
    Jobs,
    PlayerJoins,
    WorldChanges,
    DomainSwitches,
    Autosave,
    TabList,
    OverloadPacketProgress,
}

impl TickStep {
    const ALL: [Self; 12] = [
        Self::Sleeping,
        Self::DrainPackets,
        Self::Commands,
        Self::Worlds,
        Self::Connections,
        Self::Jobs,
        Self::PlayerJoins,
        Self::WorldChanges,
        Self::DomainSwitches,
        Self::Autosave,
        Self::TabList,
        Self::OverloadPacketProgress,
    ];

    fn from_u8(value: u8) -> Self {
        Self::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or(Self::Sleeping)
    }
}

/// Shared between the game loop, which records its progress, and the watchdog thread.
pub(super) struct TickWatchdog {
    started: Instant,
    step: AtomicU8,
    /// Milliseconds since `started` at which the current step began.
    step_since_ms: AtomicU64,
    tick_count: AtomicU64,
}

impl TickWatchdog {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            step: AtomicU8::new(TickStep::Sleeping as u8),
            step_since_ms: AtomicU64::new(0),
            tick_count: AtomicU64::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Records that the game loop entered `step`.
    pub(super) fn enter(&self, step: TickStep) {
        self.step_since_ms.store(self.now_ms(), Ordering::Release);
        self.step.store(step as u8, Ordering::Release);
    }

    /// Records that a new tick started.
    pub(super) fn start_tick(&self, tick_count: u64) {
        self.tick_count.store(tick_count, Ordering::Release);
    }

    /// Returns the stalled step and for how long it has run, if it exceeds `limit`.
    fn stalled(&self, limit: Duration) -> Option<(TickStep, Duration)> {
        let step = TickStep::from_u8(self.step.load(Ordering::Acquire));
        if step == TickStep::Sleeping {
            return None;
        }
        let since = self.step_since_ms.load(Ordering::Acquire);
        let running = Duration::from_millis(self.now_ms().saturating_sub(since));
        (running > limit).then_some((step, running))
    }

    /// Starts the watchdog thread. It exits once the server is dropped.
    ///
    /// A dedicated OS thread is required: a stalled game loop may be holding the async
    /// runtime's attention, and the check must run even when no async task makes progress.
    pub(super) fn spawn(self: &Arc<Self>, server: Weak<Server>) {
        let watchdog = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("tick-watchdog".to_owned())
            .spawn(move || {
                loop {
                    thread::sleep(CHECK_INTERVAL);
                    let Some(server) = server.upgrade() else {
                        return;
                    };
                    if server.cancel_token.is_cancelled() {
                        return;
                    }
                    let Some((step, running)) = watchdog.stalled(MAX_TICK_TIME) else {
                        continue;
                    };
                    let tick = watchdog.tick_count.load(Ordering::Acquire);
                    log::error!(
                        "A single server tick took {:.2} seconds (should be max 0.05)",
                        running.as_secs_f64()
                    );
                    log::error!("Considering it to be crashed, server will forcibly shutdown.");
                    log::error!(
                        "Stalled in game loop step {step:?} of tick {tick}; packet queue: {}",
                        server.packet_processor.debug_state()
                    );
                    log::logger().flush();
                    // Give the async logger a moment to write before the process ends.
                    thread::sleep(Duration::from_secs(1));
                    process::exit(1);
                }
            });
        if let Err(error) = spawned {
            log::error!("Failed to start the tick watchdog: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeping_between_ticks_is_never_a_stall() {
        let watchdog = TickWatchdog::new();
        watchdog.enter(TickStep::Sleeping);
        assert_eq!(watchdog.stalled(Duration::ZERO), None);
    }

    #[test]
    fn a_running_step_past_the_limit_is_reported() {
        let watchdog = TickWatchdog::new();
        watchdog.enter(TickStep::DrainPackets);
        thread::sleep(Duration::from_millis(20));
        assert!(matches!(
            watchdog.stalled(Duration::from_millis(5)),
            Some((TickStep::DrainPackets, _))
        ));
        assert_eq!(watchdog.stalled(Duration::from_secs(60)), None);
    }
}
