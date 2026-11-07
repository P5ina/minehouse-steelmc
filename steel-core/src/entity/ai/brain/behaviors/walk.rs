use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_blocks;
use steel_utils::{BlockPos, BlockStateId, Identifier};

use super::UniformInt;
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement, MemoryValue};
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, LOOK_TARGET, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::goal::random_pos::{
    air_and_water_random_pos, is_restricted, land_random_pos, land_random_pos_away, mob_restricted,
};
use crate::entity::ai::goal::random_swimmable_pos_within;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

const MAX_XZ_DIST: i32 = 10;
const MAX_Y_DIST: i32 = 7;
/// Vanilla `SetWalkTargetAwayFrom` flee distance and attempts.
const FLEE_XZ_DIST: i32 = 16;
const FLEE_ATTEMPTS: usize = 10;
/// Vanilla `RandomStroll.SWIM_XY_DISTANCE_TIERS`.
const SWIM_XY_DISTANCE_TIERS: [(i32, i32); 6] = [(1, 1), (3, 3), (5, 5), (6, 5), (7, 7), (10, 7)];

/// Vanilla `TryFindLiquid`: the retry delay while still on cooldown.
const TRY_FIND_LIQUID_RETRY_TICKS: i64 = 22;
/// Vanilla `TryFindLiquid`: the cooldown after a search.
const TRY_FIND_LIQUID_COOLDOWN_TICKS: i64 = 40;

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

/// Flies to a random air or water position ahead of the body
/// (vanilla `RandomStroll.fly`).
pub fn random_fly<E: PathfinderMob + ?Sized>(speed_modifier: f32) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&WALK_TARGET) {
                return false;
            }
            let wander_direction = body.look_angle();
            let target = air_and_water_random_pos(
                body,
                MAX_XZ_DIST,
                MAX_Y_DIST,
                -2,
                wander_direction.x,
                wander_direction.z,
                f64::from(FRAC_PI_2),
            )
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

/// Walks to a nearby block of `fluid_tag` liquid with air above it, when the
/// body stands outside that liquid (vanilla `TryFindLiquid.create`).
pub fn try_find_liquid<E: PathfinderMob + ?Sized>(
    range: i32,
    speed_modifier: f32,
    fluid_tag: &'static Identifier,
) -> impl BehaviorControl<E> {
    let mut next_ok_start_time = 0_i64;
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            {
                let memories = memories.lock();
                if memories.has_value(&ATTACK_TARGET) || memories.has_value(&WALK_TARGET) {
                    return false;
                }
            }
            let body_pos = body.block_position();
            if fluid_has_tag(world.get_block_state(body_pos), fluid_tag) {
                return false;
            }
            if time < next_ok_start_time {
                next_ok_start_time = time + TRY_FIND_LIQUID_RETRY_TICKS;
                return true;
            }
            let position = body.position();
            let mut found = None;
            for pos in body_pos.within_manhattan(range, range, range) {
                if pos.x() == body_pos.x() && pos.z() == body_pos.z() {
                    continue;
                }
                let state = world.get_block_state(pos);
                let block = state.get_block();
                let is_liquid_block =
                    block == &vanilla_blocks::WATER || block == &vanilla_blocks::LAVA;
                if !is_liquid_block || !fluid_has_tag(state, fluid_tag) {
                    continue;
                }
                if world.get_block_state(pos.above()).is_air() {
                    found = Some(pos);
                    break;
                }
                if found.is_none() && !pos.closer_to_center_than(position, 1.5) {
                    found = Some(pos);
                }
            }
            if let Some(pos) = found {
                let mut memories = memories.lock();
                memories.set(&LOOK_TARGET, Some(PositionTracker::block(pos)));
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::block(pos),
                        speed_modifier,
                        0,
                    )),
                );
            }
            next_ok_start_time = time + TRY_FIND_LIQUID_COOLDOWN_TICKS;
            true
        },
    )
}

/// Vanilla `FluidState.is(tag)`.
fn fluid_has_tag(state: BlockStateId, fluid_tag: &Identifier) -> bool {
    let fluid = state.get_fluid_state().fluid_id;
    !fluid.is_empty && fluid.has_tag(fluid_tag)
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
