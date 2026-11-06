//! The vex's own goals (Vanilla `Vex.VexChargeAttackGoal`, `Vex.VexRandomMoveGoal`
//! and `Vex.VexCopyOwnerTargetGoal`).

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::sound_events;
use steel_utils::Downcast as _;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use super::target_goal::TargetGoalBase;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::VexEntity;
use crate::entity::{PathfinderMob, SharedEntity};

/// Vanilla `random.nextInt(reducedTickDelay(7))`.
const MOVE_CHANCE_TICKS: i32 = 7;
/// Vanilla `distanceToSqr(target) > 4.0`: a vex only charges from further away.
const MIN_CHARGE_DISTANCE_SQR: f64 = 4.0;
/// Vanilla `distance < 9.0`: close enough to keep homing in on the target.
const HOMING_DISTANCE_SQR: f64 = 9.0;
const RANDOM_MOVE_ATTEMPTS: usize = 3;
const RANDOM_MOVE_SPEED: f64 = 0.25;

fn vex(mob: &dyn PathfinderMob) -> Option<&VexEntity> {
    mob.downcast_ref::<VexEntity>()
}

fn eye_position(entity: &SharedEntity) -> DVec3 {
    let position = entity.position();
    let eye_y = entity.get_eye_y();
    DVec3::new(position.x, eye_y, position.z)
}

/// Vanilla `Vex.VexChargeAttackGoal`: darts at the target and hits it on contact.
pub(crate) struct VexChargeAttackGoal;

impl VexChargeAttackGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl Goal for VexChargeAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(vex) = vex(mob) else {
            return false;
        };
        let Some(target) = mob.target() else {
            return false;
        };
        target.is_alive()
            && !vex.move_control_has_wanted()
            && rand::random_range(0..reduced_tick_delay(MOVE_CHANCE_TICKS)) == 0
            && mob.position().distance_squared(target.position()) > MIN_CHARGE_DISTANCE_SQR
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        vex(mob).is_some_and(|vex| vex.move_control_has_wanted() && vex.is_charging())
            && mob.target().is_some_and(|target| target.is_alive())
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(vex) = vex(mob) else {
            return;
        };
        if let Some(target) = mob.target() {
            vex.set_wanted_position(eye_position(&target), 1.0);
        }
        vex.set_is_charging(true);
        mob.play_sound(&sound_events::ENTITY_VEX_CHARGE, 1.0, 1.0);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(vex) = vex(mob) {
            vex.set_is_charging(false);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(vex) = vex(mob) else {
            return;
        };
        let Some(target) = mob.target() else {
            return;
        };
        if mob.bounding_box().intersects(target.bounding_box()) {
            if let Some(world) = mob.level() {
                let _ = mob.do_hurt_target(entity, &world, &target);
            }
            vex.set_is_charging(false);
        } else if mob.position().distance_squared(target.position()) < HOMING_DISTANCE_SQR {
            vex.set_wanted_position(eye_position(&target), 1.0);
        }
    }
}

/// Vanilla `Vex.VexRandomMoveGoal`: drifts to a random free spot near its origin.
pub(crate) struct VexRandomMoveGoal;

impl VexRandomMoveGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl Goal for VexRandomMoveGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        vex(mob).is_some_and(|vex| !vex.move_control_has_wanted())
            && rand::random_range(0..reduced_tick_delay(MOVE_CHANCE_TICKS)) == 0
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(vex) = vex(mob) else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        let origin = vex.bound_origin().unwrap_or_else(|| mob.block_position());
        for _ in 0..RANDOM_MOVE_ATTEMPTS {
            let test_pos = origin.offset(
                rand::random_range(0..15) - 7,
                rand::random_range(0..11) - 5,
                rand::random_range(0..15) - 7,
            );
            if world.get_block_state(test_pos).is_air() {
                let wanted = DVec3::new(
                    f64::from(test_pos.x()) + 0.5,
                    f64::from(test_pos.y()) + 0.5,
                    f64::from(test_pos.z()) + 0.5,
                );
                vex.set_wanted_position(wanted, RANDOM_MOVE_SPEED);
                if mob.target().is_none() {
                    mob.mob_base()
                        .controls()
                        .lock()
                        .look_control
                        .set_look_at(wanted, 180.0, 20.0);
                }
                break;
            }
        }
    }
}

/// Vanilla `Vex.VexCopyOwnerTargetGoal`: a vex attacks whatever its owner targets.
pub(crate) struct VexCopyOwnerTargetGoal {
    target_goal: TargetGoalBase,
    copy_owner_targeting: TargetingConditions,
}

impl VexCopyOwnerTargetGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            target_goal: TargetGoalBase::new(false, false),
            copy_owner_targeting: TargetingConditions::for_non_combat()
                .ignore_line_of_sight()
                .ignore_invisibility_testing(),
        }
    }
}

fn owner_target(mob: &dyn PathfinderMob) -> Option<SharedEntity> {
    vex(mob)?.owner()?.as_mob()?.target()
}

impl Goal for VexCopyOwnerTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(target) = owner_target(mob) else {
            return false;
        };
        self.target_goal
            .can_attack(mob, target.as_living_entity(), &self.copy_owner_targeting)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target_goal.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let target = owner_target(mob);
        let _ = mob.set_target(target.as_ref());
        self.target_goal.start();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target_goal.stop(mob);
    }
}
