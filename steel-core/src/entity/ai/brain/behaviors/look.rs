use std::sync::Arc;

use glam::DVec3;
use steel_math::wrap_degrees;

use super::UniformInt;
use super::utils::is_visible_by;
use crate::entity::ai::brain::behavior::{Behavior, OneShot};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    GAZE_COOLDOWN_TICKS, LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::{LivingEntity, Mob};
use crate::world::World;

const LOOK_AT_TARGET_SINK_ENTRY: [MemoryRequirement; 1] = [MemoryRequirement::new(
    &LOOK_TARGET,
    MemoryStatus::ValuePresent,
)];

/// Turns the head towards `LOOK_TARGET` (vanilla `LookAtTargetSink`).
pub struct LookAtTargetSink {
    min_duration: i32,
    max_duration: i32,
}

impl LookAtTargetSink {
    #[must_use]
    pub const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
        }
    }
}

impl<E: Mob + ?Sized> Behavior<E> for LookAtTargetSink {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &LOOK_AT_TARGET_SINK_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_duration, self.max_duration)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        let Some(target) = body
            .brain_memories()
            .and_then(|memories| memories.lock().get(&LOOK_TARGET))
        else {
            return false;
        };
        is_visible_by(&target, body)
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            memories.lock().erase(&LOOK_TARGET);
        }
    }

    fn tick(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        let Some(target) = body
            .brain_memories()
            .and_then(|memories| memories.lock().get(&LOOK_TARGET))
        else {
            return;
        };
        // Vanilla `LookControl.setLookAt(Vec3)` uses the mob's head rotation
        // speed and maximum head pitch.
        let position = target.current_position();
        let head_rot_speed = body.head_rot_speed();
        body.mob_base().controls().lock().look_control.set_look_at(
            position,
            head_rot_speed,
            body.max_head_x_rot(),
        );
    }
}

const RANDOM_LOOK_AROUND_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&GAZE_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
];

/// Looks at a random nearby point, then waits out a gaze cooldown
/// (vanilla `RandomLookAround`).
pub struct RandomLookAround {
    interval: UniformInt,
    max_yaw: f32,
    min_pitch: f32,
    pitch_range: f32,
}

impl RandomLookAround {
    /// `min_pitch` must not exceed `max_pitch`; vanilla throws otherwise, so
    /// they are swapped here.
    #[must_use]
    pub const fn new(interval: UniformInt, max_yaw: f32, min_pitch: f32, max_pitch: f32) -> Self {
        let (min_pitch, max_pitch) = if min_pitch > max_pitch {
            (max_pitch, min_pitch)
        } else {
            (min_pitch, max_pitch)
        };
        Self {
            interval,
            max_yaw,
            min_pitch,
            pitch_range: max_pitch - min_pitch,
        }
    }
}

impl<E: Mob + ?Sized> Behavior<E> for RandomLookAround {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &RANDOM_LOOK_AROUND_ENTRY
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        let pitch = (rand::random::<f32>() * self.pitch_range + self.min_pitch).clamp(-90.0, 90.0);
        let (y_rot, _) = body.rotation();
        let rotation =
            wrap_degrees(y_rot + 2.0 * rand::random::<f32>() * self.max_yaw - self.max_yaw);
        let look = body.calculate_view_vector(pitch, rotation);
        let eye = body.position() + DVec3::Y * body.get_eye_height();
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let mut memories = memories.lock();
        memories.set(&LOOK_TARGET, Some(PositionTracker::point(eye + look)));
        memories.set(&GAZE_COOLDOWN_TICKS, Some(self.interval.sample()));
    }
}

/// Vanilla `SetEntityLookTargetSometimes.Ticker`.
struct Ticker {
    interval: UniformInt,
    ticks_until_next_start: i32,
}

impl Ticker {
    fn tick_down_and_check(&mut self) -> bool {
        if self.ticks_until_next_start == 0 {
            self.ticks_until_next_start = self.interval.sample() - 1;
            false
        } else {
            self.ticks_until_next_start -= 1;
            self.ticks_until_next_start == 0
        }
    }
}

/// Every so often looks at the closest visible entity matching `predicate`
/// within `max_dist` (vanilla `SetEntityLookTargetSometimes`).
///
/// `interval.min_inclusive` must be above 1, as vanilla requires.
pub fn set_entity_look_target_sometimes<E: LivingEntity + ?Sized>(
    max_dist: f32,
    interval: UniformInt,
    predicate: impl Fn(&dyn LivingEntity) -> bool + Send + 'static,
) -> impl BehaviorControl<E> {
    let max_dist_sqr = f64::from(max_dist * max_dist);
    let mut ticker = Ticker {
        interval,
        ticks_until_next_start: 0,
    };
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let visible = {
                let memories = memories.lock();
                if memories.has_value(&LOOK_TARGET) {
                    return false;
                }
                let Some(visible) = memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES) else {
                    return false;
                };
                visible
            };
            let position = body.position();
            let Some(target) = visible.find_closest(|entity| {
                entity.as_living_entity().is_some_and(&predicate)
                    && entity.position().distance_squared(position) <= max_dist_sqr
            }) else {
                return false;
            };
            if !ticker.tick_down_and_check() {
                return false;
            }
            memories
                .lock()
                .set(&LOOK_TARGET, Some(PositionTracker::entity(target, true)));
            true
        },
    )
}

/// Looks at the closest visible entity matching `predicate` within
/// `max_dist` (vanilla `SetEntityLookTarget.create(predicate, maxDist)`).
pub fn set_entity_look_target<E: LivingEntity + ?Sized>(
    max_dist: f32,
    predicate: impl Fn(&dyn LivingEntity) -> bool + Send + 'static,
) -> impl BehaviorControl<E> {
    let max_dist_sqr = f64::from(max_dist * max_dist);
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let Some(visible) = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES) else {
                return false;
            };
            let position = body.position();
            let Some(target) = visible.find_closest(|entity| {
                entity.as_living_entity().is_some_and(&predicate)
                    && entity.position().distance_squared(position) <= max_dist_sqr
                    && !body.has_passenger(entity.as_ref())
            }) else {
                return false;
            };
            memories
                .lock()
                .set(&LOOK_TARGET, Some(PositionTracker::entity(target, true)));
            true
        },
    )
}
