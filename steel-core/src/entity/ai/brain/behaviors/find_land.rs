//! Vanilla `TryFindLand`, `TryFindLandNearLiquid` and
//! `TryLaySpawnOnFluidNearLand`: amphibians heading out of the water and
//! laying spawn at its edge.

use std::sync::Arc;

use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::{sound_events, vanilla_game_events};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Direction, Identifier};

use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, IS_PREGNANT, LOOK_TARGET, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::{Entity, LivingEntity, PathfinderMob};
use crate::world::game_event::GameEventContext;
use crate::world::{LevelReader as _, World};

/// Vanilla `TryFindLand.COOLDOWN_TICKS`.
const TRY_FIND_LAND_COOLDOWN_TICKS: i64 = 60;
/// Vanilla `TryFindLandNearLiquid`'s cooldown.
const TRY_FIND_LAND_NEAR_LIQUID_COOLDOWN_TICKS: i64 = 40;
/// Below this much of a block's height a box does not reach its top face.
const FACE_EPSILON: f64 = 1.0e-7;

static WATER: Identifier = FluidTag::WATER;
static SUPPORTS_FROGSPAWN_FLUID: Identifier = FluidTag::SUPPORTS_FROGSPAWN;
static SUPPORTS_FROGSPAWN_BLOCK: Identifier = BlockTag::SUPPORTS_FROGSPAWN;

const FIND_LAND_MEMORIES: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
];

/// Vanilla `FluidState.is(tag)`.
fn fluid_has_tag(state: BlockStateId, fluid_tag: &Identifier) -> bool {
    let fluid = state.get_fluid_state().fluid_id;
    !fluid.is_empty && fluid.has_tag(fluid_tag)
}

fn collision_shape_is_empty(
    world: &World,
    state: BlockStateId,
    pos: BlockPos,
    context: BlockCollisionContext,
) -> bool {
    BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, pos, context)
        .is_empty()
}

/// Vanilla `getCollisionShape(level, pos).getFaceShape(Direction.UP).isEmpty()`:
/// no part of the collision shape reaches the top of the block.
fn top_face_is_empty(world: &World, pos: BlockPos) -> bool {
    let state = world.get_block_state(pos);
    BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, pos, BlockCollisionContext::empty())
        .iter()
        .all(|aabb| {
            aabb.min_y() >= 1.0 - FACE_EPSILON
                || aabb.max_y() < 1.0 - FACE_EPSILON
                || aabb.min_x() >= aabb.max_x()
                || aabb.min_z() >= aabb.max_z()
        })
}

fn entity_collision_context(body: &(impl LivingEntity + ?Sized)) -> BlockCollisionContext {
    BlockCollisionContext::entity(body.position().y, body.is_descending())
}

/// Whether `ATTACK_TARGET` and `WALK_TARGET` are both absent.
fn has_no_targets(body: &(impl LivingEntity + ?Sized)) -> bool {
    body.brain_memories().is_some_and(|memories| {
        let memories = memories.lock();
        !memories.has_value(&ATTACK_TARGET) && !memories.has_value(&WALK_TARGET)
    })
}

fn walk_and_look_to(body: &(impl LivingEntity + ?Sized), pos: BlockPos, speed: f32, close: i32) {
    if let Some(memories) = body.brain_memories() {
        let mut memories = memories.lock();
        memories.set(&LOOK_TARGET, Some(PositionTracker::block(pos)));
        memories.set(
            &WALK_TARGET,
            Some(WalkTarget::new(PositionTracker::block(pos), speed, close)),
        );
    }
}

/// Vanilla `TryFindLand`: from water, walks to the first dry spot within
/// `range` that it could stand on.
pub fn try_find_land<E: PathfinderMob + ?Sized>(
    range: i32,
    speed_modifier: f32,
) -> impl BehaviorControl<E> {
    let mut next_ok_start_time = 0_i64;
    OneShot::new(
        FIND_LAND_MEMORIES.to_vec(),
        move |world: &Arc<World>, body: &E, time: i64| {
            if !has_no_targets(body) {
                return false;
            }
            let body_pos = body.block_position();
            if !fluid_has_tag(world.get_block_state(body_pos), &WATER) {
                return false;
            }
            if time < next_ok_start_time {
                next_ok_start_time = time + TRY_FIND_LAND_COOLDOWN_TICKS;
                return true;
            }
            let context = entity_collision_context(body);
            let found = body_pos
                .within_manhattan(range, range, range)
                .filter(|pos| pos.x() != body_pos.x() || pos.z() != body_pos.z())
                .find(|&pos| {
                    let state = world.get_block_state(pos);
                    if !state.get_fluid_state().fluid_id.is_empty {
                        return false;
                    }
                    let below = pos.below();
                    collision_shape_is_empty(world, state, pos, context)
                        && world.is_face_sturdy(world.get_block_state(below), below, Direction::Up)
                });
            if let Some(pos) = found {
                walk_and_look_to(body, pos, speed_modifier, 1);
            }
            next_ok_start_time = time + TRY_FIND_LAND_COOLDOWN_TICKS;
            true
        },
    )
}

/// Vanilla `TryFindLandNearLiquid`: out of `fluid_tag`, walks to a dry spot
/// next to an open edge of it.
pub fn try_find_land_near_liquid<E: PathfinderMob + ?Sized>(
    range: i32,
    speed_modifier: f32,
    fluid_tag: &'static Identifier,
) -> impl BehaviorControl<E> {
    let mut next_ok_start_time = 0_i64;
    OneShot::new(
        FIND_LAND_MEMORIES.to_vec(),
        move |world: &Arc<World>, body: &E, time: i64| {
            if !has_no_targets(body) {
                return false;
            }
            let body_pos = body.block_position();
            if fluid_has_tag(world.get_block_state(body_pos), fluid_tag) {
                return false;
            }
            if time < next_ok_start_time {
                next_ok_start_time = time + TRY_FIND_LAND_NEAR_LIQUID_COOLDOWN_TICKS;
                return true;
            }
            let context = entity_collision_context(body);
            let found = body_pos
                .within_manhattan(range, range, range)
                .filter(|pos| pos.x() != body_pos.x() || pos.z() != body_pos.z())
                .find(|&pos| {
                    if !collision_shape_is_empty(world, world.get_block_state(pos), pos, context) {
                        return false;
                    }
                    // Vanilla tests the block below with the target's position.
                    if collision_shape_is_empty(
                        world,
                        world.get_block_state(pos.below()),
                        pos,
                        context,
                    ) {
                        return false;
                    }
                    Direction::HORIZONTAL.into_iter().any(|direction| {
                        let side = pos.relative(direction);
                        world.get_block_state(side).is_air()
                            && fluid_has_tag(world.get_block_state(side.below()), fluid_tag)
                    })
                });
            if let Some(pos) = found {
                walk_and_look_to(body, pos, speed_modifier, 0);
            }
            next_ok_start_time = time + TRY_FIND_LAND_NEAR_LIQUID_COOLDOWN_TICKS;
            true
        },
    )
}

/// Vanilla `TryLaySpawnOnFluidNearLand`: a pregnant body on the shore lays
/// `spawn_block` on the water next to it.
pub fn try_lay_spawn_on_fluid_near_land<E: LivingEntity>(
    spawn_block: BlockRef,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&IS_PREGNANT, MemoryStatus::ValuePresent),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            {
                let memories = memories.lock();
                if memories.has_value(&ATTACK_TARGET)
                    || !memories.has_value(&WALK_TARGET)
                    || !memories.has_value(&IS_PREGNANT)
                {
                    return false;
                }
            }
            if body.is_in_water() || !body.on_ground() {
                return false;
            }
            let below_pos = body.block_position().below();
            for direction in Direction::HORIZONTAL {
                let relative_pos = below_pos.relative(direction);
                let relative_state = world.get_block_state(relative_pos);
                let supports_spawn = fluid_has_tag(relative_state, &SUPPORTS_FROGSPAWN_FLUID)
                    || relative_state
                        .get_block()
                        .has_tag(&SUPPORTS_FROGSPAWN_BLOCK);
                if !top_face_is_empty(world, relative_pos) || !supports_spawn {
                    continue;
                }
                let spawn_pos = relative_pos.above();
                if world.get_block_state(spawn_pos).is_air() {
                    let new_state = spawn_block.default_state();
                    world.set_block(spawn_pos, new_state, UpdateFlags::UPDATE_ALL);
                    world.game_event(
                        &vanilla_game_events::BLOCK_PLACE,
                        spawn_pos,
                        &GameEventContext::new(Some(body as &dyn Entity), Some(new_state)),
                    );
                    world.play_sound_at(
                        &sound_events::ENTITY_FROG_LAY_SPAWN,
                        SoundSource::Blocks,
                        body.position(),
                        1.0,
                        1.0,
                        None,
                    );
                    memories.lock().erase(&IS_PREGNANT);
                    return true;
                }
            }
            true
        },
    )
}
