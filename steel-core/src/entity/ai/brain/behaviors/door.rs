//! Opening the doors on the path and closing them behind (vanilla
//! `InteractWithDoor`).

use std::sync::Arc;

use rustc_hash::FxHashSet;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_utils::{BlockPos, GlobalPos};

use crate::behavior::BLOCK_BEHAVIORS;
use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{DOORS_TO_CLOSE, NEAREST_LIVING_ENTITIES, PATH};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::node::Node;
use crate::entity::ai::path::Path;
use crate::entity::{LivingEntity, SharedEntity};
use crate::world::World;

/// Vanilla `COOLDOWN_BEFORE_RERUNNING_IN_SAME_NODE`.
const COOLDOWN_BEFORE_RERUNNING_IN_SAME_NODE: i32 = 20;
/// Vanilla `SKIP_CLOSING_DOOR_IF_FURTHER_AWAY_THAN`.
const SKIP_CLOSING_DOOR_IF_FURTHER_AWAY_THAN: f64 = 3.0;
/// Vanilla `MAX_DISTANCE_TO_HOLD_DOOR_OPEN_FOR_OTHER_MOBS`.
const MAX_DISTANCE_TO_HOLD_DOOR_OPEN_FOR_OTHER_MOBS: f64 = 2.0;

/// Whether `pos`'s center is closer than `distance` to `position` (vanilla
/// `Vec3i.closerToCenterThan`).
fn closer_to_center_than(pos: BlockPos, position: glam::DVec3, distance: f64) -> bool {
    let (x, y, z) = pos.get_center();
    glam::DVec3::new(x, y, z).distance_squared(position) < distance * distance
}

/// Vanilla `state.is(BlockTags.MOB_INTERACTABLE_DOORS, s -> s.getBlock()
/// instanceof DoorBlock)`: every block in the tag is a door.
fn is_mob_interactable_door(world: &World, pos: BlockPos) -> bool {
    let state = world.get_block_state(pos);
    state.get_block().has_tag(&BlockTag::MOB_INTERACTABLE_DOORS)
        && state.try_get_value(&BlockStateProperties::OPEN).is_some()
}

fn is_open(world: &World, pos: BlockPos) -> bool {
    world
        .get_block_state(pos)
        .try_get_value(&BlockStateProperties::OPEN)
        == Some(true)
}

/// Vanilla `DoorBlock.setOpen`.
fn set_open<E: LivingEntity + ?Sized>(world: &Arc<World>, body: &E, pos: BlockPos, open: bool) {
    let state = world.get_block_state(pos);
    let behavior = BLOCK_BEHAVIORS.get_behavior(state.get_block());
    let _changed =
        behavior.set_door_open(state, world, pos, Some(body.as_entity_event_source()), open);
}

/// Vanilla `rememberDoorToClose`.
fn remember_door_to_close<E: LivingEntity + ?Sized>(world: &World, body: &E, pos: BlockPos) {
    let Some(memories) = body.brain_memories() else {
        return;
    };
    let mut memories = memories.lock();
    let mut doors = memories.get(&DOORS_TO_CLOSE).unwrap_or_default();
    doors.insert(GlobalPos::new(world.key.clone(), pos));
    memories.set(&DOORS_TO_CLOSE, Some(doors));
}

/// Vanilla `InteractWithDoor.create`: opens wooden doors on the next two
/// path nodes and closes the ones it has walked through.
pub fn interact_with_door<E: LivingEntity + ?Sized>() -> impl BehaviorControl<E> {
    let mut last_checked_node: Option<BlockPos> = None;
    let mut remaining_cooldown = 0_i32;
    OneShot::new(
        vec![
            MemoryRequirement::new(&PATH, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&DOORS_TO_CLOSE, MemoryStatus::Registered),
            MemoryRequirement::new(&NEAREST_LIVING_ENTITIES, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let nodes = {
                let memories = memories.lock();
                let Some(path) = memories.get_ref(&PATH) else {
                    return false;
                };
                if path.not_started() || path.is_done() {
                    return false;
                }
                path.previous_node()
                    .map(Node::as_block_pos)
                    .zip(path.next_node().map(Node::as_block_pos))
            };
            let Some((from_pos, to_pos)) = nodes else {
                return false;
            };
            if last_checked_node == Some(to_pos) {
                remaining_cooldown = COOLDOWN_BEFORE_RERUNNING_IN_SAME_NODE;
            } else {
                remaining_cooldown -= 1;
                if remaining_cooldown > 0 {
                    return false;
                }
            }
            last_checked_node = Some(to_pos);

            if is_mob_interactable_door(world, from_pos) {
                if !is_open(world, from_pos) {
                    set_open(world, body, from_pos, true);
                }
                remember_door_to_close(world, body, from_pos);
            }
            if is_mob_interactable_door(world, to_pos) && !is_open(world, to_pos) {
                set_open(world, body, to_pos, true);
                remember_door_to_close(world, body, to_pos);
            }

            let (doors, nearest) = {
                let memories = memories.lock();
                (
                    memories.get(&DOORS_TO_CLOSE),
                    memories.get(&NEAREST_LIVING_ENTITIES),
                )
            };
            if let Some(mut doors) = doors {
                close_doors_that_i_have_opened_or_passed_through(
                    world,
                    body,
                    Some(from_pos),
                    Some(to_pos),
                    &mut doors,
                    nearest.as_deref(),
                );
                memories.lock().set(&DOORS_TO_CLOSE, Some(doors));
            }
            true
        },
    )
}

/// Vanilla `closeDoorsThatIHaveOpenedOrPassedThrough`: closes the doors
/// in `doors` the body is no longer walking through, unless they are too
/// far away or another mob of its kind is about to pass, and forgets all
/// of them but the ones it is walking through.
pub fn close_doors_that_i_have_opened_or_passed_through<E: LivingEntity + ?Sized>(
    world: &Arc<World>,
    body: &E,
    moving_from: Option<BlockPos>,
    moving_to: Option<BlockPos>,
    doors: &mut FxHashSet<GlobalPos>,
    nearest_entities: Option<&[SharedEntity]>,
) {
    doors.retain(|door| {
        let pos = door.pos;
        if moving_from == Some(pos) || moving_to == Some(pos) {
            return true;
        }
        let too_far = door.dimension != world.key
            || !closer_to_center_than(pos, body.position(), SKIP_CLOSING_DOOR_IF_FURTHER_AWAY_THAN);
        if too_far || !is_mob_interactable_door(world, pos) || !is_open(world, pos) {
            return false;
        }
        if !are_other_mobs_coming_through_door(body, pos, nearest_entities) {
            set_open(world, body, pos, false);
        }
        false
    });
}

/// Vanilla `areOtherMobsComingThroughDoor`.
fn are_other_mobs_coming_through_door<E: LivingEntity + ?Sized>(
    body: &E,
    door: BlockPos,
    nearest_entities: Option<&[SharedEntity]>,
) -> bool {
    nearest_entities.is_some_and(|entities| {
        entities.iter().any(|other| {
            other.entity_type() == body.entity_type()
                && closer_to_center_than(
                    door,
                    other.position(),
                    MAX_DISTANCE_TO_HOLD_DOOR_OPEN_FOR_OTHER_MOBS,
                )
                && is_mob_coming_through_door(other, door)
        })
    })
}

/// Vanilla `isMobComingThroughDoor`.
fn is_mob_coming_through_door(other: &SharedEntity, door: BlockPos) -> bool {
    let Some(memories) = other
        .as_living_entity()
        .and_then(LivingEntity::brain_memories)
    else {
        return false;
    };
    let memories = memories.lock();
    let Some(path) = memories.get_ref(&PATH) else {
        return false;
    };
    path_moves_through(path, door)
}

fn path_moves_through(path: &Path, door: BlockPos) -> bool {
    if path.is_done() {
        return false;
    }
    let Some(from) = path.previous_node() else {
        return false;
    };
    door == from.as_block_pos() || path.next_node().is_some_and(|to| door == to.as_block_pos())
}
