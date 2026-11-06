//! Vanilla `Brain`: memory-driven AI used by mobs such as goats, frogs,
//! villagers and piglins instead of goal selectors.
//!
//! Memories live behind their own lock so sensors and behaviors can read
//! and write them while the brain ticks its behavior list.
//!
//! Not yet ported: schedules (`Brain.setSchedule` and
//! `updateActivityFromSchedule`), which villagers need.

pub mod activity;
pub mod behavior;
pub mod behaviors;
pub mod memory;
pub mod memory_types;
pub mod nearest_visible;
pub mod sensor;
pub mod sensors;

use std::collections::BTreeMap;
use std::sync::Arc;

use simdnbt::borrow::NbtCompound as BorrowedNbtCompound;
use simdnbt::owned::NbtCompound;
use steel_utils::locks::SyncMutex;

pub use activity::Activity;
pub use behavior::{BehaviorControl, BehaviorStatus};
pub use memory::{AnyMemoryModuleType, Memories, MemoryRequirement, MemoryStatus};
pub use sensor::Sensor;
use sensor::SensorSlot;

use crate::world::World;

/// Behaviors of one activity with their priorities (vanilla `ActivityData`).
pub struct ActivityData<E: ?Sized> {
    activity: Activity,
    behaviors: Vec<(i32, Box<dyn BehaviorControl<E>>)>,
    conditions: Vec<MemoryRequirement>,
    memories_to_erase_when_stopped: Vec<&'static dyn AnyMemoryModuleType>,
}

impl<E: ?Sized> ActivityData<E> {
    /// Behaviors with explicit priorities and no start conditions.
    #[must_use]
    pub fn new(activity: Activity, behaviors: Vec<(i32, Box<dyn BehaviorControl<E>>)>) -> Self {
        Self {
            activity,
            behaviors,
            conditions: Vec::new(),
            memories_to_erase_when_stopped: Vec::new(),
        }
    }

    /// Behaviors with priorities counting up from `first_priority`.
    #[must_use]
    pub fn with_first_priority(
        activity: Activity,
        first_priority: i32,
        behaviors: Vec<Box<dyn BehaviorControl<E>>>,
    ) -> Self {
        Self::new(activity, Self::priority_pairs(first_priority, behaviors))
    }

    /// Vanilla `ActivityData.createPriorityPairs`.
    #[must_use]
    pub fn priority_pairs(
        first_priority: i32,
        behaviors: Vec<Box<dyn BehaviorControl<E>>>,
    ) -> Vec<(i32, Box<dyn BehaviorControl<E>>)> {
        (first_priority..).zip(behaviors).collect()
    }

    /// Requires `conditions` for the activity to become active.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<MemoryRequirement>) -> Self {
        self.conditions = conditions;
        self
    }

    /// Erases `memories` when the activity stops.
    #[must_use]
    pub fn erasing_when_stopped(mut self, memories: Vec<&'static dyn AnyMemoryModuleType>) -> Self {
        self.memories_to_erase_when_stopped = memories;
        self
    }

    /// The activity runs while `memory` has a value, and erases it when it stops.
    #[must_use]
    pub fn while_memory_present(self, memory: &'static dyn AnyMemoryModuleType) -> Self {
        self.with_conditions(vec![MemoryRequirement::new(
            memory,
            MemoryStatus::ValuePresent,
        )])
        .erasing_when_stopped(vec![memory])
    }
}

struct ActivityState {
    requirements: BTreeMap<Activity, Vec<MemoryRequirement>>,
    memories_to_erase_when_stopped: BTreeMap<Activity, Vec<&'static dyn AnyMemoryModuleType>>,
    core_activities: Vec<Activity>,
    active_activities: Vec<Activity>,
    default_activity: Activity,
}

impl ActivityState {
    fn new() -> Self {
        Self {
            requirements: BTreeMap::new(),
            memories_to_erase_when_stopped: BTreeMap::new(),
            core_activities: vec![Activity::CORE],
            active_activities: Vec::new(),
            default_activity: Activity::IDLE,
        }
    }

    fn requirements_met(&self, activity: Activity, memories: &Memories) -> bool {
        self.requirements
            .get(&activity)
            .is_some_and(|requirements| memories.meets(requirements))
    }

    fn set_active(&mut self, activity: Activity, memories: &mut Memories) {
        if self.active_activities.contains(&activity) {
            return;
        }

        for old in &self.active_activities {
            if *old == activity {
                continue;
            }
            if let Some(erase) = self.memories_to_erase_when_stopped.get(old) {
                for memory in erase {
                    memories.erase(*memory);
                }
            }
        }
        self.active_activities.clear();
        self.active_activities.extend(&self.core_activities);
        if !self.active_activities.contains(&activity) {
            self.active_activities.push(activity);
        }
    }
}

type ActivityBehaviors<E> = (Activity, Vec<Box<dyn BehaviorControl<E>>>);

struct PriorityGroup<E: ?Sized> {
    priority: i32,
    activities: Vec<ActivityBehaviors<E>>,
}

/// A mob's brain (vanilla `Brain`).
pub struct Brain<E: ?Sized> {
    memories: SyncMutex<Memories>,
    activities: SyncMutex<ActivityState>,
    sensors: SyncMutex<Vec<SensorSlot<E>>>,
    behaviors: SyncMutex<Vec<PriorityGroup<E>>>,
}

impl<E: ?Sized> Brain<E> {
    /// An empty brain (vanilla `new Brain()`), used by mobs without one.
    #[must_use]
    pub fn empty() -> Self {
        let brain = Self {
            memories: SyncMutex::new(Memories::default()),
            activities: SyncMutex::new(ActivityState::new()),
            sensors: SyncMutex::new(Vec::new()),
            behaviors: SyncMutex::new(Vec::new()),
        };
        brain.use_default_activity();
        brain
    }

    /// Builds a brain (vanilla `Brain.Provider.makeBrain`): registers the
    /// memories of `memory_types`, the sensors and the behaviors, then
    /// restores `packed` memories if any.
    #[must_use]
    pub fn new(
        memory_types: &[&'static dyn AnyMemoryModuleType],
        sensors: Vec<Box<dyn Sensor<E>>>,
        activities: Vec<ActivityData<E>>,
        packed: Option<BorrowedNbtCompound<'_, '_>>,
    ) -> Self {
        let mut memories = Memories::default();
        for memory in memory_types {
            memory.register_into(&mut memories);
        }

        let sensors = sensors
            .into_iter()
            .map(|sensor| {
                sensor.register_memories(&mut memories);
                SensorSlot::new(sensor)
            })
            .collect();

        let mut state = ActivityState::new();
        let mut groups: Vec<PriorityGroup<E>> = Vec::new();
        for data in activities {
            state.requirements.insert(data.activity, data.conditions);
            if !data.memories_to_erase_when_stopped.is_empty() {
                state
                    .memories_to_erase_when_stopped
                    .insert(data.activity, data.memories_to_erase_when_stopped);
            }
            for (priority, behavior) in data.behaviors {
                behavior.register_memories(&mut memories);
                let index = match groups.binary_search_by_key(&priority, |group| group.priority) {
                    Ok(index) => index,
                    Err(index) => {
                        groups.insert(
                            index,
                            PriorityGroup {
                                priority,
                                activities: Vec::new(),
                            },
                        );
                        index
                    }
                };
                let group = &mut groups[index];
                match group
                    .activities
                    .iter_mut()
                    .find(|(activity, _)| *activity == data.activity)
                {
                    Some((_, behaviors)) => behaviors.push(behavior),
                    None => group.activities.push((data.activity, vec![behavior])),
                }
            }
        }

        if let Some(packed) = packed {
            memories.load(packed);
        }

        let brain = Self {
            memories: SyncMutex::new(memories),
            activities: SyncMutex::new(state),
            sensors: SyncMutex::new(sensors),
            behaviors: SyncMutex::new(groups),
        };
        brain.use_default_activity();
        brain
    }

    /// The brain's memories.
    #[must_use]
    pub const fn memories(&self) -> &SyncMutex<Memories> {
        &self.memories
    }

    /// Writes the saved memories (vanilla `Brain.pack`).
    #[must_use]
    pub fn save(&self) -> NbtCompound {
        self.memories.lock().save()
    }

    /// Clears every memory.
    pub fn clear_memories(&self) {
        self.memories.lock().clear();
    }

    /// Whether the brain has no memories, sensors or behaviors.
    #[must_use]
    pub fn is_brain_dead(&self) -> bool {
        self.memories.lock().is_empty()
            && self.sensors.lock().is_empty()
            && self.behaviors.lock().is_empty()
    }

    /// Drops every behavior (vanilla `Brain.removeAllBehaviors`).
    pub fn remove_all_behaviors(&self) {
        self.behaviors.lock().clear();
    }

    /// Replaces the activities that are always active.
    pub fn set_core_activities(&self, activities: Vec<Activity>) {
        self.activities.lock().core_activities = activities;
    }

    /// Sets the activity [`Self::use_default_activity`] switches to.
    pub fn set_default_activity(&self, activity: Activity) {
        self.activities.lock().default_activity = activity;
    }

    /// Switches to the default activity.
    pub fn use_default_activity(&self) {
        let mut state = self.activities.lock();
        let activity = state.default_activity;
        state.set_active(activity, &mut self.memories.lock());
    }

    /// Whether `activity` is active.
    #[must_use]
    pub fn is_active(&self, activity: Activity) -> bool {
        self.activities.lock().active_activities.contains(&activity)
    }

    /// The first active activity that is not a core activity.
    #[must_use]
    pub fn active_non_core_activity(&self) -> Option<Activity> {
        let state = self.activities.lock();
        state
            .active_activities
            .iter()
            .copied()
            .find(|activity| !state.core_activities.contains(activity))
    }

    /// Switches to `activity` if its requirements hold, otherwise to the default.
    pub fn set_active_activity_if_possible(&self, activity: Activity) {
        let mut state = self.activities.lock();
        let mut memories = self.memories.lock();
        let activity = if state.requirements_met(activity, &memories) {
            activity
        } else {
            state.default_activity
        };
        state.set_active(activity, &mut memories);
    }

    /// Switches to the first of `activities` whose requirements hold.
    pub fn set_active_activity_to_first_valid(&self, activities: &[Activity]) {
        let mut state = self.activities.lock();
        let mut memories = self.memories.lock();
        if let Some(activity) = activities
            .iter()
            .copied()
            .find(|activity| state.requirements_met(*activity, &memories))
        {
            state.set_active(activity, &mut memories);
        }
    }

    fn active_activities(&self) -> Vec<Activity> {
        self.activities.lock().active_activities.clone()
    }

    /// Vanilla `Brain.tick`: forgets expired memories, ticks sensors, starts
    /// stopped behaviors of active activities and ticks running ones.
    pub fn tick(&self, world: &Arc<World>, body: &E) {
        self.memories.lock().forget_outdated();
        for sensor in self.sensors.lock().iter_mut() {
            sensor.tick(world, body);
        }

        let time = world.game_time();
        let mut groups = self.behaviors.lock();
        for group in groups.iter_mut() {
            for (activity, behaviors) in &mut group.activities {
                // Vanilla reads the live set, so an activity switched on by an
                // earlier behavior this tick starts its behaviors too.
                if !self.active_activities().contains(activity) {
                    continue;
                }
                for behavior in behaviors.iter_mut() {
                    if behavior.status() == BehaviorStatus::Stopped {
                        behavior.try_start(world, body, time);
                    }
                }
            }
        }

        for group in groups.iter_mut() {
            for (_, behaviors) in &mut group.activities {
                for behavior in behaviors.iter_mut() {
                    if behavior.status() == BehaviorStatus::Running {
                        behavior.tick_or_stop(world, body, time);
                    }
                }
            }
        }
    }

    /// Stops every running behavior (vanilla `Brain.stopAll`).
    pub fn stop_all(&self, world: &Arc<World>, body: &E) {
        let time = world.game_time();
        for group in self.behaviors.lock().iter_mut() {
            for (_, behaviors) in &mut group.activities {
                for behavior in behaviors.iter_mut() {
                    if behavior.status() == BehaviorStatus::Running {
                        behavior.do_stop(world, body, time);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
