use std::sync::{Arc, LazyLock};

use glam::DVec3;
use steel_macros::block_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, Direction, EnumProperty};
use steel_registry::{vanilla_blocks, vanilla_entities};
use steel_utils::{BlockPos, BlockStateId};

use crate::behavior::block_pattern::{BlockPattern, BlockPatternMatch};
use crate::behavior::{BlockBehavior, BlockPlaceContext, BlockRef};
use crate::entity::entities::{IronGolemEntity, SnowGolemEntity};
use crate::entity::{SharedEntity, next_entity_id};
use crate::world::World;

const HORIZONTAL_FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;

fn is_pumpkin(state: BlockStateId) -> bool {
    let block = state.get_block();
    block == &vanilla_blocks::CARVED_PUMPKIN || block == &vanilla_blocks::JACK_O_LANTERN
}

fn is_snow_block(state: BlockStateId) -> bool {
    state.get_block() == &vanilla_blocks::SNOW_BLOCK
}

fn is_iron_block(state: BlockStateId) -> bool {
    state.get_block() == &vanilla_blocks::IRON_BLOCK
}

fn is_air(state: BlockStateId) -> bool {
    state.is_air()
}

static SNOW_GOLEM_BASE: LazyLock<BlockPattern> =
    LazyLock::new(|| BlockPattern::new(&[&[" ", "#", "#"]], &[('#', is_snow_block)]));
static SNOW_GOLEM_FULL: LazyLock<BlockPattern> = LazyLock::new(|| {
    BlockPattern::new(
        &[&["^", "#", "#"]],
        &[('^', is_pumpkin), ('#', is_snow_block)],
    )
});
static IRON_GOLEM_BASE: LazyLock<BlockPattern> = LazyLock::new(|| {
    BlockPattern::new(
        &[&["~ ~", "###", "~#~"]],
        &[('#', is_iron_block), ('~', is_air)],
    )
});
static IRON_GOLEM_FULL: LazyLock<BlockPattern> = LazyLock::new(|| {
    BlockPattern::new(
        &[&["~^~", "###", "~#~"]],
        &[('^', is_pumpkin), ('#', is_iron_block), ('~', is_air)],
    )
});

/// Behavior for carved pumpkins and jack o'lanterns.
#[block_behavior]
pub struct CarvedPumpkinBlock {
    block: BlockRef,
}

impl CarvedPumpkinBlock {
    /// Creates a carved pumpkin block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    /// Whether a pumpkin placed at `top_pos` would complete a golem.
    // TODO: The copper golem pattern comes with the copper golem entity.
    #[must_use]
    pub fn can_spawn_golem(world: &World, top_pos: BlockPos) -> bool {
        SNOW_GOLEM_BASE.find(world, top_pos).is_some()
            || IRON_GOLEM_BASE.find(world, top_pos).is_some()
    }

    fn try_spawn_golem(world: &Arc<World>, top_pos: BlockPos) {
        if let Some(found) = SNOW_GOLEM_FULL.find(world, top_pos) {
            let spawn_pos = found.block(0, 2, 0);
            let golem: SharedEntity = Arc::new(SnowGolemEntity::new(
                &vanilla_entities::SNOW_GOLEM,
                next_entity_id(),
                golem_spawn_position(spawn_pos),
                Arc::downgrade(world),
            ));
            spawn_golem_in_world(world, &found, golem);
            return;
        }

        if let Some(found) = IRON_GOLEM_FULL.find(world, top_pos) {
            let spawn_pos = found.block(1, 2, 0);
            let golem = IronGolemEntity::new(
                &vanilla_entities::IRON_GOLEM,
                next_entity_id(),
                golem_spawn_position(spawn_pos),
                Arc::downgrade(world),
            );
            golem.set_player_created(true);
            spawn_golem_in_world(world, &found, Arc::new(golem));
        }
    }
}

/// Vanilla `snapTo(x + 0.5, y + 0.05, z + 0.5, 0, 0)` of the golem's bottom block.
fn golem_spawn_position(pos: BlockPos) -> DVec3 {
    DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()) + 0.05,
        f64::from(pos.z()) + 0.5,
    )
}

/// Vanilla `CarvedPumpkinBlock.spawnGolemInWorld`.
// TODO: Trigger `CriteriaTriggers.SUMMONED_ENTITY` for players within 5 blocks once
// Steel has advancement triggers.
fn spawn_golem_in_world(world: &Arc<World>, found: &BlockPatternMatch, golem: SharedEntity) {
    found.clear_blocks(world);
    golem.set_rotation((0.0, 0.0));
    golem.base().set_old_rotation_to_current();
    if let Some(living) = golem.as_living_entity() {
        living.set_y_head_rot(0.0);
        living.set_y_body_rot(0.0);
    }
    if let Err(error) = world.try_add_entity(golem) {
        log::debug!("world rejected a built golem: {error}");
    }
    found.update_blocks(world);
}

impl BlockBehavior for CarvedPumpkinBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(
            self.block
                .default_state()
                .set_value(HORIZONTAL_FACING, context.horizontal_direction().opposite()),
        )
    }

    fn on_place(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        if old_state.get_block() != state.get_block() {
            Self::try_spawn_golem(world, pos);
        }
    }
}
