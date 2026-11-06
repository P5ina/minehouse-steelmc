//! Vanilla `PatrollingMonster.LongDistancePatrolGoal`.

use glam::DVec3;
use steel_math::vector::y_rot;
use steel_utils::BlockPos;

use super::selector::{Goal, GoalControls};
use crate::chunk::heightmap::HeightmapType;
use crate::entity::{PathfinderMob, PatrollingMonster, SharedEntity};

/// Vanilla `NAVIGATION_FAILED_COOLDOWN`.
const NAVIGATION_FAILED_COOLDOWN: i64 = 200;
/// Patrol companions are looked for this far around the mob.
const COMPANION_SEARCH_RANGE: f64 = 16.0;
/// A leader picks a new patrol target once this close to the old one.
const PATROL_TARGET_REACHED_DISTANCE: f64 = 10.0;
/// Vanilla `distance.yRot(90.0F)`: the angle is in radians, as `Vec3.yRot` takes.
const PATROL_SWERVE_ANGLE: f32 = 90.0;
const PATROL_SWERVE_SCALE: f64 = 0.4;
const PATROL_STEP_LENGTH: f64 = 10.0;
const RANDOM_MOVE_SPREAD: i32 = 8;
const VANILLA_NORMALIZE_EPSILON: f64 = 1.0E-5;

fn as_patrolling(mob: &dyn PathfinderMob) -> Option<&dyn PatrollingMonster> {
    mob.as_patrolling_monster()
}

/// Walks a patrol toward its far-away target in steps of ten blocks, the leader
/// handing each step to the rest of the patrol.
pub(crate) struct LongDistancePatrolGoal {
    speed_modifier: f64,
    leader_speed_modifier: f64,
    cooldown_until: i64,
}

impl LongDistancePatrolGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64, leader_speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            leader_speed_modifier,
            cooldown_until: -1,
        }
    }

    fn find_patrol_companions(mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
        let Some(world) = mob.level() else {
            return Vec::new();
        };
        let uuid = mob.uuid();
        let search_box = mob.bounding_box().inflate_xyz(
            COMPANION_SEARCH_RANGE,
            COMPANION_SEARCH_RANGE,
            COMPANION_SEARCH_RANGE,
        );
        world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.uuid() != uuid
                && entity
                    .as_patrolling_monster()
                    .is_some_and(PatrollingMonster::can_join_patrol)
        })
    }

    fn move_randomly(&self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        let offset = mob.block_position().offset(
            -RANDOM_MOVE_SPREAD + rand::random_range(0..RANDOM_MOVE_SPREAD * 2),
            0,
            -RANDOM_MOVE_SPREAD + rand::random_range(0..RANDOM_MOVE_SPREAD * 2),
        );
        let target = world.heightmap_pos(HeightmapType::MotionBlockingNoLeaves, offset);
        mob.move_to_pos(block_as_vec(target), self.speed_modifier)
    }
}

/// Vanilla `Vec3.normalize`: zero below a length of `1.0E-5`.
fn vanilla_normalize(vector: DVec3) -> DVec3 {
    let length = vector.length();
    if length < VANILLA_NORMALIZE_EPSILON {
        DVec3::ZERO
    } else {
        vector / length
    }
}

fn block_as_vec(pos: BlockPos) -> DVec3 {
    DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()))
}

impl Goal for LongDistancePatrolGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(patroller) = as_patrolling(mob) else {
            return false;
        };
        let on_cooldown = mob
            .level()
            .is_some_and(|world| world.game_time() < self.cooldown_until);
        patroller.is_patrolling()
            && mob.target().is_none()
            && !mob.has_controlling_passenger()
            && patroller.has_patrol_target()
            && !on_cooldown
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(patroller) = as_patrolling(mob) else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        let patrol_leader = patroller.is_patrol_leader();
        if !mob.mob_base().navigation().lock().is_done() {
            return;
        }

        let companions = Self::find_patrol_companions(mob);
        let Some(patrol_target) = patroller.patrol_target() else {
            return;
        };
        if patroller.is_patrolling() && companions.is_empty() {
            patroller.set_patrolling(false);
        } else if patrol_leader
            && patrol_target.closer_to_center_than(mob.position(), PATROL_TARGET_REACHED_DISTANCE)
        {
            patroller.find_patrol_target();
        } else {
            let long_distance_target = DVec3::new(
                f64::from(patrol_target.x()) + 0.5,
                f64::from(patrol_target.y()),
                f64::from(patrol_target.z()) + 0.5,
            );
            let self_vector = mob.position();
            let distance = self_vector - long_distance_target;
            let long_distance_target =
                y_rot(distance, PATROL_SWERVE_ANGLE) * PATROL_SWERVE_SCALE + long_distance_target;
            let move_target = vanilla_normalize(long_distance_target - self_vector)
                * PATROL_STEP_LENGTH
                + self_vector;
            let path_target = BlockPos::containing(move_target.x, move_target.y, move_target.z);
            let path_target =
                world.heightmap_pos(HeightmapType::MotionBlockingNoLeaves, path_target);
            let speed = if patrol_leader {
                self.leader_speed_modifier
            } else {
                self.speed_modifier
            };
            if !mob.move_to_pos(block_as_vec(path_target), speed) {
                self.move_randomly(mob);
                self.cooldown_until = world.game_time() + NAVIGATION_FAILED_COOLDOWN;
            } else if patrol_leader {
                for companion in companions {
                    if let Some(companion) = companion.as_patrolling_monster() {
                        companion.set_patrol_target(path_target);
                    }
                }
            }
        }
    }
}
