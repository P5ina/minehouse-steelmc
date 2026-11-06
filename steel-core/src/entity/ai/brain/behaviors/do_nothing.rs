use std::sync::Arc;

use crate::entity::ai::brain::{BehaviorControl, BehaviorStatus, Memories};
use crate::world::World;

/// Stands idle for a random time (vanilla `DoNothing`).
pub struct DoNothing {
    min_duration: i32,
    max_duration: i32,
    status: BehaviorStatus,
    end_time: i64,
}

impl DoNothing {
    #[must_use]
    pub const fn new(min_duration: i32, max_duration: i32) -> Self {
        Self {
            min_duration,
            max_duration,
            status: BehaviorStatus::Stopped,
            end_time: 0,
        }
    }
}

impl<E: ?Sized> BehaviorControl<E> for DoNothing {
    fn status(&self) -> BehaviorStatus {
        self.status
    }

    fn register_memories(&self, _memories: &mut Memories) {}

    fn try_start(&mut self, _world: &Arc<World>, _body: &E, time: i64) -> bool {
        self.status = BehaviorStatus::Running;
        let duration = rand::random_range(self.min_duration..=self.max_duration);
        self.end_time = time + i64::from(duration);
        true
    }

    fn tick_or_stop(&mut self, world: &Arc<World>, body: &E, time: i64) {
        if time > self.end_time {
            self.do_stop(world, body, time);
        }
    }

    fn do_stop(&mut self, _world: &Arc<World>, _body: &E, _time: i64) {
        self.status = BehaviorStatus::Stopped;
    }
}
