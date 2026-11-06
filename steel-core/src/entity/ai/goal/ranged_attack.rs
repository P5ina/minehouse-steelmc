use glam::DVec3;

use super::selector::{Goal, GoalControls};
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};

/// Line-of-sight ticks before a ranged mob stops to shoot.
const SEE_TIME_TO_STOP: i32 = 5;
const LOOK_MAX_ROTATION: f32 = 30.0;

/// Vanilla `RangedAttackGoal`: approaches the target and shoots it from range.
///
/// The mob must implement [`crate::entity::RangedAttackMob`].
pub(crate) struct RangedAttackGoal {
    target: Option<SharedEntity>,
    attack_time: i32,
    speed_modifier: f64,
    see_time: i32,
    attack_interval_min: i32,
    attack_interval_max: i32,
    attack_radius: f32,
    attack_radius_sqr: f32,
}

impl RangedAttackGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64, attack_interval: i32, attack_radius: f32) -> Self {
        Self::with_interval(
            speed_modifier,
            attack_interval,
            attack_interval,
            attack_radius,
        )
    }

    #[must_use]
    pub(crate) const fn with_interval(
        speed_modifier: f64,
        attack_interval_min: i32,
        attack_interval_max: i32,
        attack_radius: f32,
    ) -> Self {
        Self {
            target: None,
            attack_time: -1,
            speed_modifier,
            see_time: 0,
            attack_interval_min,
            attack_interval_max,
            attack_radius,
            attack_radius_sqr: attack_radius * attack_radius,
        }
    }
}

fn is_alive(entity: &SharedEntity) -> bool {
    entity
        .as_living_entity()
        .is_some_and(LivingEntity::is_alive)
}

impl Goal for RangedAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(target) = mob.target().filter(is_alive) else {
            return false;
        };
        self.target = Some(target);
        true
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob)
            || self.target.as_ref().is_some_and(is_alive)
                && !mob.mob_base().navigation().lock().is_done()
    }

    fn stop(&mut self, _mob: &dyn PathfinderMob) {
        self.target = None;
        self.see_time = 0;
        self.attack_time = -1;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(target_living) = target.as_living_entity() else {
            return;
        };
        let target_dist_sqr = mob.distance_to_sqr(target.position());
        let has_line_of_sight = mob.has_line_of_sight_cached(target_living);
        if has_line_of_sight {
            self.see_time += 1;
        } else {
            self.see_time = 0;
        }

        if target_dist_sqr <= f64::from(self.attack_radius_sqr) && self.see_time >= SEE_TIME_TO_STOP
        {
            mob.mob_base().navigation().lock().stop();
        } else {
            let _ = mob.move_to_pos(target.position(), self.speed_modifier);
        }

        let target_position = target.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
            LOOK_MAX_ROTATION,
            LOOK_MAX_ROTATION,
        );

        self.attack_time -= 1;
        let dist_ratio = target_dist_sqr.sqrt() / f64::from(self.attack_radius);
        if self.attack_time == 0 {
            if !has_line_of_sight {
                return;
            }
            let dist = dist_ratio as f32;
            let power = dist.clamp(0.1, 1.0);
            if let Some(ranged) = mob.as_ranged_attack_mob() {
                ranged.perform_ranged_attack(entity, &target, power);
            }
            let span = (self.attack_interval_max - self.attack_interval_min) as f32;
            self.attack_time = (dist * span + self.attack_interval_min as f32).floor() as i32;
        } else if self.attack_time < 0 {
            let span = f64::from(self.attack_interval_max - self.attack_interval_min);
            self.attack_time =
                (dist_ratio * span + f64::from(self.attack_interval_min)).floor() as i32;
        }
    }
}
