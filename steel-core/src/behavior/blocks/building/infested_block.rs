//! Vanilla `InfestedBlock` and `InfestedRotatedPillarBlock`: stone-like
//! blocks that hide a silverfish.

use std::ptr;
use std::sync::{Arc, OnceLock};

use glam::DVec3;
use steel_macros::block_behavior;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_enchantment_tags::EnchantmentTag;
use steel_registry::vanilla_game_rules::BLOCK_DROPS;
use steel_registry::{REGISTRY, RegistryExt, TaggedRegistryExt, vanilla_entities};
use steel_utils::entity_events::EntityStatus;
use steel_utils::{BlockPos, BlockStateId};

use super::RotatedPillarBlock;
use crate::behavior::{BLOCK_BEHAVIORS, BlockBehavior, BlockPlaceContext};
use crate::entity::create_entity_instance;
use crate::world::World;

/// Vanilla `InfestedBlock`.
#[block_behavior]
pub struct InfestedBlock {
    block: BlockRef,
    #[json_arg(vanilla_blocks, json = "host_block")]
    host_block: BlockRef,
}

impl InfestedBlock {
    /// Creates an infested block that imitates `host_block`.
    #[must_use]
    pub const fn new(block: BlockRef, host_block: BlockRef) -> Self {
        Self { block, host_block }
    }
}

impl BlockBehavior for InfestedBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn spawn_after_break(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        tool: &ItemStack,
        _drop_experience: bool,
    ) {
        spawn_after_break_infested(world, pos, tool);
    }

    fn infested_host_block(&self) -> Option<BlockRef> {
        Some(self.host_block)
    }
}

/// Vanilla `InfestedRotatedPillarBlock` (infested deepslate).
#[block_behavior]
pub struct InfestedRotatedPillarBlock {
    block: BlockRef,
    #[json_arg(vanilla_blocks, json = "host_block")]
    host_block: BlockRef,
}

impl InfestedRotatedPillarBlock {
    /// Creates an infested pillar block that imitates `host_block`.
    #[must_use]
    pub const fn new(block: BlockRef, host_block: BlockRef) -> Self {
        Self { block, host_block }
    }
}

impl BlockBehavior for InfestedRotatedPillarBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(RotatedPillarBlock::placement_state(self.block, context))
    }

    fn spawn_after_break(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        tool: &ItemStack,
        _drop_experience: bool,
    ) {
        spawn_after_break_infested(world, pos, tool);
    }

    fn infested_host_block(&self) -> Option<BlockRef> {
        Some(self.host_block)
    }
}

/// Vanilla `InfestedBlock.spawnAfterBreak`: a silverfish crawls out unless
/// block drops are off or the tool prevents it (silk touch).
fn spawn_after_break_infested(world: &Arc<World>, pos: BlockPos, tool: &ItemStack) {
    if world.get_game_rule(&BLOCK_DROPS) && !prevents_infested_spawns(tool) {
        spawn_infestation(world, pos);
    }
}

/// Vanilla `EnchantmentHelper.hasTag(tool, PREVENTS_INFESTED_SPAWNS)`.
fn prevents_infested_spawns(tool: &ItemStack) -> bool {
    let Some(enchantments) = tool.get_enchantments() else {
        return false;
    };
    enchantments.iter().any(|(key, _)| {
        REGISTRY
            .enchantments
            .by_key(key)
            .is_some_and(|enchantment| {
                REGISTRY
                    .enchantments
                    .is_in_tag(enchantment, &EnchantmentTag::PREVENTS_INFESTED_SPAWNS)
            })
    })
}

/// Vanilla `InfestedBlock.spawnInfestation`.
fn spawn_infestation(world: &Arc<World>, pos: BlockPos) {
    let position = DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()),
        f64::from(pos.z()) + 0.5,
    );
    let Ok(silverfish) = create_entity_instance(world, &vanilla_entities::SILVERFISH, position)
    else {
        return;
    };
    silverfish.set_rotation((0.0, 0.0));
    if world.try_add_entity(Arc::clone(&silverfish)).is_ok() {
        silverfish.broadcast_entity_event(EntityStatus::SilverfishMergeAnim);
    }
}

/// Host and infested block pairs, read from the registered behaviors.
fn infested_pairs() -> &'static [(BlockRef, BlockRef)] {
    static PAIRS: OnceLock<Vec<(BlockRef, BlockRef)>> = OnceLock::new();
    PAIRS.get_or_init(|| {
        REGISTRY
            .blocks
            .iter()
            .filter_map(|(_, infested)| {
                BLOCK_BEHAVIORS
                    .get_behavior(infested)
                    .infested_host_block()
                    .map(|host| (host, infested))
            })
            .collect()
    })
}

/// Vanilla `InfestedBlock.isCompatibleHostBlock`.
#[must_use]
pub(crate) fn is_compatible_host_block(state: BlockStateId) -> bool {
    let block = state.get_block();
    infested_pairs()
        .iter()
        .any(|(host, _)| ptr::eq(*host, block))
}

/// Vanilla `InfestedBlock.infestedStateByHost`, keeping matching properties.
#[must_use]
pub(crate) fn infested_state_by_host(host_state: BlockStateId) -> Option<BlockStateId> {
    let block = host_state.get_block();
    infested_pairs()
        .iter()
        .find(|(host, _)| ptr::eq(*host, block))
        .map(|(_, infested)| {
            REGISTRY
                .blocks
                .copy_matching_properties(host_state, infested)
        })
}

/// Vanilla `InfestedBlock.hostStateByInfested`; `None` for blocks that are
/// not infested.
#[must_use]
pub(crate) fn host_state_by_infested(infested_state: BlockStateId) -> Option<BlockStateId> {
    BLOCK_BEHAVIORS
        .get_behavior(infested_state.get_block())
        .infested_host_block()
        .map(|host| {
            REGISTRY
                .blocks
                .copy_matching_properties(infested_state, host)
        })
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::{ChunkPos, Identifier, WorldAabb};

    use steel_utils::axis::Axis;

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::entity::init_entities;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    fn silverfish_near(world: &Arc<World>, pos: BlockPos) -> usize {
        let bounds = WorldAabb::new(
            f64::from(pos.x()) - 1.0,
            f64::from(pos.y()) - 1.0,
            f64::from(pos.z()) - 1.0,
            f64::from(pos.x()) + 2.0,
            f64::from(pos.y()) + 2.0,
            f64::from(pos.z()) + 2.0,
        );
        world
            .get_entities_in_aabb(&bounds)
            .iter()
            .filter(|entity| entity.entity_type() == &vanilla_entities::SILVERFISH)
            .count()
    }

    #[test]
    fn host_and_infested_states_map_both_ways() {
        init_vanilla_registry();
        init_behaviors();
        let stone = vanilla_blocks::STONE.default_state();
        assert!(is_compatible_host_block(stone));
        assert!(!is_compatible_host_block(
            vanilla_blocks::DIRT.default_state()
        ));
        assert_eq!(
            infested_state_by_host(stone),
            Some(vanilla_blocks::INFESTED_STONE.default_state())
        );
        assert_eq!(
            host_state_by_infested(vanilla_blocks::INFESTED_STONE.default_state()),
            Some(stone)
        );
        assert_eq!(host_state_by_infested(stone), None);

        let deepslate = vanilla_blocks::DEEPSLATE
            .default_state()
            .set_value(RotatedPillarBlock::AXIS, Axis::X);
        let infested = infested_state_by_host(deepslate).expect("deepslate hosts silverfish");
        assert_eq!(infested.get_block(), &vanilla_blocks::INFESTED_DEEPSLATE);
        assert_eq!(infested.get_value(RotatedPillarBlock::AXIS), Axis::X);
    }

    #[test]
    fn breaking_an_infested_block_releases_a_silverfish_without_silk_touch() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let world_fixture = fresh_test_world("infested_block_break");
        let world = &world_fixture.world;
        let pos = BlockPos::new(8, 64, 8);
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(pos));
        let state = vanilla_blocks::INFESTED_STONE.default_state();
        let behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::INFESTED_STONE);

        let mut silk_touch = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        silk_touch.set_enchantments(&[(Identifier::vanilla_static("silk_touch"), 1)], false);
        behavior.spawn_after_break(state, world, pos, &silk_touch, true);
        assert_eq!(silverfish_near(world, pos), 0, "silk touch keeps it inside");

        let pickaxe = ItemStack::new(&vanilla_items::DIAMOND_PICKAXE);
        behavior.spawn_after_break(state, world, pos, &pickaxe, true);
        assert_eq!(silverfish_near(world, pos), 1, "a silverfish crawls out");
    }
}
