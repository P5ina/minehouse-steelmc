//! Vanilla `SmoothSwimmingMoveControl` and `SmoothSwimmingLookControl`, used
//! by dolphins, axolotls, nautiluses and tadpoles.

use std::f32::consts::PI;
use std::f64::consts::PI as PI_F64;

use glam::DVec3;
use steel_math::wrap_degrees;
use steel_registry::vanilla_attributes;

use crate::entity::ai::control::{MoveControlOperation, rotate_towards};
use crate::entity::mob::rotlerp;
use crate::entity::{LivingTravelInput, Mob};

const FULL_SPEED_TURN_THRESHOLD: f32 = 10.0;
const STOP_TURN_THRESHOLD: f32 = 60.0;
const MIN_DISTANCE_SQR: f64 = 2.500_000_3E-7_f32 as f64;
const ANGLE_EPSILON: f64 = 1.0E-5_f32 as f64;
const X_ROT_STEP: f32 = 5.0;
const HEAD_TILT_X: f32 = 10.0;
const HEAD_TILT_Y: f32 = 20.0;
const BODY_CATCH_UP: f32 = 4.0;
const IN_WATER_LIFT: f64 = 0.005;

/// Vanilla `(float)(Mth.atan2(a, b) * 180.0F / (float)Math.PI)`.
fn atan2_degrees(a: f64, b: f64) -> f32 {
    (a.atan2(b) * 180.0 / f64::from(PI)) as f32
}

/// Vanilla `x * (float)(Math.PI / 180.0)` on a float angle.
fn to_radians(degrees: f32) -> f32 {
    degrees * (PI_F64 / 180.0) as f32
}

/// Settings of a vanilla `SmoothSwimmingMoveControl`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SmoothSwimmingMoveControl {
    /// Largest pitch, in degrees, the mob swims at.
    pub max_turn_x: i32,
    /// Largest yaw change per tick, in degrees.
    pub max_turn_y: i32,
    /// Speed factor while in water.
    pub in_water_speed_modifier: f32,
    /// Speed factor while out of water.
    pub outside_water_speed_modifier: f32,
    /// Whether the mob drifts up a little each tick in water.
    pub apply_gravity: bool,
}

impl SmoothSwimmingMoveControl {
    /// Vanilla `getTurningSpeedFactor`: slows down on sharp turns on land.
    fn turning_speed_factor(left_to_turn: f32) -> f32 {
        1.0 - ((left_to_turn - FULL_SPEED_TURN_THRESHOLD)
            / (STOP_TURN_THRESHOLD - FULL_SPEED_TURN_THRESHOLD))
            .clamp(0.0, 1.0)
    }

    /// Vanilla `SmoothSwimmingMoveControl.tick`. It keeps `MOVE_TO` until
    /// navigation finishes, unlike the base move control.
    pub(crate) fn tick<M: Mob + ?Sized>(self, mob: &M) {
        if self.apply_gravity && mob.is_in_water() {
            mob.set_velocity(mob.velocity() + DVec3::new(0.0, IN_WATER_LIFT, 0.0));
        }

        let move_control = mob.mob_base().controls().lock().move_control;
        let navigating = !mob.mob_base().navigation().lock().is_done();
        if !matches!(move_control.operation(), MoveControlOperation::MoveTo) || !navigating {
            mob.set_mob_speed(0.0);
            mob.set_travel_input(LivingTravelInput::new(0.0, 0.0, 0.0));
            return;
        }

        let position = mob.position();
        let wanted = move_control.wanted_position();
        let xd = wanted.x - position.x;
        let yd = wanted.y - position.y;
        let zd = wanted.z - position.z;
        let dd = xd * xd + yd * yd + zd * zd;
        if dd < MIN_DISTANCE_SQR {
            let input = mob.travel_input();
            mob.set_travel_input(LivingTravelInput::new(
                input.sideways(),
                input.vertical(),
                0.0,
            ));
            return;
        }

        let y_rot_d = atan2_degrees(zd, xd) - 90.0;
        let (y_rot, x_rot) = mob.rotation();
        let y_rot = rotlerp(y_rot, y_rot_d, self.max_turn_y as f32);
        mob.set_rotation((y_rot, x_rot));
        mob.set_y_body_rot(y_rot);
        mob.set_y_head_rot(y_rot);

        let movement_speed = mob
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED);
        let speed = (move_control.speed_modifier() * movement_speed) as f32;
        if mob.is_in_water() {
            mob.set_mob_speed(speed * self.in_water_speed_modifier);
            let horizontal = (xd * xd + zd * zd).sqrt();
            let mut x_rot = x_rot;
            if yd.abs() > ANGLE_EPSILON || horizontal.abs() > ANGLE_EPSILON {
                let max_turn_x = self.max_turn_x as f32;
                let x_rot_d =
                    wrap_degrees(-atan2_degrees(yd, horizontal)).clamp(-max_turn_x, max_turn_x);
                x_rot = rotate_towards(x_rot, x_rot_d, X_ROT_STEP);
                mob.set_rotation((y_rot, x_rot));
            }

            let cos = to_radians(x_rot).cos();
            let sin = to_radians(x_rot).sin();
            let input = mob.travel_input();
            mob.set_travel_input(LivingTravelInput::new(
                input.sideways(),
                -sin * speed,
                cos * speed,
            ));
        } else {
            let left_to_turn = wrap_degrees(y_rot - y_rot_d).abs();
            mob.set_mob_speed(
                speed
                    * self.outside_water_speed_modifier
                    * Self::turning_speed_factor(left_to_turn),
            );
        }
    }
}

/// Vanilla `LookControl.getYRotD`.
fn look_y_rot_d<M: Mob + ?Sized>(mob: &M, wanted: DVec3) -> Option<f32> {
    let position = mob.position();
    let xd = wanted.x - position.x;
    let zd = wanted.z - position.z;
    (zd.abs() > ANGLE_EPSILON || xd.abs() > ANGLE_EPSILON).then(|| atan2_degrees(zd, xd) - 90.0)
}

/// Vanilla `LookControl.getXRotD`.
fn look_x_rot_d<M: Mob + ?Sized>(mob: &M, wanted: DVec3) -> Option<f32> {
    let position = mob.position();
    let xd = wanted.x - position.x;
    let yd = wanted.y - mob.get_eye_y();
    let zd = wanted.z - position.z;
    let sd = (xd * xd + zd * zd).sqrt();
    (yd.abs() > ANGLE_EPSILON || sd.abs() > ANGLE_EPSILON).then(|| -atan2_degrees(yd, sd))
}

/// Vanilla `SmoothSwimmingLookControl.tick`: the head leads the body, tilted a
/// little, and the body follows once the head turns past
/// `max_y_rot_from_center`.
pub(crate) fn tick_smooth_swimming_look_control<M: Mob + ?Sized>(
    mob: &M,
    max_y_rot_from_center: i32,
) {
    let (looking, look_control) = {
        let mut controls = mob.mob_base().controls().lock();
        let looking = controls.look_control.tick_cooldown();
        (looking, controls.look_control)
    };

    if looking {
        let wanted = look_control.wanted_position();
        if let Some(y_rot_d) = look_y_rot_d(mob, wanted) {
            mob.set_y_head_rot(rotate_towards(
                mob.y_head_rot(),
                y_rot_d + HEAD_TILT_Y,
                look_control.y_max_rot_speed(),
            ));
        }
        if let Some(x_rot_d) = look_x_rot_d(mob, wanted) {
            let (y_rot, x_rot) = mob.rotation();
            mob.set_rotation((
                y_rot,
                rotate_towards(x_rot, x_rot_d + HEAD_TILT_X, look_control.x_max_rot_angle()),
            ));
        }
    } else {
        if mob.mob_base().navigation().lock().is_done() {
            let (y_rot, x_rot) = mob.rotation();
            mob.set_rotation((y_rot, rotate_towards(x_rot, 0.0, X_ROT_STEP)));
        }
        mob.set_y_head_rot(rotate_towards(
            mob.y_head_rot(),
            mob.y_body_rot(),
            look_control.y_max_rot_speed(),
        ));
    }

    let head_diff_body = wrap_degrees(mob.y_head_rot() - mob.y_body_rot());
    let max_from_center = max_y_rot_from_center as f32;
    if head_diff_body < -max_from_center {
        mob.set_y_body_rot(mob.y_body_rot() - BODY_CATCH_UP);
    } else if head_diff_body > max_from_center {
        mob.set_y_body_rot(mob.y_body_rot() + BODY_CATCH_UP);
    }
}
