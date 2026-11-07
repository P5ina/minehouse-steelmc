//! Vanilla `MoveThroughVillageGoal`: a mob near a village walks from one
//! occupied village point of interest to the next.

use std::f64::consts::FRAC_PI_2;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_poi_type_tags::PoiTag;
use steel_registry::{REGISTRY, RegistryExt as _, TaggedRegistryExt as _};
use steel_utils::BlockPos;

use super::random_pos::{default_random_pos_towards, land_random_pos_weighted};
use super::selector::{Goal, GoalControls};
use crate::behavior::BLOCK_BEHAVIORS;
use crate::entity::ai::navigation::NavigationKind;
use crate::entity::ai::path::Path;
use crate::entity::{PathfinderMob, SharedEntity};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `isCloseToVillage(pos, 6)`.
const CLOSE_TO_VILLAGE_SECTIONS: i32 = 6;
/// Vanilla `PoiManager.find` radius around the candidate position.
const POI_SEARCH_RADIUS: i32 = 10;
/// Vanilla `updateVisited`: the visited list keeps at most this many entries.
const MAX_VISITED: usize = 15;

/// Whether the mob may path through doors, vanilla `canDealWithDoors`.
type CanDealWithDoors = fn(&dyn PathfinderMob) -> bool;

/// Vanilla `MoveThroughVillageGoal`.
pub struct MoveThroughVillageGoal {
    speed_modifier: f64,
    path: Option<Path>,
    poi_pos: BlockPos,
    only_at_night: bool,
    visited: Vec<BlockPos>,
    distance_to_poi: i32,
    can_deal_with_doors: CanDealWithDoors,
}

impl MoveThroughVillageGoal {
    #[must_use]
    pub(crate) const fn new(
        speed_modifier: f64,
        only_at_night: bool,
        distance_to_poi: i32,
        can_deal_with_doors: CanDealWithDoors,
    ) -> Self {
        Self {
            speed_modifier,
            path: None,
            poi_pos: BlockPos::ZERO,
            only_at_night,
            visited: Vec::new(),
            distance_to_poi,
            can_deal_with_doors,
        }
    }

    /// Vanilla `hasNotVisited`.
    fn has_not_visited(&self, poi: BlockPos) -> bool {
        !self.visited.contains(&poi)
    }

    /// Vanilla `updateVisited`.
    fn update_visited(&mut self) {
        if self.visited.len() > MAX_VISITED {
            self.visited.remove(0);
        }
    }

    /// Vanilla `PoiManager.find(#village, hasNotVisited, pos, 10, IS_OCCUPIED)`.
    fn find_village_poi(&self, world: &World, pos: BlockPos) -> Option<BlockPos> {
        world
            .poi_storage
            .lock()
            .get_in_range(
                &|poi_type| {
                    REGISTRY.poi_types.by_id(poi_type).is_some_and(|poi_type| {
                        REGISTRY.poi_types.is_in_tag(poi_type, &PoiTag::VILLAGE)
                    })
                },
                pos,
                POI_SEARCH_RADIUS,
                OccupationStatus::Occupied,
            )
            .into_iter()
            .map(|(poi_pos, _)| poi_pos)
            .find(|&poi_pos| self.has_not_visited(poi_pos))
    }

    /// Vanilla `navigation.createPath` with `setCanOpenDoors(canDealWithDoors)`
    /// around it.
    fn create_path(&self, mob: &dyn PathfinderMob, target: BlockPos) -> Option<Path> {
        let can_deal_with_doors = (self.can_deal_with_doors)(mob);
        mob.mob_base()
            .navigation()
            .lock()
            .set_can_open_doors(can_deal_with_doors);
        let path = mob.create_path_to(target, 0);
        mob.mob_base().navigation().lock().set_can_open_doors(true);
        path
    }
}

/// Vanilla `Vec3i.distSqr`.
fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let dx = f64::from(a.x() - b.x());
    let dy = f64::from(a.y() - b.y());
    let dz = f64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}

/// Vanilla `GoalUtils.hasGroundPathNavigation`.
fn has_ground_path_navigation(mob: &dyn PathfinderMob) -> bool {
    mob.mob_base().navigation().lock().kind() == NavigationKind::Ground
}

/// Vanilla `DoorBlock.isWoodenDoor(level, pos)`.
fn is_wooden_door(world: &World, pos: BlockPos) -> bool {
    let state = world.get_block_state(pos);
    BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .is_wooden_door(state)
}

impl Goal for MoveThroughVillageGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if !has_ground_path_navigation(mob) {
            return false;
        }
        self.update_visited();
        let Some(world) = mob.level() else {
            return false;
        };
        if self.only_at_night && world.is_bright_outside() {
            return false;
        }
        let pos = mob.block_position();
        if !world.is_close_to_village(pos, CLOSE_TO_VILLAGE_SECTIONS) {
            return false;
        }
        let Some(land_pos) = land_random_pos_weighted(mob, 15, 7, |candidate| {
            if !world.is_village(candidate) {
                return f64::NEG_INFINITY;
            }
            self.find_village_poi(&world, candidate)
                .map_or(f64::NEG_INFINITY, |poi_pos| -dist_sqr(poi_pos, pos))
        }) else {
            return false;
        };
        let Some(poi_pos) = self.find_village_poi(
            &world,
            BlockPos::containing(land_pos.x, land_pos.y, land_pos.z),
        ) else {
            return false;
        };
        self.poi_pos = poi_pos;
        let mut path = self.create_path(mob, poi_pos);
        if path.is_none() {
            let target = DVec3::new(
                f64::from(poi_pos.x()) + 0.5,
                f64::from(poi_pos.y()),
                f64::from(poi_pos.z()) + 0.5,
            );
            let Some(partial_step) = default_random_pos_towards(mob, 10, 7, target, FRAC_PI_2)
            else {
                return false;
            };
            path = self.create_path(
                mob,
                BlockPos::containing(partial_step.x, partial_step.y, partial_step.z),
            );
            if path.is_none() {
                return false;
            }
        }
        if let Some(found) = &path {
            let door_node = (0..found.node_count())
                .filter_map(|index| found.node(index))
                .map(|node| BlockPos::new(node.x, node.y, node.z))
                .find(|node_pos| is_wooden_door(&world, node_pos.above()));
            if let Some(node_pos) = door_node {
                path = mob.create_path_to(node_pos, 0);
            }
        }
        self.path = path;
        self.path.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if mob.mob_base().navigation().lock().is_done() {
            return false;
        }
        !self.poi_pos.closer_to_center_than(
            mob.position(),
            mob.bounding_box().width() + f64::from(self.distance_to_poi),
        )
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.move_to_path(self.path.clone(), self.speed_modifier);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if mob.mob_base().navigation().lock().is_done()
            || self
                .poi_pos
                .closer_to_center_than(mob.position(), f64::from(self.distance_to_poi))
        {
            self.visited.push(self.poi_pos);
        }
    }

    fn tick(&mut self, _mob: &dyn PathfinderMob, _entity: &SharedEntity) {}
}
