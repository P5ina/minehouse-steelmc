//! Vanilla `FlyNodeEvaluator`: paths through the air for flying mobs such as
//! parrots, bees and allays.

use rustc_hash::FxHashMap;
use steel_math::fast_floor;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_blocks;
use steel_utils::BlockPos;

use crate::entity::ai::node::{Node, NodeStore};
use crate::entity::ai::node_evaluator::{NodeEvaluator, NodeNeighbors};
use crate::entity::ai::path::{PathType, PathfindingContext};
use crate::entity::ai::walk::{
    MobPathSettings, WalkNodeCollision, WalkNodeEvaluator, WalkPathEvaluator,
};

/// Vanilla `SMALL_MOB_SIZE`.
const SMALL_MOB_SIZE: f64 = 1.0;
/// Vanilla `SMALL_MOB_INFLATED_START_NODE_BOUNDING_BOX`, a float widened to double.
const SMALL_MOB_INFLATED_START_NODE_BOUNDING_BOX: f32 = 1.1;
/// Vanilla `MAX_START_NODE_CANDIDATES`.
const MAX_START_NODE_CANDIDATES: usize = 10;

/// Fly node evaluator. It builds on the walk evaluator, as vanilla's
/// `FlyNodeEvaluator extends WalkNodeEvaluator`, but searches all 26
/// neighbors and classifies blocks with [`FlyNodeEvaluator::path_type`].
#[derive(Debug, Clone)]
pub struct FlyNodeEvaluator {
    walk: WalkNodeEvaluator,
    path_type_by_pos: FxHashMap<BlockPos, PathType>,
}

impl FlyNodeEvaluator {
    #[must_use]
    pub fn new(settings: MobPathSettings) -> Self {
        Self {
            walk: WalkNodeEvaluator::with_path_type(settings, Self::path_type),
            path_type_by_pos: FxHashMap::default(),
        }
    }

    const fn settings(&self) -> &MobPathSettings {
        self.walk.settings()
    }

    /// Vanilla `FlyNodeEvaluator.getPathType`: open air above the ground is
    /// walkable, and the block below decides whether the air is dangerous.
    #[must_use]
    pub fn path_type(context: &mut PathfindingContext<'_>, x: i32, y: i32, z: i32) -> PathType {
        let mut block_path_type = context.get_path_type_from_state(x, y, z);
        if block_path_type == PathType::Open && y > context.level().min_y() {
            let below_pos = BlockPos::new(x, y - 1, z);
            let below_type = context.get_path_type_from_state(x, y - 1, z);
            block_path_type = match below_type {
                PathType::Fire | PathType::Lava => PathType::Fire,
                PathType::Damaging => PathType::Damaging,
                PathType::Cocoa => PathType::Cocoa,
                PathType::Fence => {
                    if below_pos == context.mob_position() {
                        PathType::Open
                    } else {
                        PathType::Fence
                    }
                }
                PathType::Walkable | PathType::Open | PathType::Water => PathType::Open,
                _ => PathType::Walkable,
            };
        }

        if matches!(block_path_type, PathType::Walkable | PathType::Open) {
            block_path_type =
                WalkPathEvaluator::check_neighbour_blocks(context, x, y, z, block_path_type);
        }

        block_path_type
    }

    /// Vanilla `getCachedPathType`: the mob-sized path type, cached per search.
    fn get_cached_path_type(
        &mut self,
        context: &mut PathfindingContext<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> PathType {
        let pos = BlockPos::new(x, y, z);
        if let Some(path_type) = self.path_type_by_pos.get(&pos) {
            return *path_type;
        }
        let path_type = self.walk.get_path_type_of_mob(context, x, y, z);
        self.path_type_by_pos.insert(pos, path_type);
        path_type
    }

    fn can_start_at(&mut self, context: &mut PathfindingContext<'_>, pos: BlockPos) -> bool {
        let path_type = self.get_cached_path_type(context, pos.x(), pos.y(), pos.z());
        self.settings().pathfinding_malus(path_type) >= 0.0
    }

    /// Vanilla `findAcceptedNode`.
    fn find_accepted_node(
        &mut self,
        context: &mut PathfindingContext<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<i32> {
        let path_type = self.get_cached_path_type(context, x, y, z);
        let path_cost = self.settings().pathfinding_malus(path_type);
        if path_cost < 0.0 {
            return None;
        }

        let node = self.walk.nodes_mut().get_node(x, y, z);
        node.path_type = path_type;
        node.cost_malus = node.cost_malus.max(path_cost);
        if path_type == PathType::Walkable {
            node.cost_malus += 1.0;
        }
        Some(node.hash())
    }

    fn has_malus(&self, node: Option<i32>) -> bool {
        node.and_then(|hash| self.node(hash))
            .is_some_and(|node| node.cost_malus >= 0.0)
    }

    fn is_open(&self, node: Option<i32>) -> bool {
        node.and_then(|hash| self.node(hash))
            .is_some_and(|node| !node.closed)
    }

    /// Vanilla `iteratePathfindingStartNodeCandidatePositions`, consumed until
    /// the first position the mob can start at. Random candidates are rolled
    /// lazily, as vanilla's iterator does.
    fn first_startable_candidate(
        &mut self,
        context: &mut PathfindingContext<'_>,
    ) -> Option<BlockPos> {
        let bounding_box = self.settings().bounding_box();
        let mob_block_y = self.settings().mob_position().y();
        if bounding_box.size() >= SMALL_MOB_SIZE {
            let corners = [
                BlockPos::containing(
                    bounding_box.min_x(),
                    f64::from(mob_block_y),
                    bounding_box.min_z(),
                ),
                BlockPos::containing(
                    bounding_box.min_x(),
                    f64::from(mob_block_y),
                    bounding_box.max_z(),
                ),
                BlockPos::containing(
                    bounding_box.max_x(),
                    f64::from(mob_block_y),
                    bounding_box.min_z(),
                ),
                BlockPos::containing(
                    bounding_box.max_x(),
                    f64::from(mob_block_y),
                    bounding_box.max_z(),
                ),
            ];
            return corners
                .into_iter()
                .find(|pos| self.can_start_at(context, *pos));
        }

        let inflated_size = f64::from(SMALL_MOB_INFLATED_START_NODE_BOUNDING_BOX);
        let z_padding = (inflated_size - bounding_box.depth()).max(0.0);
        let x_padding = (inflated_size - bounding_box.width()).max(0.0);
        let y_padding = (inflated_size - bounding_box.height()).max(0.0);
        let inflated = bounding_box.inflate_xyz(x_padding, y_padding, z_padding);
        let min_x = fast_floor(inflated.min_x());
        let min_y = fast_floor(inflated.min_y());
        let min_z = fast_floor(inflated.min_z());
        let max_x = fast_floor(inflated.max_x());
        let max_y = fast_floor(inflated.max_y());
        let max_z = fast_floor(inflated.max_z());
        for _ in 0..MAX_START_NODE_CANDIDATES {
            // Vanilla `BlockPos.randomBetweenClosed`: x, then y, then z.
            let x = min_x + rand::random_range(0..=max_x - min_x);
            let y = min_y + rand::random_range(0..=max_y - min_y);
            let z = min_z + rand::random_range(0..=max_z - min_z);
            let pos = BlockPos::new(x, y, z);
            if self.can_start_at(context, pos) {
                return Some(pos);
            }
        }
        None
    }
}

impl NodeEvaluator for FlyNodeEvaluator {
    fn node(&self, hash: i32) -> Option<&Node> {
        self.walk.node(hash)
    }

    fn node_mut(&mut self, hash: i32) -> Option<&mut Node> {
        self.walk.node_mut(hash)
    }

    fn nodes_mut(&mut self) -> &mut NodeStore {
        self.walk.nodes_mut()
    }

    fn reset_search_state(&mut self) {
        self.walk.reset_search_state();
        self.path_type_by_pos.clear();
    }

    fn get_start(&mut self, context: &mut PathfindingContext<'_>) -> i32 {
        let position = self.settings().mob_position_vec();
        let start_y = if self.settings().can_float() && self.settings().in_water() {
            let mut start_y = self.settings().mob_position().y();
            while context
                .get_block_state(BlockPos::containing(
                    position.x,
                    f64::from(start_y),
                    position.z,
                ))
                .get_block()
                == &vanilla_blocks::WATER
            {
                start_y += 1;
            }
            start_y
        } else {
            fast_floor(position.y + 0.5)
        };

        let start_pos = BlockPos::containing(position.x, f64::from(start_y), position.z);
        if !self.can_start_at(context, start_pos)
            && let Some(candidate) = self.first_startable_candidate(context)
        {
            return self.walk.get_start_node(context, candidate);
        }

        self.walk.get_start_node(context, start_pos)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "vanilla checks all 26 neighbors in order"
    )]
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
        let (x, y, z) = (pos.x, pos.y, pos.z);

        let south = self.find_accepted_node(context, x, y, z + 1);
        let west = self.find_accepted_node(context, x - 1, y, z);
        let east = self.find_accepted_node(context, x + 1, y, z);
        let north = self.find_accepted_node(context, x, y, z - 1);
        let up = self.find_accepted_node(context, x, y + 1, z);
        let down = self.find_accepted_node(context, x, y - 1, z);
        // Vanilla evaluates each neighbor and pushes it before computing the
        // next, so `closed` and `cost_malus` are read in this same order.
        let mut push = |evaluator: &Self, node: Option<i32>, condition: bool| {
            if condition
                && evaluator.is_open(node)
                && let Some(node) = node
            {
                neighbors.push(node);
            }
        };
        push(self, south, true);
        push(self, west, true);
        push(self, east, true);
        push(self, north, true);
        push(self, up, true);
        push(self, down, true);

        let south_up = self.find_accepted_node(context, x, y + 1, z + 1);
        push(self, south_up, self.has_malus(south) && self.has_malus(up));
        let west_up = self.find_accepted_node(context, x - 1, y + 1, z);
        push(self, west_up, self.has_malus(west) && self.has_malus(up));
        let east_up = self.find_accepted_node(context, x + 1, y + 1, z);
        push(self, east_up, self.has_malus(east) && self.has_malus(up));
        let north_up = self.find_accepted_node(context, x, y + 1, z - 1);
        push(self, north_up, self.has_malus(north) && self.has_malus(up));
        let south_down = self.find_accepted_node(context, x, y - 1, z + 1);
        push(
            self,
            south_down,
            self.has_malus(south) && self.has_malus(down),
        );
        let west_down = self.find_accepted_node(context, x - 1, y - 1, z);
        push(
            self,
            west_down,
            self.has_malus(west) && self.has_malus(down),
        );
        let east_down = self.find_accepted_node(context, x + 1, y - 1, z);
        push(
            self,
            east_down,
            self.has_malus(east) && self.has_malus(down),
        );
        let north_down = self.find_accepted_node(context, x, y - 1, z - 1);
        push(
            self,
            north_down,
            self.has_malus(north) && self.has_malus(down),
        );
        let north_east = self.find_accepted_node(context, x + 1, y, z - 1);
        push(
            self,
            north_east,
            self.has_malus(north) && self.has_malus(east),
        );
        let south_east = self.find_accepted_node(context, x + 1, y, z + 1);
        push(
            self,
            south_east,
            self.has_malus(south) && self.has_malus(east),
        );
        let north_west = self.find_accepted_node(context, x - 1, y, z - 1);
        push(
            self,
            north_west,
            self.has_malus(north) && self.has_malus(west),
        );
        let south_west = self.find_accepted_node(context, x - 1, y, z + 1);
        push(
            self,
            south_west,
            self.has_malus(south) && self.has_malus(west),
        );

        let corners = [
            (1, 1, -1, north_east, north, east, up, north_up, east_up),
            (1, 1, 1, south_east, south, east, up, south_up, east_up),
            (-1, 1, -1, north_west, north, west, up, north_up, west_up),
            (-1, 1, 1, south_west, south, west, up, south_up, west_up),
            (
                1, -1, -1, north_east, north, east, down, north_down, east_down,
            ),
            (
                1, -1, 1, south_east, south, east, down, south_down, east_down,
            ),
            (
                -1, -1, -1, north_west, north, west, down, north_down, west_down,
            ),
            (
                -1, -1, 1, south_west, south, west, down, south_down, west_down,
            ),
        ];
        for (dx, dy, dz, horizontal, side_z, side_x, vertical, vertical_z, vertical_x) in corners {
            let corner = self.find_accepted_node(context, x + dx, y + dy, z + dz);
            let condition = [horizontal, side_z, side_x, vertical, vertical_z, vertical_x]
                .into_iter()
                .all(|node| self.has_malus(node));
            push(self, corner, condition);
        }

        neighbors
    }
}

#[cfg(test)]
mod tests;
