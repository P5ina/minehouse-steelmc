use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_utils::BlockPos;

use super::UniformInt;
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    LOOK_TARGET, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::goal::random_pos::{is_restricted, land_random_pos, mob_restricted};
use crate::entity::ai::goal::random_swimmable_pos_within;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

const MAX_XZ_DIST: i32 = 10;
const MAX_Y_DIST: i32 = 7;
/// Vanilla `RandomStroll.SWIM_XY_DISTANCE_TIERS`.
const SWIM_XY_DISTANCE_TIERS: [(i32, i32); 6] = [(1, 1), (3, 3), (5, 5), (6, 5), (7, 7), (10, 7)];

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

/// Swims to a random water position, reaching further out tier by tier
/// while each step stays in water (vanilla `RandomStroll.swim`).
pub fn random_swim<E: PathfinderMob + ?Sized>(speed_modifier: f32) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&WALK_TARGET) || !body.is_in_water() {
                return false;
            }
            let target = swim_target_pos(body)
                .map(|position| WalkTarget::to_point(position, speed_modifier, 0));
            memories.lock().set(&WALK_TARGET, target);
            true
        },
    )
}

/// Vanilla `RandomStroll.getTargetSwimPos`.
fn swim_target_pos<E: PathfinderMob + ?Sized>(body: &E) -> Option<DVec3> {
    let world = body.level()?;
    let mut fallback: Option<DVec3> = None;
    let mut target = None;
    for (horizontal, vertical) in SWIM_XY_DISTANCE_TIERS {
        target = match fallback {
            None => random_swimmable_pos_within(body, horizontal, vertical),
            Some(fallback) => {
                let direction = (fallback - body.position()).normalize();
                Some(
                    body.position()
                        + direction
                            * DVec3::new(
                                f64::from(horizontal),
                                f64::from(vertical),
                                f64::from(horizontal),
                            ),
                )
            }
        };
        let restrict = mob_restricted(body, f64::from(horizontal));
        let Some(position) = target else {
            return fallback;
        };
        let block = BlockPos::containing(position.x, position.y, position.z);
        if world.get_block_state(block).get_fluid_state().is_empty()
            || is_restricted(restrict, body, block)
        {
            return fallback;
        }
        fallback = Some(position);
    }
    target
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
