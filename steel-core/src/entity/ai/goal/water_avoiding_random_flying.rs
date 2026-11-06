//! Vanilla `WaterAvoidingRandomFlyingGoal.getPosition`, the wander target of
//! flying mobs. Their wander goals pair `RandomStrollGoal` with this position.

use std::f32::consts::FRAC_PI_2;

use glam::DVec3;

use super::random_pos::{air_and_water_random_pos, hover_random_pos};
use crate::entity::PathfinderMob;

/// Vanilla `xzDist`.
const HORIZONTAL_DIST: i32 = 8;

/// Vanilla `WaterAvoidingRandomFlyingGoal.getPosition`: a spot ahead of the
/// mob hovering one to three blocks above the ground, else any open spot.
pub(crate) fn random_flying_pos(mob: &(impl PathfinderMob + ?Sized)) -> Option<DVec3> {
    let wander_direction = mob.look_angle();
    hover_random_pos(
        mob,
        HORIZONTAL_DIST,
        7,
        wander_direction.x,
        wander_direction.z,
        FRAC_PI_2,
        3,
        1,
    )
    .or_else(|| {
        air_and_water_random_pos(
            mob,
            HORIZONTAL_DIST,
            4,
            -2,
            wander_direction.x,
            wander_direction.z,
            f64::from(FRAC_PI_2),
        )
    })
}
