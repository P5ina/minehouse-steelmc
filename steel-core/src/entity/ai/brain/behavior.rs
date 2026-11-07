//! Brain behaviors (vanilla `BehaviorControl`, `Behavior` and `OneShot`).

use std::sync::Arc;

use super::memory::{Memories, MemoryRequirement};
use crate::entity::LivingEntity;
use crate::world::World;

/// Whether a behavior is running (vanilla `Behavior.Status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BehaviorStatus {
    Stopped,
    Running,
}

/// A brain behavior (vanilla `BehaviorControl`).
pub trait BehaviorControl<E: ?Sized>: Send {
    /// Whether the behavior is running.
    fn status(&self) -> BehaviorStatus;

    /// Registers the memories the behavior reads or writes
    /// (vanilla `getRequiredMemories`).
    fn register_memories(&self, memories: &mut Memories);

    /// Starts the behavior if it can run; returns whether it started.
    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool;

    /// Ticks the running behavior, or stops it.
    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64);

    /// Stops the behavior.
    fn do_stop(&mut self, world: &Arc<World>, body: &E, time: i64);
}

/// The hooks of a timed behavior (vanilla `Behavior` subclasses).
pub trait Behavior<E: ?Sized>: Send {
    /// Memories that must have a status for the behavior to start
    /// (vanilla `entryCondition`).
    fn entry_condition(&self) -> &[MemoryRequirement];

    /// Shortest and longest run time in ticks; vanilla defaults to 60.
    fn duration(&self) -> (i32, i32) {
        (60, 60)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, _body: &E) -> bool {
        true
    }

    fn start(&mut self, _world: &Arc<World>, _body: &E, _time: i64) {}

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &E, _time: i64) -> bool {
        false
    }

    fn tick(&mut self, _world: &Arc<World>, _body: &E, _time: i64) {}

    fn stop(&mut self, _world: &Arc<World>, _body: &E, _time: i64) {}

    /// Vanilla `stop` for behaviors that read `timedOut(timestamp)` there;
    /// `end_time` is the run's vanilla `endTimestamp`.
    fn stop_with_end_time(&mut self, world: &Arc<World>, body: &E, time: i64, _end_time: i64) {
        self.stop(world, body, time);
    }

    /// Vanilla `Behavior.timedOut`.
    fn timed_out(&self, time: i64, end_time: i64) -> bool {
        time > end_time
    }
}

/// Runs a [`Behavior`] with vanilla's status and end-time bookkeeping.
pub struct TimedBehavior<B> {
    behavior: B,
    status: BehaviorStatus,
    end_time: i64,
}

impl<B> TimedBehavior<B> {
    #[must_use]
    pub const fn new(behavior: B) -> Self {
        Self {
            behavior,
            status: BehaviorStatus::Stopped,
            end_time: 0,
        }
    }

    /// The wrapped behavior.
    #[must_use]
    pub const fn behavior(&self) -> &B {
        &self.behavior
    }
}

impl<E, B> BehaviorControl<E> for TimedBehavior<B>
where
    E: LivingEntity + ?Sized,
    B: Behavior<E>,
{
    fn status(&self) -> BehaviorStatus {
        self.status
    }

    fn register_memories(&self, memories: &mut Memories) {
        for requirement in self.behavior.entry_condition() {
            requirement.memory().register_into(memories);
        }
    }

    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        let entry_condition = self.behavior.entry_condition();
        if !body
            .brain_memories()
            .is_some_and(|memories| memories.lock().meets(entry_condition))
            || !self.behavior.check_extra_start_conditions(world, body)
        {
            return false;
        }

        self.status = BehaviorStatus::Running;
        let (min, max) = self.behavior.duration();
        self.end_time = time + i64::from(min + rand::random_range(0..=max - min));
        self.behavior.start(world, body, time);
        true
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        if !self.behavior.timed_out(time, self.end_time)
            && self.behavior.can_still_use(world, body, time)
        {
            self.behavior.tick(world, body, time);
        } else {
            self.do_stop(world, body, time);
        }
    }

    fn do_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.status = BehaviorStatus::Stopped;
        self.behavior
            .stop_with_end_time(world, body, time, self.end_time);
    }
}

/// A behavior that runs once when triggered (vanilla `OneShot`).
///
/// It reports `Running` from a successful trigger until the next tick stops it.
pub struct OneShot<F> {
    memories: Vec<MemoryRequirement>,
    trigger: F,
    status: BehaviorStatus,
}

impl<F> OneShot<F> {
    /// A one-shot behavior that registers `memories` and runs `trigger`.
    #[must_use]
    pub const fn new(memories: Vec<MemoryRequirement>, trigger: F) -> Self {
        Self {
            memories,
            trigger,
            status: BehaviorStatus::Stopped,
        }
    }
}

impl<E, F> BehaviorControl<E> for OneShot<F>
where
    E: ?Sized,
    F: FnMut(&Arc<World>, &E, i64) -> bool + Send,
{
    fn status(&self) -> BehaviorStatus {
        self.status
    }

    fn register_memories(&self, memories: &mut Memories) {
        for requirement in &self.memories {
            requirement.memory().register_into(memories);
        }
    }

    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        if (self.trigger)(world, body, time) {
            self.status = BehaviorStatus::Running;
            true
        } else {
            false
        }
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.do_stop(world, body, time);
    }

    fn do_stop(&mut self, _world: &Arc<World>, _body: &E, _time: i64) {
        self.status = BehaviorStatus::Stopped;
    }
}

/// Starts `behavior` only while `predicate` holds for the body
/// (vanilla `BehaviorBuilder.triggerIf(predicate, behavior)`).
pub struct TriggerIf<E: ?Sized, B> {
    predicate: fn(&E) -> bool,
    behavior: B,
}

/// Wraps `behavior` so it only starts while `predicate` holds
/// (vanilla `BehaviorBuilder.triggerIf(predicate, behavior)`).
#[must_use]
pub const fn trigger_if<E: ?Sized, B>(predicate: fn(&E) -> bool, behavior: B) -> TriggerIf<E, B> {
    TriggerIf {
        predicate,
        behavior,
    }
}

impl<E, B> BehaviorControl<E> for TriggerIf<E, B>
where
    E: ?Sized,
    B: BehaviorControl<E>,
{
    fn status(&self) -> BehaviorStatus {
        self.behavior.status()
    }

    fn register_memories(&self, memories: &mut Memories) {
        self.behavior.register_memories(memories);
    }

    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        (self.predicate)(body) && self.behavior.try_start(world, body, time)
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.behavior.tick_or_stop(world, body, time);
    }

    fn do_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.behavior.do_stop(world, body, time);
    }
}
