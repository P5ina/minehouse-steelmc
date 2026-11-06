use std::sync::Arc;

use crate::entity::PathfinderMob;
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, IS_PANICKING, LOOK_TARGET, PositionTracker, TEMPTATION_COOLDOWN_TICKS,
    TEMPTING_PLAYER, WALK_TARGET, WalkTarget,
};
use crate::world::World;

/// Ticks a tempted mob waits before following again.
pub const TEMPTATION_COOLDOWN: i32 = 100;
pub const DEFAULT_CLOSE_ENOUGH_DIST: f64 = 2.5;
pub const BACKED_UP_CLOSE_ENOUGH_DIST: f64 = 3.5;

const FOLLOW_TEMPTATION_ENTRY: [MemoryRequirement; 6] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&TEMPTATION_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&TEMPTING_PLAYER, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&BREED_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::ValueAbsent),
];

type PerBody<E, T> = Box<dyn Fn(&E) -> T + Send>;

/// Follows the player holding a tempting item (vanilla `FollowTemptation`).
pub struct FollowTemptation<E: ?Sized> {
    speed_modifier: PerBody<E, f32>,
    close_enough_distance: PerBody<E, f64>,
    look_in_the_eyes: bool,
}

impl<E: PathfinderMob + ?Sized> FollowTemptation<E> {
    #[must_use]
    pub fn new(speed_modifier: impl Fn(&E) -> f32 + Send + 'static) -> Self {
        Self::with_distance(speed_modifier, |_| DEFAULT_CLOSE_ENOUGH_DIST, false)
    }

    #[must_use]
    pub fn with_distance(
        speed_modifier: impl Fn(&E) -> f32 + Send + 'static,
        close_enough_distance: impl Fn(&E) -> f64 + Send + 'static,
        look_in_the_eyes: bool,
    ) -> Self {
        Self {
            speed_modifier: Box::new(speed_modifier),
            close_enough_distance: Box::new(close_enough_distance),
            look_in_the_eyes,
        }
    }
}

impl<E: PathfinderMob + ?Sized> Behavior<E> for FollowTemptation<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &FOLLOW_TEMPTATION_ENTRY
    }

    fn timed_out(&self, _time: i64, _end_time: i64) -> bool {
        false
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        body.brain_memories().is_some_and(|memories| {
            let memories = memories.lock();
            memories.has_value(&TEMPTING_PLAYER)
                && !memories.has_value(&BREED_TARGET)
                && !memories.has_value(&IS_PANICKING)
        })
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.set(&TEMPTATION_COOLDOWN_TICKS, Some(TEMPTATION_COOLDOWN));
            memories.erase(&WALK_TARGET);
            memories.erase(&LOOK_TARGET);
        }
    }

    fn tick(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let Some(player) = memories.lock().get(&TEMPTING_PLAYER) else {
            return;
        };
        let close_enough = (self.close_enough_distance)(body);
        let speed = (self.speed_modifier)(body);
        let mut memories = memories.lock();
        memories.set(
            &LOOK_TARGET,
            Some(PositionTracker::entity(Arc::clone(&player), true)),
        );
        if body.position().distance_squared(player.position()) < close_enough * close_enough {
            memories.erase(&WALK_TARGET);
        } else {
            let target = PositionTracker::Entity {
                entity: player,
                track_eye_height: self.look_in_the_eyes,
                target_eye_height: self.look_in_the_eyes,
            };
            memories.set(&WALK_TARGET, Some(WalkTarget::new(target, speed, 2)));
        }
    }
}
