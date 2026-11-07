use std::sync::Arc;

use glam::DVec3;
use steel_utils::BlockPos;

use super::UniformInt;
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement, MemoryValue};
use crate::entity::ai::brain::memory_types::{
    LOOK_TARGET, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::goal::random_pos::{land_random_pos, land_random_pos_away};
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

const MAX_XZ_DIST: i32 = 10;
const MAX_Y_DIST: i32 = 7;
/// Vanilla `SetWalkTargetAwayFrom` flee distance and attempts.
const FLEE_XZ_DIST: i32 = 16;
const FLEE_ATTEMPTS: usize = 10;

/// Walks to a random land position within 10 blocks; with
/// `may_stroll_from_water` false it only starts out of water
/// (vanilla `RandomStroll.stroll`).
pub fn random_stroll<E: PathfinderMob + ?Sized>(
    speed_modifier: f32,
    may_stroll_from_water: bool,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::Registered,
        )],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&WALK_TARGET)
                || !may_stroll_from_water && body.is_in_water()
            {
                return false;
            }
            let target = land_random_pos(body, MAX_XZ_DIST, MAX_Y_DIST)
                .map(|position| WalkTarget::to_point(position, speed_modifier, 0));
            memories.lock().set(&WALK_TARGET, target);
            true
        },
    )
}

/// Walks to whatever the body looks at (vanilla `SetWalkTargetFromLookTarget`).
pub fn set_walk_target_from_look_target<E: LivingEntity + ?Sized>(
    can_set_walk_target: impl Fn(&E) -> bool + Send + 'static,
    speed_modifier: impl Fn(&E) -> f32 + Send + 'static,
    close_enough_distance: i32,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let look_target = {
                let memories = memories.lock();
                if memories.has_value(&WALK_TARGET) {
                    return false;
                }
                let Some(look_target) = memories.get(&LOOK_TARGET) else {
                    return false;
                };
                look_target
            };
            if !can_set_walk_target(body) {
                return false;
            }
            let walk_target =
                WalkTarget::new(look_target, speed_modifier(body), close_enough_distance);
            memories.lock().set(&WALK_TARGET, Some(walk_target));
            true
        },
    )
}

/// A baby walks to the nearest visible adult in `nearest_visible` when it is
/// within `follow_range` (vanilla `BabyFollowAdult`).
pub fn baby_follow_adult<E: LivingEntity + ?Sized>(
    follow_range: UniformInt,
    speed_modifier: impl Fn(&E) -> f32 + Send + 'static,
    nearest_visible: &'static MemoryModuleType<SharedEntity>,
    target_eye: bool,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(nearest_visible, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let mut memories = memories.lock();
            if memories.has_value(&WALK_TARGET) {
                return false;
            }
            let Some(adult) = memories.get(nearest_visible) else {
                return false;
            };
            if !body.is_baby() {
                return false;
            }
            let distance_sqr = body.position().distance_squared(adult.position());
            let max = f64::from(follow_range.max_inclusive + 1);
            let min = f64::from(follow_range.min_inclusive);
            if distance_sqr >= max * max || distance_sqr < min * min {
                return false;
            }
            let walk_target = WalkTarget::new(
                PositionTracker::Entity {
                    entity: Arc::clone(&adult),
                    track_eye_height: target_eye,
                    target_eye_height: target_eye,
                },
                speed_modifier(body),
                follow_range.min_inclusive - 1,
            );
            memories.set(
                &LOOK_TARGET,
                Some(PositionTracker::Entity {
                    entity: adult,
                    track_eye_height: true,
                    target_eye_height: target_eye,
                }),
            );
            memories.set(&WALK_TARGET, Some(walk_target));
            true
        },
    )
}

/// Vanilla `SetWalkTargetAwayFrom.pos`: walk away from the block in `memory`
/// while it is closer than `desired_distance`.
pub fn set_walk_target_away_from_pos<E: PathfinderMob + ?Sized>(
    memory: &'static MemoryModuleType<BlockPos>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
) -> impl BehaviorControl<E> {
    set_walk_target_away_from(
        memory,
        speed_modifier,
        desired_distance,
        interrupt_current_walk,
        |pos: &BlockPos| {
            let (x, y, z) = pos.get_bottom_center();
            DVec3::new(x, y, z)
        },
    )
}

/// Vanilla `SetWalkTargetAwayFrom.entity`: walk away from the entity in
/// `memory` while it is closer than `desired_distance`.
pub fn set_walk_target_away_from_entity<E: PathfinderMob + ?Sized>(
    memory: &'static MemoryModuleType<SharedEntity>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
) -> impl BehaviorControl<E> {
    set_walk_target_away_from(
        memory,
        speed_modifier,
        desired_distance,
        interrupt_current_walk,
        |entity: &SharedEntity| entity.position(),
    )
}

fn set_walk_target_away_from<E, T>(
    memory: &'static MemoryModuleType<T>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
    to_position: impl Fn(&T) -> DVec3 + Send + 'static,
) -> impl BehaviorControl<E>
where
    E: PathfinderMob + ?Sized,
    T: MemoryValue,
{
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(memory, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let (target, avoid_position) = {
                let memories = memories.lock();
                let Some(avoid_position) = memories.get_ref(memory).map(&to_position) else {
                    return false;
                };
                (memories.get(&WALK_TARGET), avoid_position)
            };
            if target.is_some() && !interrupt_current_walk {
                return false;
            }
            let body_position = body.position();
            let desired_distance = f64::from(desired_distance);
            if body_position.distance_squared(avoid_position) >= desired_distance * desired_distance
            {
                return false;
            }
            // Vanilla compares the stored speed modifier exactly.
            if let Some(target) = target
                && target.speed_modifier.to_bits() == speed_modifier.to_bits()
            {
                let current_direction = target.target.current_position() - body_position;
                let avoid_direction = avoid_position - body_position;
                if current_direction.dot(avoid_direction) < 0.0 {
                    return false;
                }
            }
            for _ in 0..FLEE_ATTEMPTS {
                if let Some(flee_to) =
                    land_random_pos_away(body, FLEE_XZ_DIST, MAX_Y_DIST, avoid_position)
                {
                    memories.lock().set(
                        &WALK_TARGET,
                        Some(WalkTarget::to_point(flee_to, speed_modifier, 0)),
                    );
                    break;
                }
            }
            true
        },
    )
}
