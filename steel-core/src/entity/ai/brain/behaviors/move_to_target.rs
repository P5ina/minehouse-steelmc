use std::f64::consts::FRAC_PI_2;
use std::sync::Arc;

use glam::DVec3;
use steel_utils::BlockPos;

use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    CANT_REACH_WALK_TARGET_SINCE, PATH, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::goal::random_pos::default_random_pos_towards;
use crate::entity::ai::path::Path;
use crate::entity::{LivingEntity, PathfinderMob};
use crate::world::World;

const MAX_COOLDOWN_BEFORE_RETRYING: i32 = 40;

const MOVE_TO_TARGET_ENTRY: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
    MemoryRequirement::new(&PATH, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValuePresent),
];

/// Paths to `WALK_TARGET` and follows it (vanilla `MoveToTargetSink`).
pub struct MoveToTargetSink {
    min_timeout: i32,
    max_timeout: i32,
    remaining_cooldown: i32,
    path: Option<Path>,
    last_target_pos: Option<BlockPos>,
    speed_modifier: f32,
}

impl MoveToTargetSink {
    #[must_use]
    pub const fn new() -> Self {
        Self::with_timeout(150, 250)
    }

    #[must_use]
    pub const fn with_timeout(min_timeout: i32, max_timeout: i32) -> Self {
        Self {
            min_timeout,
            max_timeout,
            remaining_cooldown: 0,
            path: None,
            last_target_pos: None,
            speed_modifier: 0.0,
        }
    }

    fn walk_target(body: &(impl LivingEntity + ?Sized)) -> Option<WalkTarget> {
        body.brain_memories()?.lock().get(&WALK_TARGET)
    }

    fn reached_target(body: &(impl LivingEntity + ?Sized), walk_target: &WalkTarget) -> bool {
        let target = walk_target.target.current_block_position();
        let position = body.block_position();
        (target.x() - position.x()).abs()
            + (target.y() - position.y()).abs()
            + (target.z() - position.z()).abs()
            <= walk_target.close_enough_dist
    }

    fn try_compute_path(
        &mut self,
        body: &(impl PathfinderMob + ?Sized),
        walk_target: &WalkTarget,
        time: i64,
    ) -> bool {
        let target_pos = walk_target.target.current_block_position();
        self.path = body.create_path_to(target_pos, 0);
        self.speed_modifier = walk_target.speed_modifier;
        let Some(memories) = body.brain_memories() else {
            return false;
        };
        if Self::reached_target(body, walk_target) {
            memories.lock().erase(&CANT_REACH_WALK_TARGET_SINCE);
            return false;
        }

        let can_reach = self.path.as_ref().is_some_and(Path::can_reach);
        {
            let mut memories = memories.lock();
            if can_reach {
                memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
            } else if !memories.has_value(&CANT_REACH_WALK_TARGET_SINCE) {
                memories.set(&CANT_REACH_WALK_TARGET_SINCE, Some(time));
            }
        }
        if self.path.is_some() {
            return true;
        }

        let target = DVec3::new(
            f64::from(target_pos.x()) + 0.5,
            f64::from(target_pos.y()),
            f64::from(target_pos.z()) + 0.5,
        );
        let Some(partial_step) = default_random_pos_towards(body, 10, 7, target, FRAC_PI_2) else {
            return false;
        };
        self.path = body.create_path_to(
            BlockPos::containing(partial_step.x, partial_step.y, partial_step.z),
            0,
        );
        self.path.is_some()
    }
}

impl Default for MoveToTargetSink {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: PathfinderMob + ?Sized> Behavior<E> for MoveToTargetSink {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &MOVE_TO_TARGET_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (self.min_timeout, self.max_timeout)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &E) -> bool {
        if self.remaining_cooldown > 0 {
            self.remaining_cooldown -= 1;
            return false;
        }
        let Some(walk_target) = Self::walk_target(body) else {
            return false;
        };
        let reached_target = Self::reached_target(body, &walk_target);
        if !reached_target && self.try_compute_path(body, &walk_target, world.game_time()) {
            self.last_target_pos = Some(walk_target.target.current_block_position());
            return true;
        }

        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.erase(&WALK_TARGET);
            if reached_target {
                memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
            }
        }
        false
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        if self.path.is_none() || self.last_target_pos.is_none() {
            return false;
        }
        let Some(walk_target) = Self::walk_target(body) else {
            return false;
        };
        let is_spectator = walk_target
            .target
            .tracked_entity()
            .is_some_and(|entity| entity.is_spectator());
        !body.mob_base().navigation().lock().is_done()
            && !Self::reached_target(body, &walk_target)
            && !is_spectator
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(walk_target) = Self::walk_target(body)
            && !Self::reached_target(body, &walk_target)
            && body.mob_base().navigation().lock().is_stuck()
        {
            self.remaining_cooldown = rand::random_range(0..MAX_COOLDOWN_BEFORE_RETRYING);
        }
        body.mob_base().navigation().lock().stop();
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.erase(&WALK_TARGET);
            memories.erase(&PATH);
        }
        self.path = None;
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            memories.lock().set(&PATH, self.path.clone());
        }
        body.move_to_path(self.path.clone(), f64::from(self.speed_modifier));
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        let new_path = body.mob_base().navigation().lock().path().cloned();
        if self.path != new_path {
            self.path.clone_from(&new_path);
            if let Some(memories) = body.brain_memories() {
                memories.lock().set(&PATH, new_path.clone());
            }
        }

        if new_path.is_none() {
            return;
        }
        let (Some(last_target_pos), Some(walk_target)) =
            (self.last_target_pos, Self::walk_target(body))
        else {
            return;
        };
        let target = walk_target.target.current_block_position();
        let dx = f64::from(target.x() - last_target_pos.x());
        let dy = f64::from(target.y() - last_target_pos.y());
        let dz = f64::from(target.z() - last_target_pos.z());
        if dx * dx + dy * dy + dz * dz > 4.0
            && self.try_compute_path(body, &walk_target, world.game_time())
        {
            self.last_target_pos = Some(target);
            <Self as Behavior<E>>::start(self, world, body, time);
        }
    }
}
