//! Vanilla `FlyingMoveControl`, used by parrots, bees and allays.

use std::f32::consts::PI;

use steel_registry::vanilla_attributes;

use crate::entity::ai::control::MoveControlOperation;
use crate::entity::mob::rotlerp;
use crate::entity::{LivingTravelInput, Mob};

/// Vanilla `2.5000003E-7F`, widened to double.
const MIN_DISTANCE_SQR: f64 = 2.500_000_3E-7_f32 as f64;
/// Vanilla `1.0E-5F`, widened to double.
const ANGLE_EPSILON: f64 = 1.0E-5_f32 as f64;
/// Vanilla's fixed yaw step while flying.
const MAX_Y_TURN: f32 = 90.0;

/// Vanilla `(float)(Mth.atan2(a, b) * 180.0F / (float)Math.PI)`.
fn atan2_degrees(a: f64, b: f64) -> f32 {
    (a.atan2(b) * 180.0 / f64::from(PI)) as f32
}

/// Settings of a vanilla `FlyingMoveControl`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FlyingMoveControl {
    /// Largest pitch change per tick, in degrees.
    pub max_turn: i32,
    /// Whether the mob keeps gravity off while it has nowhere to go.
    pub hovers_in_place: bool,
}

impl FlyingMoveControl {
    /// Vanilla `FlyingMoveControl.tick`.
    pub(crate) fn tick<M: Mob + ?Sized>(self, mob: &M) {
        let move_control = {
            let mut controls = mob.mob_base().controls().lock();
            let move_control = controls.move_control;
            if move_control.operation() == MoveControlOperation::MoveTo {
                controls.move_control.set_wait();
            }
            move_control
        };

        if move_control.operation() != MoveControlOperation::MoveTo {
            if !self.hovers_in_place {
                mob.set_no_gravity(false);
            }
            let input = mob.travel_input();
            mob.set_travel_input(LivingTravelInput::new(input.sideways(), 0.0, 0.0));
            return;
        }

        mob.set_no_gravity(true);
        let position = mob.position();
        let wanted = move_control.wanted_position();
        let xd = wanted.x - position.x;
        let yd = wanted.y - position.y;
        let zd = wanted.z - position.z;
        let dd = xd * xd + yd * yd + zd * zd;
        if dd < MIN_DISTANCE_SQR {
            let input = mob.travel_input();
            mob.set_travel_input(LivingTravelInput::new(input.sideways(), 0.0, 0.0));
            return;
        }

        let y_rot_d = atan2_degrees(zd, xd) - 90.0;
        let (y_rot, x_rot) = mob.rotation();
        let y_rot = rotlerp(y_rot, y_rot_d, MAX_Y_TURN);
        mob.set_rotation((y_rot, x_rot));

        let attribute = if mob.on_ground() {
            vanilla_attributes::MOVEMENT_SPEED
        } else {
            vanilla_attributes::FLYING_SPEED
        };
        let speed = (move_control.speed_modifier()
            * mob.attributes().lock().required_value(attribute)) as f32;
        mob.set_mob_speed(speed);

        let horizontal = (xd * xd + zd * zd).sqrt();
        if yd.abs() > ANGLE_EPSILON || horizontal.abs() > ANGLE_EPSILON {
            let x_rot_d = -atan2_degrees(yd, horizontal);
            mob.set_rotation((y_rot, rotlerp(x_rot, x_rot_d, self.max_turn as f32)));
            let input = mob.travel_input();
            mob.set_travel_input(LivingTravelInput::new(
                input.sideways(),
                if yd > 0.0 { speed } else { -speed },
                input.forward(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::atan2_degrees;

    #[test]
    fn atan2_degrees_matches_vanilla_angles() {
        assert!((atan2_degrees(1.0, 0.0) - 90.0).abs() < 1e-4);
        assert!((atan2_degrees(0.0, 1.0)).abs() < 1e-6);
        assert!((atan2_degrees(-1.0, 0.0) + 90.0).abs() < 1e-4);
    }
}
