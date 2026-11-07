//! Vanilla frogspawn block behavior (`FrogspawnBlock`).
//!
//! Frogspawn sits on water and hatches into two to five tadpoles a few
//! minutes after it is placed.

use std::sync::Arc;

use glam::DVec3;
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::{sound_events, vanilla_entities};
use steel_utils::{BlockPos, BlockStateId, Direction};

use crate::behavior::block::BlockBehavior;
use crate::behavior::blocks::vegetation::vegetation_block::survival_update_shape;
use crate::behavior::context::BlockPlaceContext;
use crate::entity::{Entity, InsideBlockEffectCollector, create_entity_instance};
use crate::fluid::get_fluid_state_from_block;
use crate::world::{LevelReader, ScheduledTickAccess, World};

/// Vanilla `FrogspawnBlock.MIN_TADPOLES_SPAWN`.
const MIN_TADPOLES_SPAWN: u32 = 2;
/// Vanilla `FrogspawnBlock.MAX_TADPOLES_SPAWN`.
const MAX_TADPOLES_SPAWN: u32 = 5;
/// Vanilla `FrogspawnBlock.DEFAULT_MIN_HATCH_TICK_DELAY`.
const MIN_HATCH_TICK_DELAY: i32 = 3600;
/// Vanilla `FrogspawnBlock.DEFAULT_MAX_HATCH_TICK_DELAY`.
const MAX_HATCH_TICK_DELAY: i32 = 12_000;
/// Vanilla `getRandomTadpolePositionOffset`: keeps a tadpole's hitbox inside
/// the block.
const TADPOLE_OFFSET_MIN: f64 = 0.2_f32 as f64;
const TADPOLE_OFFSET_MAX: f64 = 0.799_999_997_019_767_8;

/// Behavior for vanilla frogspawn.
#[block_behavior]
pub struct FrogspawnBlock {
    block: BlockRef,
}

impl FrogspawnBlock {
    /// Creates a new frogspawn block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    /// Vanilla `FrogspawnBlock.mayPlaceOn`: frogspawn-supporting water or
    /// block with nothing fluid above it.
    fn may_place_on(world: &dyn LevelReader, pos: BlockPos) -> bool {
        let state = world.get_block_state(pos);
        let fluid = get_fluid_state_from_block(state);
        let fluid_above = get_fluid_state_from_block(world.get_block_state(pos.above()));
        ((!fluid.is_empty() && fluid.fluid_id.has_tag(&FluidTag::SUPPORTS_FROGSPAWN))
            || state.get_block().has_tag(&BlockTag::SUPPORTS_FROGSPAWN))
            && fluid_above.is_empty()
    }

    /// Vanilla `getFrogspawnHatchDelay`.
    fn hatch_delay() -> i32 {
        rand::random_range(MIN_HATCH_TICK_DELAY..MAX_HATCH_TICK_DELAY)
    }

    fn tadpole_position_offset() -> f64 {
        rand::random::<f64>().clamp(TADPOLE_OFFSET_MIN, TADPOLE_OFFSET_MAX)
    }

    /// Vanilla `hatchFrogspawn`.
    fn hatch(world: &Arc<World>, pos: BlockPos) {
        world.destroy_block(pos, false);
        world.play_block_sound(&sound_events::BLOCK_FROGSPAWN_HATCH, pos, 1.0, 1.0, None);
        Self::spawn_tadpoles(world, pos);
    }

    /// Vanilla `spawnTadpoles`.
    fn spawn_tadpoles(world: &Arc<World>, pos: BlockPos) {
        let amount = rand::random_range(MIN_TADPOLES_SPAWN..=MAX_TADPOLES_SPAWN);
        for _ in 0..amount {
            let position = DVec3::new(
                f64::from(pos.x()) + Self::tadpole_position_offset(),
                f64::from(pos.y()) - 0.5,
                f64::from(pos.z()) + Self::tadpole_position_offset(),
            );
            let Ok(tadpole) = create_entity_instance(world, &vanilla_entities::TADPOLE, position)
            else {
                continue;
            };
            let y_rot = rand::random_range(1..=360_u16);
            tadpole.snap_to(position, f32::from(y_rot), 0.0);
            if let Some(mob) = tadpole.as_mob() {
                mob.set_persistence_required();
            }
            if let Err(error) = world.try_add_entity(tadpole) {
                log::debug!("failed to add hatched tadpole at {pos:?}: {error}");
            }
        }
    }
}

impl BlockBehavior for FrogspawnBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn can_survive(&self, _state: BlockStateId, world: &dyn LevelReader, pos: BlockPos) -> bool {
        Self::may_place_on(world, pos.below())
    }

    fn on_place(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        world.schedule_block_tick_default(pos, self.block, Self::hatch_delay());
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        survival_update_shape(self, state, world, pos)
    }

    fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        if self.can_survive(state, world.as_ref(), pos) {
            Self::hatch(world, pos);
        } else {
            world.destroy_block(pos, false);
        }
    }

    fn entity_inside(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        entity: &dyn Entity,
        _effect_collector: &mut InsideBlockEffectCollector,
        _is_precise: bool,
    ) {
        if entity.entity_type() == &vanilla_entities::FALLING_BLOCK {
            world.destroy_block(pos, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{ChunkPos, WorldAabb};

    use super::*;
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::entity::{Mob, init_entities};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn frogspawn_needs_open_water_below() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("frogspawn_survival");
        let world = &fixture.world;
        let pos = BlockPos::new(8, 64, 8);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        let behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::FROGSPAWN);
        let state = vanilla_blocks::FROGSPAWN.default_state();

        assert!(!behavior.can_survive(state, world.as_ref(), pos));
        assert!(world.set_block(
            pos.below(),
            vanilla_blocks::WATER.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        assert!(behavior.can_survive(state, world.as_ref(), pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::WATER.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        assert!(!behavior.can_survive(state, world.as_ref(), pos));
    }

    #[test]
    fn frogspawn_hatches_into_tadpoles() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world("frogspawn_hatch");
        let world = &fixture.world;
        let pos = BlockPos::new(8, 64, 8);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos.below(),
            vanilla_blocks::WATER.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        assert!(world.set_block(
            pos,
            vanilla_blocks::FROGSPAWN.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));

        let behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::FROGSPAWN);
        behavior.tick(world.get_block_state(pos), world, pos);
        assert!(world.get_block_state(pos).is_air());

        let bounds = WorldAabb::new(
            f64::from(pos.x()) - 1.0,
            f64::from(pos.y()) - 2.0,
            f64::from(pos.z()) - 1.0,
            f64::from(pos.x()) + 2.0,
            f64::from(pos.y()) + 2.0,
            f64::from(pos.z()) + 2.0,
        );
        let tadpoles = world
            .get_entities_in_aabb(&bounds)
            .iter()
            .filter(|entity| {
                entity.entity_type() == &vanilla_entities::TADPOLE
                    && entity.as_mob().is_some_and(Mob::is_persistence_required)
            })
            .count();
        assert!(
            (2..=5).contains(&tadpoles),
            "frogspawn hatched {tadpoles} tadpoles"
        );
    }
}
