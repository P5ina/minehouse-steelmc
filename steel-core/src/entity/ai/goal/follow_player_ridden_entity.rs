use std::sync::Arc;

use glam::DVec3;
use steel_utils::BlockPos;
use steel_utils::direction::Direction;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{Entity, PathfinderMob, SharedEntity};
use crate::physics::MoverType;

const SEARCH_INFLATE: f64 = 5.0;
const GO_TO_ENTITY_SPEED: f32 = 0.015;
const GO_IN_ENTITY_DIRECTION_SPEED: f32 = 0.01;
const RECALC_PATH_TICKS: i32 = 10;
const CATCH_UP_DISTANCE: f32 = 4.0;
const FALL_BEHIND_DISTANCE: f32 = 12.0;
const LEAD_BLOCKS: i32 = 10;
const MOVED_EPSILON: f64 = 1.0E-5_f32 as f64;

/// Vanilla `Entity.hasMovedHorizontallyRecently`.
fn has_moved_horizontally_recently(entity: &dyn Entity) -> bool {
    let speed = entity.known_speed();
    (speed.x * speed.x + speed.z * speed.z).sqrt().abs() > MOVED_EPSILON
}

/// Vanilla `Entity.getDirection` (and `getMotionDirection`).
fn facing(entity: &dyn Entity) -> Direction {
    Direction::from_yaw(entity.rotation().0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FollowEntityGoal {
    GoToEntity,
    GoInEntityDirection,
}

/// Vanilla `FollowPlayerRiddenEntityGoal`: swims along with a vehicle a player
/// is steering. `vehicle` stands in for vanilla's entity class filter.
pub(crate) struct FollowPlayerRiddenEntityGoal {
    time_to_recalc_path: i32,
    vehicle: fn(&dyn Entity) -> bool,
    following: Option<SharedEntity>,
    current_goal: FollowEntityGoal,
}

impl FollowPlayerRiddenEntityGoal {
    #[must_use]
    pub(crate) const fn new(vehicle: fn(&dyn Entity) -> bool) -> Self {
        Self {
            time_to_recalc_path: 0,
            vehicle,
            following: None,
            current_goal: FollowEntityGoal::GoToEntity,
        }
    }

    fn nearby_controllers(&self, mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
        let Some(world) = mob.level() else {
            return Vec::new();
        };
        let area = mob.bounding_box().inflate(SEARCH_INFLATE);
        world
            .get_entities_in_aabb_matching(&area, |entity| (self.vehicle)(entity))
            .into_iter()
            .filter_map(|vehicle| vehicle.controlling_passenger())
            .filter(|controller| controller.as_player().is_some())
            .collect()
    }
}

impl Goal for FollowPlayerRiddenEntityGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self
            .following
            .as_ref()
            .is_some_and(|following| has_moved_horizontally_recently(following.as_ref()))
        {
            return true;
        }
        self.nearby_controllers(mob)
            .iter()
            .any(|controller| has_moved_horizontally_recently(controller.as_ref()))
    }

    fn is_interruptable(&self) -> bool {
        true
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        self.following.as_ref().is_some_and(|following| {
            following.is_passenger() && has_moved_horizontally_recently(following.as_ref())
        })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(controller) = self.nearby_controllers(mob).into_iter().next() {
            self.following = Some(controller);
        }
        self.time_to_recalc_path = 0;
        self.current_goal = FollowEntityGoal::GoToEntity;
    }

    fn stop(&mut self, _mob: &dyn PathfinderMob) {
        self.following = None;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let speed = if self.current_goal == FollowEntityGoal::GoInEntityDirection {
            GO_IN_ENTITY_DIRECTION_SPEED
        } else {
            GO_TO_ENTITY_SPEED
        };
        let input = mob.travel_input();
        mob.move_relative(
            speed,
            DVec3::new(
                f64::from(input.sideways()),
                f64::from(input.vertical()),
                f64::from(input.forward()),
            ),
        );
        let _ = Arc::clone(entity).move_entity(MoverType::SelfMovement, mob.velocity());

        self.time_to_recalc_path -= 1;
        if self.time_to_recalc_path > 0 {
            return;
        }
        self.time_to_recalc_path = reduced_tick_delay(RECALC_PATH_TICKS);
        let Some(following) = self.following.clone() else {
            return;
        };

        let distance = mob.position().distance(following.position()) as f32;
        match self.current_goal {
            FollowEntityGoal::GoToEntity => {
                let behind = following
                    .block_position()
                    .relative(facing(following.as_ref()).opposite())
                    .offset(0, -1, 0);
                mob.move_to_pos(block_coords(behind), 1.0);
                if distance < CATCH_UP_DISTANCE {
                    self.time_to_recalc_path = 0;
                    self.current_goal = FollowEntityGoal::GoInEntityDirection;
                }
            }
            FollowEntityGoal::GoInEntityDirection => {
                let direction = facing(following.as_ref());
                let (step_x, step_y, step_z) = direction.offset();
                let go_to = following.block_position().offset(
                    step_x * LEAD_BLOCKS,
                    step_y * LEAD_BLOCKS - 1,
                    step_z * LEAD_BLOCKS,
                );
                mob.move_to_pos(block_coords(go_to), 1.0);
                if distance > FALL_BEHIND_DISTANCE {
                    self.time_to_recalc_path = 0;
                    self.current_goal = FollowEntityGoal::GoToEntity;
                }
            }
        }
    }
}

fn block_coords(pos: BlockPos) -> DVec3 {
    DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()))
}
