//! Vanilla `BlockBehaviour.Properties.isValidSpawn` predicates from `Blocks`.

use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::Direction;
use steel_registry::blocks::shapes::SupportType;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::{vanilla_blocks, vanilla_entities};
use steel_utils::{BlockPos, BlockStateId};

use super::MAX_MOB_SPAWN_LIGHT_EMISSION;
use crate::world::LevelReader;

/// Blocks registered with `isValidSpawn(Blocks::never)`.
static NEVER: [BlockRef; 53] = [
    &vanilla_blocks::BEDROCK,
    &vanilla_blocks::GLASS,
    &vanilla_blocks::TINTED_GLASS,
    &vanilla_blocks::MOVING_PISTON,
    &vanilla_blocks::BARRIER,
    &vanilla_blocks::CHORUS_FLOWER,
    &vanilla_blocks::SCAFFOLDING,
    &vanilla_blocks::WHITE_STAINED_GLASS,
    &vanilla_blocks::ORANGE_STAINED_GLASS,
    &vanilla_blocks::MAGENTA_STAINED_GLASS,
    &vanilla_blocks::LIGHT_BLUE_STAINED_GLASS,
    &vanilla_blocks::YELLOW_STAINED_GLASS,
    &vanilla_blocks::LIME_STAINED_GLASS,
    &vanilla_blocks::PINK_STAINED_GLASS,
    &vanilla_blocks::GRAY_STAINED_GLASS,
    &vanilla_blocks::LIGHT_GRAY_STAINED_GLASS,
    &vanilla_blocks::CYAN_STAINED_GLASS,
    &vanilla_blocks::PURPLE_STAINED_GLASS,
    &vanilla_blocks::BLUE_STAINED_GLASS,
    &vanilla_blocks::BROWN_STAINED_GLASS,
    &vanilla_blocks::GREEN_STAINED_GLASS,
    &vanilla_blocks::RED_STAINED_GLASS,
    &vanilla_blocks::BLACK_STAINED_GLASS,
    &vanilla_blocks::OAK_TRAPDOOR,
    &vanilla_blocks::SPRUCE_TRAPDOOR,
    &vanilla_blocks::BIRCH_TRAPDOOR,
    &vanilla_blocks::JUNGLE_TRAPDOOR,
    &vanilla_blocks::ACACIA_TRAPDOOR,
    &vanilla_blocks::CHERRY_TRAPDOOR,
    &vanilla_blocks::DARK_OAK_TRAPDOOR,
    &vanilla_blocks::PALE_OAK_TRAPDOOR,
    &vanilla_blocks::MANGROVE_TRAPDOOR,
    &vanilla_blocks::POPLAR_TRAPDOOR,
    &vanilla_blocks::BAMBOO_TRAPDOOR,
    &vanilla_blocks::IRON_TRAPDOOR,
    &vanilla_blocks::CRIMSON_TRAPDOOR,
    &vanilla_blocks::WARPED_TRAPDOOR,
    &vanilla_blocks::COPPER_TRAPDOOR,
    &vanilla_blocks::EXPOSED_COPPER_TRAPDOOR,
    &vanilla_blocks::WEATHERED_COPPER_TRAPDOOR,
    &vanilla_blocks::OXIDIZED_COPPER_TRAPDOOR,
    &vanilla_blocks::WAXED_COPPER_TRAPDOOR,
    &vanilla_blocks::WAXED_EXPOSED_COPPER_TRAPDOOR,
    &vanilla_blocks::WAXED_WEATHERED_COPPER_TRAPDOOR,
    &vanilla_blocks::WAXED_OXIDIZED_COPPER_TRAPDOOR,
    &vanilla_blocks::COPPER_GRATE,
    &vanilla_blocks::EXPOSED_COPPER_GRATE,
    &vanilla_blocks::WEATHERED_COPPER_GRATE,
    &vanilla_blocks::OXIDIZED_COPPER_GRATE,
    &vanilla_blocks::WAXED_COPPER_GRATE,
    &vanilla_blocks::WAXED_EXPOSED_COPPER_GRATE,
    &vanilla_blocks::WAXED_WEATHERED_COPPER_GRATE,
    &vanilla_blocks::WAXED_OXIDIZED_COPPER_GRATE,
];

/// Blocks registered with `isValidSpawn(Blocks::always)`.
static ALWAYS: [BlockRef; 5] = [
    &vanilla_blocks::SOUL_SAND,
    &vanilla_blocks::CARVED_PUMPKIN,
    &vanilla_blocks::JACK_O_LANTERN,
    &vanilla_blocks::REDSTONE_LAMP,
    &vanilla_blocks::MUD,
];

/// Leaves (`leavesProperties`, cherry and pale oak) use `Blocks::ocelotOrParrot`.
static OCELOT_OR_PARROT: [BlockRef; 14] = [
    &vanilla_blocks::OAK_LEAVES,
    &vanilla_blocks::SPRUCE_LEAVES,
    &vanilla_blocks::BIRCH_LEAVES,
    &vanilla_blocks::JUNGLE_LEAVES,
    &vanilla_blocks::ACACIA_LEAVES,
    &vanilla_blocks::CHERRY_LEAVES,
    &vanilla_blocks::DARK_OAK_LEAVES,
    &vanilla_blocks::PALE_OAK_LEAVES,
    &vanilla_blocks::MANGROVE_LEAVES,
    &vanilla_blocks::RED_POPLAR_LEAVES,
    &vanilla_blocks::ORANGE_POPLAR_LEAVES,
    &vanilla_blocks::YELLOW_POPLAR_LEAVES,
    &vanilla_blocks::AZALEA_LEAVES,
    &vanilla_blocks::FLOWERING_AZALEA_LEAVES,
];

/// Vanilla `BlockStateBase.isValidSpawn`: the block's registered predicate,
/// or the `Properties` default of a sturdy top face and light emission
/// below [`MAX_MOB_SPAWN_LIGHT_EMISSION`].
pub(crate) fn properties_is_valid_spawn(
    state: BlockStateId,
    world: &dyn LevelReader,
    pos: BlockPos,
    entity_type: EntityTypeRef,
) -> bool {
    let block = state.get_block();
    let is_any = |blocks: &[BlockRef]| blocks.contains(&block);
    if is_any(&NEVER) {
        false
    } else if is_any(&ALWAYS) {
        true
    } else if is_any(&OCELOT_OR_PARROT) {
        entity_type == &vanilla_entities::OCELOT || entity_type == &vanilla_entities::PARROT
    } else if block == &vanilla_blocks::ICE || block == &vanilla_blocks::FROSTED_ICE {
        entity_type == &vanilla_entities::POLAR_BEAR
    } else if block == &vanilla_blocks::MAGMA_BLOCK {
        entity_type.fire_immune
    } else {
        world.is_face_sturdy_for(state, pos, Direction::Up, SupportType::Full)
            && state.get_light_emission() < MAX_MOB_SPAWN_LIGHT_EMISSION
    }
}
