//! Vanilla sniffer egg block behavior (`SnifferEggBlock`).
//!
//! A sniffer egg cracks twice on scheduled ticks and then hatches into a baby
//! sniffer. Sitting on a `sniffer_egg_hatch_boost` block (moss) halves the
//! hatch time.

use std::sync::Arc;

use glam::DVec3;
use steel_macros::block_behavior;
use steel_math::wrap_degrees;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, IntProperty};
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{level_events, sound_events, vanilla_entities, vanilla_game_events};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId};

use crate::behavior::block::BlockBehavior;
use crate::behavior::context::BlockPlaceContext;
use crate::entity::ai::path::PathComputationType;
use crate::entity::create_entity_instance;
use crate::world::LevelReader;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `SnifferEggBlock.MAX_HATCH_LEVEL`.
const MAX_HATCH_LEVEL: u8 = 2;
/// Vanilla `REGULAR_HATCH_TIME_TICKS`.
const REGULAR_HATCH_TIME_TICKS: i32 = 24_000;
/// Vanilla `BOOSTED_HATCH_TIME_TICKS`.
const BOOSTED_HATCH_TIME_TICKS: i32 = 12_000;
/// Vanilla `RANDOM_HATCH_OFFSET_TICKS`.
const RANDOM_HATCH_OFFSET_TICKS: i32 = 300;

const HATCH: &IntProperty = &BlockStateProperties::HATCH;

/// Behavior for vanilla sniffer eggs.
#[block_behavior]
pub struct SnifferEggBlock {
    block: BlockRef,
}

impl SnifferEggBlock {
    /// Creates a new sniffer egg block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    /// Vanilla `SnifferEggBlock.hatchBoost`.
    #[must_use]
    pub fn hatch_boost(level: &(impl LevelReader + ?Sized), pos: BlockPos) -> bool {
        level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::SNIFFER_EGG_HATCH_BOOST)
    }

    fn hatch_pitch() -> f32 {
        0.9 + rand::random::<f32>() * 0.2
    }

    /// Spawns the baby sniffer at the center of the egg's block.
    fn spawn_baby(world: &Arc<World>, pos: BlockPos) {
        let (x, y, z) = pos.get_center();
        let Ok(sniffer) =
            create_entity_instance(world, &vanilla_entities::SNIFFER, DVec3::new(x, y, z))
        else {
            return;
        };
        if let Some(mob) = sniffer.as_mob() {
            mob.set_baby(true);
        }
        sniffer.snap_to(
            DVec3::new(x, y, z),
            wrap_degrees(rand::random::<f32>() * 360.0),
            0.0,
        );
        if let Err(error) = world.try_add_entity(sniffer) {
            log::debug!("failed to add hatched sniffer at {pos:?}: {error}");
        }
    }
}

impl BlockBehavior for SnifferEggBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let hatch = state.get_value(HATCH);
        if hatch < MAX_HATCH_LEVEL {
            world.play_block_sound(
                &sound_events::BLOCK_SNIFFER_EGG_CRACK,
                pos,
                0.7,
                Self::hatch_pitch(),
                None,
            );
            world.set_block(
                pos,
                state.set_value(HATCH, hatch + 1),
                UpdateFlags::UPDATE_CLIENTS,
            );
        } else {
            world.play_block_sound(
                &sound_events::BLOCK_SNIFFER_EGG_HATCH,
                pos,
                0.7,
                Self::hatch_pitch(),
                None,
            );
            world.destroy_block(pos, false);
            Self::spawn_baby(world, pos);
        }
    }

    fn on_place(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        let boosted = Self::hatch_boost(world.as_ref(), pos);
        if boosted {
            world.level_event(level_events::PARTICLES_EGG_CRACK, pos, 0, None);
        }
        let hatch_time = if boosted {
            BOOSTED_HATCH_TIME_TICKS
        } else {
            REGULAR_HATCH_TIME_TICKS
        };
        let progression_tick_delay = hatch_time / 3;
        world.game_event(
            &vanilla_game_events::BLOCK_PLACE,
            pos,
            &GameEventContext::new(None, Some(state)),
        );
        world.schedule_block_tick_default(
            pos,
            self.block,
            progression_tick_delay + rand::random_range(0..RANDOM_HATCH_OFFSET_TICKS),
        );
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::{ChunkPos, WorldAabb};

    use super::*;
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::entity::init_entities;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn eggs_crack_twice_then_hatch() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let world_fixture = fresh_test_world("sniffer_egg_hatch");
        let world = &world_fixture.world;
        let pos = BlockPos::new(8, 64, 8);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(world.set_block(
            pos,
            vanilla_blocks::SNIFFER_EGG.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        let behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::SNIFFER_EGG);

        behavior.tick(world.get_block_state(pos), world, pos);
        assert_eq!(world.get_block_state(pos).get_value(HATCH), 1);
        behavior.tick(world.get_block_state(pos), world, pos);
        assert_eq!(world.get_block_state(pos).get_value(HATCH), 2);
        behavior.tick(world.get_block_state(pos), world, pos);
        assert!(world.get_block_state(pos).is_air());

        let bounds = WorldAabb::new(
            f64::from(pos.x()) - 1.0,
            f64::from(pos.y()) - 1.0,
            f64::from(pos.z()) - 1.0,
            f64::from(pos.x()) + 2.0,
            f64::from(pos.y()) + 2.0,
            f64::from(pos.z()) + 2.0,
        );
        let babies = world
            .get_entities_in_aabb(&bounds)
            .iter()
            .filter(|entity| {
                entity.entity_type() == &vanilla_entities::SNIFFER
                    && entity
                        .as_living_entity()
                        .is_some_and(|sniffer| sniffer.is_baby())
            })
            .count();
        assert_eq!(babies, 1, "a baby sniffer hatches");
    }

    #[test]
    fn moss_boosts_hatching() {
        init_vanilla_registry();
        let world_fixture = fresh_test_world("sniffer_egg_boost");
        let world = &world_fixture.world;
        let pos = BlockPos::new(8, 64, 8);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        assert!(!SnifferEggBlock::hatch_boost(world.as_ref(), pos));
        assert!(world.set_block(
            pos.below(),
            vanilla_blocks::MOSS_BLOCK.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        assert!(SnifferEggBlock::hatch_boost(world.as_ref(), pos));
    }
}
