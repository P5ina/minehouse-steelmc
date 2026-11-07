use std::sync::Arc;

use crate::entity::LivingEntity;
use crate::entity::ai::brain::memory::{AnyMemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::{BehaviorControl, BehaviorStatus, Memories};
use crate::world::World;

/// The order a gate tries its behaviors in (vanilla `GateBehavior.OrderPolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderPolicy {
    Ordered,
    /// Weighted shuffle each start (vanilla `ShufflingList.shuffle`).
    Shuffled,
}

/// How many behaviors a gate starts (vanilla `GateBehavior.RunningPolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningPolicy {
    RunOne,
    TryAll,
}

struct WeightedEntry<E: ?Sized> {
    behavior: Box<dyn BehaviorControl<E>>,
    weight: i32,
    rand_weight: f64,
}

/// Runs child behaviors as one (vanilla `GateBehavior`).
pub struct GateBehavior<E: ?Sized> {
    entry_condition: Vec<MemoryRequirement>,
    exit_erased_memories: Vec<&'static dyn AnyMemoryModuleType>,
    order_policy: OrderPolicy,
    running_policy: RunningPolicy,
    behaviors: Vec<WeightedEntry<E>>,
    status: BehaviorStatus,
}

impl<E: ?Sized> GateBehavior<E> {
    #[must_use]
    pub fn new(
        entry_condition: Vec<MemoryRequirement>,
        exit_erased_memories: Vec<&'static dyn AnyMemoryModuleType>,
        order_policy: OrderPolicy,
        running_policy: RunningPolicy,
        behaviors: Vec<(Box<dyn BehaviorControl<E>>, i32)>,
    ) -> Self {
        Self {
            entry_condition,
            exit_erased_memories,
            order_policy,
            running_policy,
            behaviors: behaviors
                .into_iter()
                .map(|(behavior, weight)| WeightedEntry {
                    behavior,
                    weight,
                    rand_weight: 0.0,
                })
                .collect(),
            status: BehaviorStatus::Stopped,
        }
    }

    fn shuffle(&mut self) {
        for entry in &mut self.behaviors {
            entry.rand_weight =
                -f64::from(rand::random::<f32>()).powf(1.0 / f64::from(entry.weight));
        }
        self.behaviors
            .sort_by(|a, b| a.rand_weight.total_cmp(&b.rand_weight));
    }
}

/// Starts one of `behaviors`, picked by weighted shuffle (vanilla `RunOne`).
#[must_use]
pub fn run_one<E: ?Sized>(
    entry_condition: Vec<MemoryRequirement>,
    behaviors: Vec<(Box<dyn BehaviorControl<E>>, i32)>,
) -> GateBehavior<E> {
    GateBehavior::new(
        entry_condition,
        Vec::new(),
        OrderPolicy::Shuffled,
        RunningPolicy::RunOne,
        behaviors,
    )
}

/// Fires one of `behaviors`, picked by weighted shuffle, and is done the
/// same tick (vanilla `TriggerGate.triggerOneShuffled`): one-shot children
/// run for one tick, like vanilla's triggers.
#[must_use]
pub fn trigger_one_shuffled<E: ?Sized>(
    behaviors: Vec<(Box<dyn BehaviorControl<E>>, i32)>,
) -> GateBehavior<E> {
    run_one(Vec::new(), behaviors)
}

impl<E: LivingEntity + ?Sized> BehaviorControl<E> for GateBehavior<E> {
    fn status(&self) -> BehaviorStatus {
        self.status
    }

    fn register_memories(&self, memories: &mut Memories) {
        for requirement in &self.entry_condition {
            requirement.memory().register_into(memories);
        }
        for entry in &self.behaviors {
            entry.behavior.register_memories(memories);
        }
    }

    fn try_start(&mut self, world: &Arc<World>, body: &E, time: i64) -> bool {
        if !body
            .brain_memories()
            .is_some_and(|memories| memories.lock().meets(&self.entry_condition))
        {
            return false;
        }

        self.status = BehaviorStatus::Running;
        if self.order_policy == OrderPolicy::Shuffled {
            self.shuffle();
        }
        for entry in &mut self.behaviors {
            if entry.behavior.status() == BehaviorStatus::Stopped
                && entry.behavior.try_start(world, body, time)
                && self.running_policy == RunningPolicy::RunOne
            {
                break;
            }
        }
        true
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        for entry in &mut self.behaviors {
            if entry.behavior.status() == BehaviorStatus::Running {
                entry.behavior.tick_or_stop(world, body, time);
            }
        }
        if self
            .behaviors
            .iter()
            .all(|entry| entry.behavior.status() != BehaviorStatus::Running)
        {
            self.do_stop(world, body, time);
        }
    }

    fn do_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.status = BehaviorStatus::Stopped;
        for entry in &mut self.behaviors {
            if entry.behavior.status() == BehaviorStatus::Running {
                entry.behavior.do_stop(world, body, time);
            }
        }
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            for memory in &self.exit_erased_memories {
                memories.erase(*memory);
            }
        }
    }
}
