//! Vanilla `SpawnUtil`: spawns a mob on a random nearby surface.

use std::sync::Arc;

use rand::RngExt as _;
use steel_math::{DEGREE_360, wrap_degrees};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::shapes::VoxelShape;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_utils::{BlockPos, BlockStateId, Direction};

use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::entity::spawn_placements::spawn_aabb;
use crate::entity::{EntitySpawnReason, SharedEntity, create_entity_instance};
use crate::physics::{CollisionWorld as _, WorldCollisionProvider, merged_face_occludes};
use crate::world::World;

/// Vanilla `SpawnUtil.Strategy`: whether a mob may stand on `state` at `pos`
/// with `above_state` over it.
pub(crate) type SpawnStrategy = fn(&World, BlockPos, BlockStateId, BlockPos, BlockStateId) -> bool;

/// Vanilla `Strategy.ON_TOP_OF_COLLIDER_NO_LEAVES`: on a full top face with
/// nothing solid above, never on leaves.
pub(crate) fn on_top_of_collider_no_leaves(
    world: &World,
    pos: BlockPos,
    state: BlockStateId,
    above_pos: BlockPos,
    above_state: BlockStateId,
) -> bool {
    collision_shape_is_empty(world, above_pos, above_state)
        && !state.get_block().has_tag(&BlockTag::LEAVES)
        && merged_face_occludes(
            BLOCK_BEHAVIORS
                .get_behavior(state.get_block())
                .get_collision_shape(state, world, pos, BlockCollisionContext::empty()),
            VoxelShape::EMPTY,
            Direction::Up,
        )
}

fn collision_shape_is_empty(world: &World, pos: BlockPos, state: BlockStateId) -> bool {
    BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, pos, BlockCollisionContext::empty())
        .is_empty()
}

/// Vanilla `SpawnUtil.trySpawnMob`: up to `attempts` random columns within
/// `range_xz` of `start`, searched `range_y` blocks up and down for a spot
/// `strategy` accepts.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla SpawnUtil.trySpawnMob"
)]
pub(crate) fn try_spawn_mob(
    world: &Arc<World>,
    entity_type: EntityTypeRef,
    reason: EntitySpawnReason,
    start: BlockPos,
    attempts: i32,
    range_xz: i32,
    range_y: i32,
    strategy: SpawnStrategy,
    check_collisions: bool,
) -> Option<SharedEntity> {
    let mut random = rand::rng();
    for _ in 0..attempts {
        let dx = random.random_range(-range_xz..=range_xz);
        let dz = random.random_range(-range_xz..=range_xz);
        let mut search_pos = start.offset(dx, range_y, dz);
        if !world.is_block_within_world_border(search_pos)
            || !move_to_possible_spawn_position(world, range_y, &mut search_pos, strategy)
        {
            continue;
        }
        if check_collisions {
            let aabb = spawn_aabb(
                entity_type,
                f64::from(search_pos.x()) + 0.5,
                f64::from(search_pos.y()),
                f64::from(search_pos.z()) + 0.5,
            );
            let collision = WorldCollisionProvider::new(world);
            if collision.has_entity_collision(&aabb)
                || collision.has_block_collision_with_context(&aabb, BlockCollisionContext::empty())
            {
                continue;
            }
        }
        let Some(mob) = create_mob(world, entity_type, reason, search_pos) else {
            continue;
        };
        let Some(as_mob) = mob.as_mob() else {
            continue;
        };
        if as_mob.check_spawn_rules(world.as_ref(), reason)
            && as_mob.check_spawn_obstruction(world.as_ref())
        {
            if let Err(error) = world.try_add_fresh_entity_with_passengers(Arc::clone(&mob)) {
                log::debug!("SpawnUtil could not add {}: {error:?}", entity_type.key);
                continue;
            }
            as_mob.play_ambient_sound();
            return Some(mob);
        }
    }
    None
}

/// Vanilla `EntityType.create(level, null, pos, reason, false, false)`: the
/// block's bottom center, a random yaw, then `finalizeSpawn`.
fn create_mob(
    world: &Arc<World>,
    entity_type: EntityTypeRef,
    reason: EntitySpawnReason,
    pos: BlockPos,
) -> Option<SharedEntity> {
    let (x, y, z) = pos.get_bottom_center();
    let position = glam::DVec3::new(x, y, z);
    let entity = create_entity_instance(world, entity_type, position).ok()?;
    let yaw = wrap_degrees(rand::random::<f32>() * DEGREE_360);
    entity.base().set_position_local(position);
    entity.set_rotation((yaw, 0.0));
    entity.set_old_position_to_current();
    entity.base().set_old_rotation_to_current();
    if let Some(mob) = entity.as_mob() {
        mob.set_y_head_rot(yaw);
        mob.set_y_body_rot(yaw);
        let _ = mob.finalize_spawn(world, reason, None);
    }
    Some(entity)
}

/// Vanilla `SpawnUtil.moveToPossibleSpawnPosition`: walks down from the top
/// of the column to the first block the strategy accepts and stands above it.
fn move_to_possible_spawn_position(
    world: &World,
    range_y: i32,
    search_pos: &mut BlockPos,
    strategy: SpawnStrategy,
) -> bool {
    let mut above_state = world.get_block_state(*search_pos);
    for _ in (-range_y..=range_y).rev() {
        *search_pos = search_pos.below();
        let above_pos = search_pos.above();
        let current_state = world.get_block_state(*search_pos);
        if strategy(world, *search_pos, current_state, above_pos, above_state) {
            *search_pos = search_pos.above();
            return true;
        }
        above_state = current_state;
    }
    false
}
