//! Multiblock shapes matched in any orientation (Vanilla `BlockPattern`).
//!
//! Golems and the wither are built by placing their head on a shape of blocks;
//! the shape may face any direction and be tilted onto any side.

use std::sync::Arc;

use glam::IVec3;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Direction};

use steel_registry::{level_events, vanilla_blocks};

use crate::world::World;

/// Test for one cell of a pattern. `None` matches anything, even an unloaded block
/// (Vanilla's `' '` predicate).
pub(crate) type CellPredicate = Option<BlockPredicate>;

/// Test for a block state in a pattern cell.
pub(crate) type BlockPredicate = fn(BlockStateId) -> bool;

/// A 3D grid of block predicates, indexed `[depth][height][width]`.
pub(crate) struct BlockPattern {
    pattern: Vec<Vec<Vec<CellPredicate>>>,
    depth: i32,
    height: i32,
    width: i32,
}

impl BlockPattern {
    /// Builds a pattern from aisles of rows, like Vanilla `BlockPatternBuilder`.
    ///
    /// Each aisle is one depth slice listed top row first. `' '` matches anything;
    /// every other character must have a predicate in `lookup`.
    ///
    /// # Panics
    /// Panics if the aisles are ragged or a character has no predicate, as the
    /// Vanilla builder throws; patterns are constants, so this is a programming error.
    #[must_use]
    pub(crate) fn new(aisles: &[&[&str]], lookup: &[(char, BlockPredicate)]) -> Self {
        let height = aisles[0].len();
        let width = aisles[0][0].len();
        let pattern = aisles
            .iter()
            .map(|aisle| {
                assert_eq!(aisle.len(), height, "aisle height mismatch");
                aisle
                    .iter()
                    .map(|row| {
                        assert_eq!(row.len(), width, "aisle row width mismatch");
                        row.chars()
                            .map(|c| {
                                if c == ' ' {
                                    return None;
                                }
                                let predicate = lookup
                                    .iter()
                                    .find(|(key, _)| *key == c)
                                    .map(|(_, predicate)| *predicate);
                                assert!(predicate.is_some(), "no predicate for {c:?}");
                                predicate
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect();
        Self {
            pattern,
            depth: aisles.len() as i32,
            height: height as i32,
            width: width as i32,
        }
    }

    /// Finds this pattern with any orientation among the positions within
    /// `max(width, height, depth)` of `origin` in the positive axes.
    pub(crate) fn find(&self, world: &World, origin: BlockPos) -> Option<BlockPatternMatch> {
        let dist = self.width.max(self.height).max(self.depth);
        for test_pos in
            BlockPos::between_closed(origin, origin.offset(dist - 1, dist - 1, dist - 1))
        {
            for forwards in Direction::ALL {
                for up in Direction::ALL {
                    if up == forwards || up == forwards.opposite() {
                        continue;
                    }
                    if let Some(found) = self.matches(world, test_pos, forwards, up) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    fn matches(
        &self,
        world: &World,
        origin: BlockPos,
        forwards: Direction,
        up: Direction,
    ) -> Option<BlockPatternMatch> {
        for x in 0..self.width {
            for y in 0..self.height {
                for z in 0..self.depth {
                    let Some(predicate) = self.pattern[z as usize][y as usize][x as usize] else {
                        continue;
                    };
                    let pos = translate_and_rotate(origin, forwards, up, x, y, z);
                    // Vanilla `BlockInWorld.hasState` fails on unloaded blocks.
                    let state = world.loaded_block_state(pos)?;
                    if !predicate(state) {
                        return None;
                    }
                }
            }
        }
        Some(BlockPatternMatch {
            front_top_left: origin,
            forwards,
            up,
            width: self.width,
            height: self.height,
        })
    }
}

/// Where a pattern matched.
pub(crate) struct BlockPatternMatch {
    front_top_left: BlockPos,
    forwards: Direction,
    up: Direction,
    width: i32,
    height: i32,
}

impl BlockPatternMatch {
    /// The world position of pattern cell (`right`, `down`, `forwards`).
    #[must_use]
    pub(crate) fn block(&self, right: i32, down: i32, forwards: i32) -> BlockPos {
        translate_and_rotate(
            self.front_top_left,
            self.forwards,
            self.up,
            right,
            down,
            forwards,
        )
    }

    /// Vanilla `CarvedPumpkinBlock.clearPatternBlocks`: replaces the front slice
    /// with air, with block-break particles.
    pub(crate) fn clear_blocks(&self, world: &Arc<World>) {
        for x in 0..self.width {
            for y in 0..self.height {
                let pos = self.block(x, y, 0);
                let state = world.get_block_state(pos);
                world.set_block(
                    pos,
                    vanilla_blocks::AIR.default_state(),
                    UpdateFlags::UPDATE_CLIENTS,
                );
                world.level_event(
                    level_events::PARTICLES_DESTROY_BLOCK,
                    pos,
                    level_events::encode_block_state_data(u32::from(state.0)),
                    None,
                );
            }
        }
    }

    /// Vanilla `CarvedPumpkinBlock.updatePatternBlocks`.
    pub(crate) fn update_blocks(&self, world: &Arc<World>) {
        for x in 0..self.width {
            for y in 0..self.height {
                world.update_neighbors_at(self.block(x, y, 0), &vanilla_blocks::AIR);
            }
        }
    }
}

fn translate_and_rotate(
    origin: BlockPos,
    forwards: Direction,
    up: Direction,
    right: i32,
    down: i32,
    forward: i32,
) -> BlockPos {
    let forwards_vec: IVec3 = forwards.offset_vec();
    let up_vec: IVec3 = up.offset_vec();
    let right_vec = forwards_vec.cross(up_vec);
    let offset = up_vec * -down + right_vec * right + forwards_vec * forward;
    origin.offset(offset.x, offset.y, offset.z)
}

#[cfg(test)]
mod tests {
    use steel_utils::{BlockPos, Direction};

    use super::translate_and_rotate;

    #[test]
    fn rotation_follows_vanilla_cross_product() {
        // Facing north with up = up, "right" is east (north × up = -z × y = +x).
        let origin = BlockPos::new(0, 0, 0);
        assert_eq!(
            translate_and_rotate(origin, Direction::North, Direction::Up, 1, 0, 0),
            BlockPos::new(1, 0, 0)
        );
        assert_eq!(
            translate_and_rotate(origin, Direction::North, Direction::Up, 0, 2, 0),
            BlockPos::new(0, -2, 0)
        );
        // Lying down: forwards = up, up = south, so "down" runs north.
        assert_eq!(
            translate_and_rotate(origin, Direction::Up, Direction::South, 0, 1, 0),
            BlockPos::new(0, 0, -1)
        );
    }
}
