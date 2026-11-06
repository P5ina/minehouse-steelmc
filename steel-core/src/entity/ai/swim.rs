//! Vanilla `SwimNodeEvaluator`: paths through water for water-bound mobs.

use rustc_hash::FxHashMap;
use steel_math::fast_floor;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_utils::{BlockPos, Direction};

use crate::behavior::BlockStateBehaviorExt as _;
use crate::entity::ai::node::{Node, NodeStore};
use crate::entity::ai::node_evaluator::{NodeEvaluator, NodeNeighbors};
use crate::entity::ai::path::{PathComputationType, PathType, PathfindingContext};
use crate::entity::ai::walk::{MobPathSettings, WalkNodeCollision};
use crate::fluid::FluidStateExt as _;

/// Vanilla `Direction.values()` order.
const ALL_DIRECTIONS: [Direction; 6] = [
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
    Direction::West,
    Direction::East,
];

/// Vanilla `Direction.Plane.HORIZONTAL` order, which `getClockWise` walks.
const HORIZONTAL_DIRECTIONS: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// Swim node evaluator. Only water nodes are accepted, plus breach nodes
/// (air above water) when `allow_breaching` is set, as for dolphins.
#[derive(Debug, Clone)]
pub struct SwimNodeEvaluator {
    settings: MobPathSettings,
    allow_breaching: bool,
    nodes: NodeStore,
    path_types_by_pos: FxHashMap<BlockPos, PathType>,
}

impl SwimNodeEvaluator {
    #[must_use]
    pub fn new(settings: MobPathSettings, allow_breaching: bool) -> Self {
        Self {
            settings,
            allow_breaching,
            nodes: NodeStore::new(),
            path_types_by_pos: FxHashMap::default(),
        }
    }

    /// Classifies the mob-sized box whose minimum corner is at `x, y, z`.
    #[must_use]
    pub fn get_path_type_of_mob(
        &self,
        context: &PathfindingContext<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> PathType {
        let mut pos = BlockPos::new(x, y, z);
        for xx in x..x + self.settings.entity_width() {
            for yy in y..y + self.settings.entity_height() {
                for zz in z..z + self.settings.entity_depth() {
                    pos = BlockPos::new(xx, yy, zz);
                    let block_state = context.get_block_state(pos);
                    let fluid_state = block_state.get_fluid_state();
                    let below_state = context.get_block_state(pos.below());
                    if fluid_state.is_empty()
                        && below_state.is_pathfindable(PathComputationType::Water)
                        && block_state.is_air()
                    {
                        return PathType::Breach;
                    }

                    if !fluid_state.is_water() {
                        return PathType::Blocked;
                    }
                }
            }
        }

        // Vanilla reads the last position the loop visited.
        if context
            .get_block_state(pos)
            .is_pathfindable(PathComputationType::Water)
        {
            PathType::Water
        } else {
            PathType::Blocked
        }
    }

    fn get_cached_block_type(
        &mut self,
        context: &PathfindingContext<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> PathType {
        let pos = BlockPos::new(x, y, z);
        if let Some(path_type) = self.path_types_by_pos.get(&pos) {
            return *path_type;
        }
        let path_type = self.get_path_type_of_mob(context, x, y, z);
        self.path_types_by_pos.insert(pos, path_type);
        path_type
    }

    fn find_accepted_node(
        &mut self,
        context: &PathfindingContext<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<i32> {
        let path_type = self.get_cached_block_type(context, x, y, z);
        if !(self.allow_breaching && path_type == PathType::Breach || path_type == PathType::Water)
        {
            return None;
        }

        let path_cost = self.settings.pathfinding_malus(path_type);
        if path_cost < 0.0 {
            return None;
        }

        let no_fluid = context
            .get_block_state(BlockPos::new(x, y, z))
            .get_fluid_state()
            .is_empty();
        let node = self.nodes.get_node(x, y, z);
        node.path_type = path_type;
        node.cost_malus = node.cost_malus.max(path_cost);
        if no_fluid {
            node.cost_malus += 8.0;
        }
        Some(node.hash())
    }

    fn is_node_valid(&self, node: Option<i32>) -> bool {
        node.and_then(|hash| self.node(hash))
            .is_some_and(|node| !node.closed)
    }

    fn has_malus(&self, node: Option<i32>) -> bool {
        node.and_then(|hash| self.node(hash))
            .is_some_and(|node| node.cost_malus >= 0.0)
    }
}

impl NodeEvaluator for SwimNodeEvaluator {
    fn node(&self, hash: i32) -> Option<&Node> {
        self.nodes.get(hash)
    }

    fn node_mut(&mut self, hash: i32) -> Option<&mut Node> {
        self.nodes.get_mut(hash)
    }

    fn nodes_mut(&mut self) -> &mut NodeStore {
        &mut self.nodes
    }

    fn reset_search_state(&mut self) {
        self.nodes.reset_search_state();
    }

    fn get_start(&mut self, _context: &mut PathfindingContext<'_>) -> i32 {
        let bounding_box = self.settings.bounding_box();
        self.nodes
            .get_node(
                fast_floor(bounding_box.min_x()),
                fast_floor(bounding_box.min_y() + 0.5),
                fast_floor(bounding_box.min_z()),
            )
            .hash()
    }

    fn get_neighbors(
        &mut self,
        context: &mut PathfindingContext<'_>,
        _collision: &mut impl WalkNodeCollision,
        pos_hash: i32,
    ) -> NodeNeighbors {
        let mut neighbors = NodeNeighbors::new();
        let Some(pos) = self.node(pos_hash) else {
            return neighbors;
        };
        let (pos_x, pos_y, pos_z) = (pos.x, pos.y, pos.z);

        let mut by_direction = [None; 6];
        for (index, direction) in ALL_DIRECTIONS.iter().copied().enumerate() {
            let (step_x, step_y, step_z) = direction.offset();
            let node =
                self.find_accepted_node(context, pos_x + step_x, pos_y + step_y, pos_z + step_z);
            by_direction[index] = node;
            if self.is_node_valid(node)
                && let Some(node) = node
            {
                neighbors.push(node);
            }
        }

        for direction in HORIZONTAL_DIRECTIONS {
            let second_direction = direction.rotate_y_clockwise();
            let first = by_direction[direction_index(direction)];
            let second = by_direction[direction_index(second_direction)];
            if !self.has_malus(first) || !self.has_malus(second) {
                continue;
            }

            let (step_x, _, step_z) = direction.offset();
            let (second_step_x, _, second_step_z) = second_direction.offset();
            let diagonal = self.find_accepted_node(
                context,
                pos_x + step_x + second_step_x,
                pos_y,
                pos_z + step_z + second_step_z,
            );
            if self.is_node_valid(diagonal)
                && let Some(diagonal) = diagonal
            {
                neighbors.push(diagonal);
            }
        }

        neighbors
    }
}

/// Index into [`ALL_DIRECTIONS`], which follows the 3D data value order.
const fn direction_index(direction: Direction) -> usize {
    direction.get_3d_data_value() as usize
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::{BlockPos, BlockStateId, WorldAabb};
    use steel_worldgen::density_functions::overworld::OverworldNoiseSettings;

    use super::SwimNodeEvaluator;
    use crate::behavior::init_behaviors;
    use crate::entity::ai::node::Node;
    use crate::entity::ai::path::{PathType, PathfindingContext, PathfindingMalus};
    use crate::entity::ai::pathfinder::{PathFinder, PathRequest};
    use crate::entity::ai::walk::MobPathSettings;
    use crate::world::LevelReader;

    /// Water at y 64..=65 for x, z in 0..=4, air above, stone everywhere else.
    struct Pond {
        walls: Vec<BlockPos>,
    }

    impl LevelReader for Pond {
        fn get_block_state(&self, pos: BlockPos) -> BlockStateId {
            let inside = (0..=4).contains(&pos.x()) && (0..=4).contains(&pos.z());
            if !inside || pos.y() < 64 || self.walls.contains(&pos) {
                vanilla_blocks::STONE.default_state()
            } else if pos.y() <= 65 {
                vanilla_blocks::WATER.default_state()
            } else {
                vanilla_blocks::AIR.default_state()
            }
        }

        fn raw_brightness(&self, _pos: BlockPos, _sky_darkening: u8) -> u8 {
            0
        }

        fn min_y(&self) -> i32 {
            OverworldNoiseSettings::MIN_Y
        }

        fn height(&self) -> i32 {
            OverworldNoiseSettings::HEIGHT
        }

        fn sea_level(&self) -> i32 {
            OverworldNoiseSettings::SEA_LEVEL
        }
    }

    fn fish_settings() -> MobPathSettings {
        let mut malus = PathfindingMalus::new();
        malus.set(PathType::Water, 0.0);
        MobPathSettings::new(1, 1, 1, BlockPos::new(0, 64, 0), &malus)
    }

    fn find(level: &Pond, allow_breaching: bool, target: BlockPos) -> Option<Vec<BlockPos>> {
        let mut context = PathfindingContext::new(level, BlockPos::new(0, 64, 0));
        let mut evaluator = SwimNodeEvaluator::new(fish_settings(), allow_breaching);
        let mut no_collision = |_aabb: WorldAabb| false;
        let path = PathFinder::new(512).find_path(
            &mut evaluator,
            &mut context,
            &mut no_collision,
            PathRequest {
                targets: &[target],
                max_path_length: 32.0,
                reach_range: 0,
                max_visited_nodes_multiplier: 1.0,
            },
        )?;
        path.can_reach()
            .then(|| path.nodes().iter().map(Node::as_block_pos).collect())
    }

    #[test]
    fn swims_around_a_wall() {
        init_vanilla_registry();
        init_behaviors();

        let walls = (0..=3)
            .flat_map(|z| [BlockPos::new(2, 64, z), BlockPos::new(2, 65, z)])
            .collect();
        let level = Pond { walls };
        let Some(nodes) = find(&level, false, BlockPos::new(4, 64, 0)) else {
            panic!("a path around the wall should exist");
        };

        assert!(
            nodes.contains(&BlockPos::new(2, 64, 4)) || nodes.contains(&BlockPos::new(2, 65, 4))
        );
        assert!(nodes.iter().all(|pos| pos.y() <= 65));
    }

    #[test]
    fn only_breaching_mobs_path_into_the_air_above_water() {
        init_vanilla_registry();
        init_behaviors();

        let level = Pond { walls: Vec::new() };
        let above_surface = BlockPos::new(3, 66, 3);

        assert!(find(&level, false, above_surface).is_none());
        let Some(nodes) = find(&level, true, above_surface) else {
            panic!("a breaching mob should reach the air above the water");
        };
        assert_eq!(nodes.last(), Some(&above_surface));
    }
}
