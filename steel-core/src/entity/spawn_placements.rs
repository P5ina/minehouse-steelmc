//! Entity-type spawn placement dispatch.

use rand::Rng;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::fluid::FluidStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{vanilla_blocks, vanilla_entities};
use steel_utils::{BlockPos, BlockStateId, WorldAabb, types::Difficulty};

use crate::behavior::BLOCK_BEHAVIORS;
use crate::entity::ai::walk::WalkPathEvaluator;
use crate::entity::{ENTITIES, EntitySpawnReason};
use crate::world::{SignalQueryContext, World, is_redstone_conductor};

/// Vanilla `SpawnPlacementTypes`: where a mob may stand when it spawns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnPlacementType {
    /// Any position.
    NoRestrictions,
    /// Inside water, below a block that does not conduct redstone.
    InWater,
    /// Inside lava.
    InLava,
    /// On a valid spawn block, with two free blocks above it.
    OnGround,
}

impl SpawnPlacementType {
    /// Vanilla `SpawnPlacementType.isSpawnPositionOk`.
    pub(crate) fn is_spawn_position_ok(
        self,
        world: &World,
        pos: BlockPos,
        entity_type: EntityTypeRef,
    ) -> bool {
        match self {
            Self::NoRestrictions => true,
            Self::InWater => {
                if !world.is_block_within_world_border(pos) {
                    return false;
                }
                let above = pos.above();
                world.get_block_state(pos).get_fluid_state().is_water()
                    && !is_redstone_conductor(world, world.get_block_state(above), above)
            }
            Self::InLava => {
                world.is_block_within_world_border(pos)
                    && world.get_block_state(pos).get_fluid_state().is_lava()
            }
            Self::OnGround => {
                if !world.is_block_within_world_border(pos) {
                    return false;
                }
                let below = pos.below();
                let below_state = world.get_block_state(below);
                BLOCK_BEHAVIORS
                    .get_behavior(below_state.get_block())
                    .is_valid_spawn(below_state, world, below, entity_type)
                    && is_valid_empty_spawn_block(world, pos, entity_type)
                    && is_valid_empty_spawn_block(world, pos.above(), entity_type)
            }
        }
    }
}

/// Vanilla `NaturalSpawner.isValidEmptySpawnBlock` for the state at `pos`.
pub(crate) fn is_valid_empty_spawn_block(
    world: &World,
    pos: BlockPos,
    entity_type: EntityTypeRef,
) -> bool {
    let state = world.get_block_state(pos);
    !world.is_collision_shape_full_block_at(pos, state)
        && !BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .is_signal_source(state, SignalQueryContext::DEFAULT)
        && state.get_fluid_state().is_empty()
        && !state
            .get_block()
            .has_tag(&BlockTag::PREVENT_MOB_SPAWNING_INSIDE)
        && !is_block_dangerous(entity_type, state)
}

/// Vanilla `EntityType.isBlockDangerous`.
pub(crate) fn is_block_dangerous(entity_type: EntityTypeRef, state: BlockStateId) -> bool {
    let block = state.get_block();
    if immune_to(entity_type).is_some_and(|tag| block.has_tag(&tag)) {
        return false;
    }
    (!entity_type.fire_immune && WalkPathEvaluator::is_burning_block(state))
        || block == &vanilla_blocks::WITHER_ROSE
        || block == &vanilla_blocks::SWEET_BERRY_BUSH
        || block == &vanilla_blocks::CACTUS
        || block == &vanilla_blocks::POWDER_SNOW
}

/// Vanilla `EntityType.Builder.immuneTo` tags from `EntityTypes`.
fn immune_to(entity_type: EntityTypeRef) -> Option<steel_utils::Identifier> {
    Some(if entity_type == &vanilla_entities::FOX {
        BlockTag::FOX_IMMUNE_TO
    } else if entity_type == &vanilla_entities::POLAR_BEAR {
        BlockTag::POLAR_BEAR_IMMUNE_TO
    } else if entity_type == &vanilla_entities::SNOW_GOLEM {
        BlockTag::SNOW_GOLEM_IMMUNE_TO
    } else if entity_type == &vanilla_entities::STRAY {
        BlockTag::STRAY_IMMUNE_TO
    } else if entity_type == &vanilla_entities::WITHER {
        BlockTag::WITHER_IMMUNE_TO
    } else if entity_type == &vanilla_entities::WITHER_SKELETON {
        BlockTag::WITHER_SKELETON_IMMUNE_TO
    } else {
        return None;
    })
}

/// Vanilla `EntityType.getSpawnAABB`: the entity's box centered on `x`/`z`.
#[must_use]
pub(crate) fn spawn_aabb(entity_type: EntityTypeRef, x: f64, y: f64, z: f64) -> WorldAabb {
    let dimensions = entity_type.dimensions;
    WorldAabb::entity_box(
        x,
        y,
        z,
        f64::from(dimensions.half_width()),
        f64::from(dimensions.height),
    )
}

/// Vanilla `SpawnPlacements`.
///
/// Predicates are each entity's `SPAWN_RULE`, registered by `#[entity_behavior(spawn_rule)]`.
/// Placement types and heightmaps mirror the static table in vanilla's `SpawnPlacements`.
pub(crate) struct SpawnPlacements;

impl SpawnPlacements {
    /// Vanilla `getPlacementType`; unregistered types have no restrictions.
    #[must_use]
    pub(crate) fn placement_type(entity_type: EntityTypeRef) -> SpawnPlacementType {
        const IN_WATER: [EntityTypeRef; 12] = [
            &vanilla_entities::AXOLOTL,
            &vanilla_entities::COD,
            &vanilla_entities::DOLPHIN,
            &vanilla_entities::DROWNED,
            &vanilla_entities::GUARDIAN,
            &vanilla_entities::PUFFERFISH,
            &vanilla_entities::SALMON,
            &vanilla_entities::SQUID,
            &vanilla_entities::TROPICAL_FISH,
            &vanilla_entities::GLOW_SQUID,
            &vanilla_entities::NAUTILUS,
            &vanilla_entities::ELDER_GUARDIAN,
        ];
        const ON_GROUND: [EntityTypeRef; 60] = [
            &vanilla_entities::ARMADILLO,
            &vanilla_entities::BAT,
            &vanilla_entities::BLAZE,
            &vanilla_entities::BOGGED,
            &vanilla_entities::BREEZE,
            &vanilla_entities::CAMEL,
            &vanilla_entities::CAMEL_HUSK,
            &vanilla_entities::CAVE_SPIDER,
            &vanilla_entities::CHICKEN,
            &vanilla_entities::COW,
            &vanilla_entities::CREEPER,
            &vanilla_entities::DONKEY,
            &vanilla_entities::ENDERMAN,
            &vanilla_entities::ENDERMITE,
            &vanilla_entities::ENDER_DRAGON,
            &vanilla_entities::FROG,
            &vanilla_entities::GHAST,
            &vanilla_entities::HAPPY_GHAST,
            &vanilla_entities::GIANT,
            &vanilla_entities::GOAT,
            &vanilla_entities::HORSE,
            &vanilla_entities::HUSK,
            &vanilla_entities::IRON_GOLEM,
            &vanilla_entities::LLAMA,
            &vanilla_entities::MAGMA_CUBE,
            &vanilla_entities::SULFUR_CUBE,
            &vanilla_entities::MOOSHROOM,
            &vanilla_entities::MULE,
            &vanilla_entities::OCELOT,
            &vanilla_entities::PARROT,
            &vanilla_entities::PIG,
            &vanilla_entities::HOGLIN,
            &vanilla_entities::PIGLIN,
            &vanilla_entities::PILLAGER,
            &vanilla_entities::POLAR_BEAR,
            &vanilla_entities::RABBIT,
            &vanilla_entities::SHEEP,
            &vanilla_entities::SILVERFISH,
            &vanilla_entities::SKELETON,
            &vanilla_entities::SKELETON_HORSE,
            &vanilla_entities::SLIME,
            &vanilla_entities::SNOW_GOLEM,
            &vanilla_entities::SPIDER,
            &vanilla_entities::STRAY,
            &vanilla_entities::PARCHED,
            &vanilla_entities::TURTLE,
            &vanilla_entities::VILLAGER,
            &vanilla_entities::WITCH,
            &vanilla_entities::WITHER,
            &vanilla_entities::WITHER_SKELETON,
            &vanilla_entities::WOLF,
            &vanilla_entities::ZOGLIN,
            &vanilla_entities::CREAKING,
            &vanilla_entities::ZOMBIE,
            &vanilla_entities::ZOMBIE_HORSE,
            &vanilla_entities::ZOMBIFIED_PIGLIN,
            &vanilla_entities::ZOMBIE_VILLAGER,
            &vanilla_entities::CAT,
            &vanilla_entities::RAVAGER,
            &vanilla_entities::WANDERING_TRADER,
        ];

        if IN_WATER.contains(&entity_type) {
            SpawnPlacementType::InWater
        } else if entity_type == &vanilla_entities::STRIDER {
            SpawnPlacementType::InLava
        } else if ON_GROUND.contains(&entity_type) {
            SpawnPlacementType::OnGround
        } else {
            SpawnPlacementType::NoRestrictions
        }
    }

    /// Vanilla `isSpawnPositionOk`.
    #[must_use]
    pub(crate) fn is_spawn_position_ok(
        entity_type: EntityTypeRef,
        world: &World,
        pos: BlockPos,
    ) -> bool {
        Self::placement_type(entity_type).is_spawn_position_ok(world, pos, entity_type)
    }

    /// Vanilla `checkSpawnRules`: the peaceful gate, then the type's predicate.
    pub(crate) fn check_spawn_rules(
        entity_type: EntityTypeRef,
        world: &World,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        random: &mut dyn Rng,
    ) -> bool {
        if !entity_type.allowed_in_peaceful && world.difficulty() == Difficulty::Peaceful {
            return false;
        }

        ENTITIES
            .spawn_rule(entity_type)
            .is_none_or(|rule| rule(entity_type, world, spawn_reason, pos, random))
    }

    /// Checks the vanilla placement predicate used by normal mob spawners.
    pub(crate) fn check_spawner_spawn_rules(
        entity_type: EntityTypeRef,
        world: &World,
        pos: BlockPos,
        random: &mut dyn Rng,
    ) -> bool {
        Self::check_spawn_rules(entity_type, world, EntitySpawnReason::Spawner, pos, random)
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos};

    use super::{SpawnPlacementType, SpawnPlacements, is_block_dangerous};
    use crate::behavior::init_behaviors;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn placement_types_follow_vanilla_table() {
        assert_eq!(
            SpawnPlacements::placement_type(&vanilla_entities::ZOMBIE),
            SpawnPlacementType::OnGround
        );
        assert_eq!(
            SpawnPlacements::placement_type(&vanilla_entities::COD),
            SpawnPlacementType::InWater
        );
        assert_eq!(
            SpawnPlacements::placement_type(&vanilla_entities::STRIDER),
            SpawnPlacementType::InLava
        );
        assert_eq!(
            SpawnPlacements::placement_type(&vanilla_entities::PHANTOM),
            SpawnPlacementType::NoRestrictions
        );
    }

    #[test]
    fn dangerous_blocks_respect_immunity_tags() {
        init_vanilla_registry();
        let berry_bush = vanilla_blocks::SWEET_BERRY_BUSH.default_state();

        assert!(is_block_dangerous(&vanilla_entities::ZOMBIE, berry_bush));
        assert!(!is_block_dangerous(&vanilla_entities::FOX, berry_bush));
        assert!(is_block_dangerous(
            &vanilla_entities::ZOMBIE,
            vanilla_blocks::FIRE.default_state()
        ));
        assert!(!is_block_dangerous(
            &vanilla_entities::BLAZE,
            vanilla_blocks::FIRE.default_state()
        ));
    }

    #[test]
    fn ground_placement_needs_a_floor_and_two_free_blocks() {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world("natural_spawner_ground_placement");
        let world = &world_fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let pos = BlockPos::new(8, 65, 8);
        assert!(world.set_block(
            pos.below(),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));

        assert!(SpawnPlacements::is_spawn_position_ok(
            &vanilla_entities::CREEPER,
            world,
            pos
        ));
        assert!(!SpawnPlacements::is_spawn_position_ok(
            &vanilla_entities::CREEPER,
            world,
            pos.above()
        ));
        assert!(!SpawnPlacements::is_spawn_position_ok(
            &vanilla_entities::COD,
            world,
            pos
        ));

        assert!(world.set_block(
            pos.above(),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        assert!(!SpawnPlacements::is_spawn_position_ok(
            &vanilla_entities::CREEPER,
            world,
            pos
        ));
    }
}
