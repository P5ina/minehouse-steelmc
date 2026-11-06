use steel_registry::{init_vanilla_registry, vanilla_blocks};
use steel_utils::{BlockPos, BlockStateId, WorldAabb};
use steel_worldgen::density_functions::overworld::OverworldNoiseSettings;

use super::FlyNodeEvaluator;
use crate::behavior::init_behaviors;
use crate::entity::ai::node::Node;
use crate::entity::ai::path::{PathType, PathfindingContext, PathfindingMalus};
use crate::entity::ai::pathfinder::{PathFinder, PathRequest};
use crate::entity::ai::walk::MobPathSettings;
use crate::world::LevelReader;

/// Stone below y 64, air above, plus extra blocks.
struct Field {
    blocks: Vec<(BlockPos, BlockStateId)>,
}

impl LevelReader for Field {
    fn get_block_state(&self, pos: BlockPos) -> BlockStateId {
        if let Some((_, state)) = self.blocks.iter().find(|(block, _)| *block == pos) {
            return *state;
        }
        if pos.y() < 64 {
            vanilla_blocks::STONE.default_state()
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

fn setup() {
    init_vanilla_registry();
    init_behaviors();
}

fn bird_malus() -> PathfindingMalus {
    let mut malus = PathfindingMalus::new();
    malus.set(PathType::FireInNeighbor, -1.0);
    malus.set(PathType::Fire, -1.0);
    malus.set(PathType::Cocoa, -1.0);
    malus
}

fn find(level: &Field, start: BlockPos, target: BlockPos) -> Option<Vec<BlockPos>> {
    let mut context = PathfindingContext::new(level, start);
    let settings = MobPathSettings::new(1, 1, 1, start, &bird_malus());
    let mut evaluator = FlyNodeEvaluator::new(settings);
    let mut no_collision = |_aabb: WorldAabb| false;
    let path = PathFinder::new(1024).find_path(
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
fn air_over_ground_is_walkable_and_higher_air_is_open() {
    setup();
    let level = Field { blocks: Vec::new() };
    let mut context = PathfindingContext::new(&level, BlockPos::new(0, 70, 0));
    assert_eq!(
        FlyNodeEvaluator::path_type(&mut context, 0, 64, 0),
        PathType::Walkable
    );
    assert_eq!(
        FlyNodeEvaluator::path_type(&mut context, 0, 66, 0),
        PathType::Open
    );
}

#[test]
fn air_above_fire_and_cocoa_is_dangerous() {
    setup();
    let level = Field {
        blocks: vec![
            (
                BlockPos::new(0, 64, 0),
                vanilla_blocks::FIRE.default_state(),
            ),
            (
                BlockPos::new(3, 64, 0),
                vanilla_blocks::COCOA.default_state(),
            ),
        ],
    };
    let mut context = PathfindingContext::new(&level, BlockPos::new(0, 70, 0));
    assert_eq!(
        FlyNodeEvaluator::path_type(&mut context, 0, 65, 0),
        PathType::Fire
    );
    assert_eq!(
        FlyNodeEvaluator::path_type(&mut context, 3, 65, 0),
        PathType::Cocoa
    );
}

#[test]
fn flies_over_a_wall_through_the_air() {
    setup();
    // A wall at x 2 from y 64 up to y 67 across z -3..=3.
    let blocks = (-3..=3)
        .flat_map(|z| (64..=67).map(move |y| BlockPos::new(2, y, z)))
        .map(|pos| (pos, vanilla_blocks::STONE.default_state()))
        .collect();
    let level = Field { blocks };
    let Some(nodes) = find(&level, BlockPos::new(0, 64, 0), BlockPos::new(4, 64, 0)) else {
        panic!("a flying mob should get over or around the wall");
    };
    assert_eq!(nodes.last(), Some(&BlockPos::new(4, 64, 0)));
    assert!(
        nodes.iter().any(|pos| pos.y() > 67 || pos.z().abs() > 3),
        "the path must leave the ground to pass the wall: {nodes:?}"
    );
}

#[test]
fn flies_straight_up_to_a_target_in_the_air() {
    setup();
    let level = Field { blocks: Vec::new() };
    let Some(nodes) = find(&level, BlockPos::new(0, 64, 0), BlockPos::new(0, 72, 0)) else {
        panic!("open air should be reachable");
    };
    assert_eq!(nodes.last(), Some(&BlockPos::new(0, 72, 0)));
    assert!(nodes.iter().all(|pos| pos.x() == 0 && pos.z() == 0));
}
