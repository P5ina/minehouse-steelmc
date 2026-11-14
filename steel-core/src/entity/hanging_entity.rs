//! Vanilla `HangingEntity`: a block-attached entity hanging on a wall, such as an
//! item frame or a painting.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_utils::{BlockPos, Direction, WorldAabb};

use crate::behavior::BLOCK_BEHAVIORS;
use crate::entity::block_attached_entity::BlockAttachedEntity;
use crate::entity::physics_state_for_move;
use crate::physics::{CollisionWorld as _, WorldCollisionProvider};
use crate::world::{LevelReader as _, World};

/// Vanilla `HangingEntity`.
pub trait HangingEntity: BlockAttachedEntity {
    /// Vanilla `getDirection`: the way the entity faces, away from its wall.
    fn direction(&self) -> Direction;

    /// Vanilla `getPopBox`: the box that must stay clear of blocks and other
    /// hanging entities.
    fn pop_box(&self) -> WorldAabb {
        self.bounding_box()
    }

    /// Vanilla `HangingEntity.survives`: free space, a solid block (or a diode)
    /// behind every part, and no hanging entity in the way.
    fn survives_hanging_entity(&self) -> bool {
        let Some(world) = self.level() else {
            return true;
        };
        let pop_box = self.pop_box();
        if self.has_level_collision(&world, &pop_box) {
            return false;
        }
        let supported = blocks_in(&self.support_box()).all(|pos| {
            let state = world.get_block_state(pos);
            state.is_solid() || BLOCK_BEHAVIORS.get_behavior(state.get_block()).is_diode()
        });
        supported && self.can_coexist(&world, false)
    }

    /// Vanilla `calculateSupportBox`: the entity's box pushed half a block into
    /// its wall.
    fn support_box(&self) -> WorldAabb {
        let step = self.direction().offset_vec().as_dvec3() * -0.5;
        self.bounding_box().translate(step).deflate(1.0E-7)
    }

    /// Vanilla `canCoexist`: no other hanging entity facing the same way, nor
    /// unless allowed one of the same type, overlaps the pop box.
    fn can_coexist(&self, world: &World, allow_intersecting_same_type: bool) -> bool {
        let id = self.id();
        let entity_type = self.entity_type();
        let direction = self.direction();
        !world.has_entity_in_aabb_matching(&self.pop_box(), |other| {
            other.id() != id
                && other.as_hanging_entity().is_some_and(|hanging| {
                    let intersects_same_type =
                        !allow_intersecting_same_type && hanging.entity_type() == entity_type;
                    intersects_same_type || hanging.direction() == direction
                })
        })
    }

    /// Vanilla `hasLevelCollision`.
    fn has_level_collision(&self, world: &Arc<World>, pop_box: &WorldAabb) -> bool {
        let entity = self.as_entity_event_source();
        let collisions = WorldCollisionProvider::for_entity(world, entity);
        collisions.has_block_collision_with_context(
            pop_box,
            physics_state_for_move(entity).block_collision_context(),
        ) || collisions.has_world_border_collision(pop_box)
    }

    /// Vanilla `HangingEntity.spawnAtLocation`: drops sit a little in front of the wall.
    fn spawn_in_front(&self, item: ItemStack) {
        let step = self.direction().offset_vec();
        let offset = DVec3::new(
            f64::from(step.x as f32 * 0.15),
            0.0,
            f64::from(step.z as f32 * 0.15),
        );
        self.spawn_at_location_with_offset(item, offset);
    }
}

/// Vanilla `BlockPos.betweenClosedStream(AABB)`: every block the box touches.
fn blocks_in(aabb: &WorldAabb) -> impl Iterator<Item = BlockPos> {
    BlockPos::between_closed(
        BlockPos::containing(aabb.min_x(), aabb.min_y(), aabb.min_z()),
        BlockPos::containing(aabb.max_x(), aabb.max_y(), aabb.max_z()),
    )
}
