//! Shared node evaluator interface used by the A* search.

use crate::entity::ai::node::{Node, NodeStore};
use crate::entity::ai::path::PathfindingContext;
use crate::entity::ai::walk::WalkNodeCollision;

/// Vanilla `NodeEvaluator`: produces start and neighbor nodes for the
/// [`PathFinder`](crate::entity::ai::pathfinder::PathFinder).
pub trait NodeEvaluator {
    /// Returns the node with the given hash.
    fn node(&self, hash: i32) -> Option<&Node>;

    /// Returns the node with the given hash mutably.
    fn node_mut(&mut self, hash: i32) -> Option<&mut Node>;

    /// Returns the node store the search heap indexes into.
    fn nodes_mut(&mut self) -> &mut NodeStore;

    /// Clears per-search node state before a new search.
    fn reset_search_state(&mut self);

    /// Returns the hash of the node the path starts from.
    fn get_start(&mut self, context: &mut PathfindingContext<'_>) -> i32;

    /// Returns the nodes reachable from `pos_hash`.
    fn get_neighbors(
        &mut self,
        context: &mut PathfindingContext<'_>,
        collision: &mut impl WalkNodeCollision,
        pos_hash: i32,
    ) -> NodeNeighbors;
}

/// Neighbor node hashes returned by a [`NodeEvaluator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeNeighbors {
    nodes: [i32; Self::CAPACITY],
    len: usize,
}

impl NodeNeighbors {
    /// Vanilla's neighbor array holds 32 nodes; walking yields at most 8 and
    /// swimming at most 10.
    const CAPACITY: usize = 10;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            nodes: [0; Self::CAPACITY],
            len: 0,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        self.nodes[..self.len].iter().copied()
    }

    pub(crate) const fn push(&mut self, node: i32) {
        self.nodes[self.len] = node;
        self.len += 1;
    }
}

impl Default for NodeNeighbors {
    fn default() -> Self {
        Self::new()
    }
}
