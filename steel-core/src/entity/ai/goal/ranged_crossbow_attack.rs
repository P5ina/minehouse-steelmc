//! Vanilla `RangedCrossbowAttackGoal`: charge, wait, then fire a crossbow.

use glam::DVec3;
use steel_registry::vanilla_items;

use super::selector::{Goal, GoalControls};
use super::weapon_holding_hand;
use crate::behavior::items::crossbow::get_charge_duration;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};

/// Line-of-sight ticks before the mob stops to charge.
const SEE_TIME_TO_STOP: i32 = 5;
const LOOK_MAX_ROTATION: f32 = 30.0;
/// Vanilla `PATHFINDING_DELAY_RANGE = TimeUtil.rangeOfSeconds(1, 2)`, in ticks.
const PATHFINDING_DELAY_MIN: i32 = 20;
const PATHFINDING_DELAY_MAX: i32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrossbowState {
    Uncharged,
    Charging,
    Charged,
    ReadyToAttack,
}

/// Vanilla `RangedCrossbowAttackGoal`. The mob must implement
/// [`crate::entity::CrossbowAttackMob`].
pub(crate) struct RangedCrossbowAttackGoal {
    crossbow_state: CrossbowState,
    speed_modifier: f64,
    attack_radius_sqr: f32,
    see_time: i32,
    attack_delay: i32,
    update_path_delay: i32,
}

impl RangedCrossbowAttackGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64, attack_radius: f32) -> Self {
        Self {
            crossbow_state: CrossbowState::Uncharged,
            speed_modifier,
            attack_radius_sqr: attack_radius * attack_radius,
            see_time: 0,
            attack_delay: 0,
            update_path_delay: 0,
        }
    }

    const fn can_run(&self) -> bool {
        matches!(self.crossbow_state, CrossbowState::Uncharged)
    }
}

fn is_valid_target(mob: &dyn PathfinderMob) -> bool {
    mob.target().is_some_and(|target| {
        target
            .as_living_entity()
            .is_some_and(LivingEntity::is_alive)
    })
}

fn is_holding_crossbow(mob: &dyn PathfinderMob) -> bool {
    mob.is_holding(&mut |stack| stack.is(&vanilla_items::CROSSBOW))
}

impl Goal for RangedCrossbowAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        is_valid_target(mob) && is_holding_crossbow(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        is_valid_target(mob)
            && (self.can_use(mob) || !mob.mob_base().navigation().lock().is_done())
            && is_holding_crossbow(mob)
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        mob.set_aggressive(false);
        mob.set_target(None);
        self.see_time = 0;
        if mob.living_base().is_using_item() {
            let Some(crossbow_mob) = mob.as_crossbow_attack_mob() else {
                return;
            };
            // VANILLA: `getUseItem().set(CHARGED_PROJECTILES, EMPTY)` runs after
            // `stopUsingItem`, when the use item is already `ItemStack.EMPTY`, so
            // the crossbow keeps whatever it had loaded.
            crossbow_mob.stop_using_held_item();
            crossbow_mob.set_charging_crossbow(false);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        let Some(target_living) = target.as_living_entity() else {
            return;
        };
        let Some(crossbow_mob) = mob.as_crossbow_attack_mob() else {
            return;
        };
        let has_line_of_sight = mob.has_line_of_sight_cached(target_living);
        let saw_target_before = self.see_time > 0;
        if has_line_of_sight != saw_target_before {
            self.see_time = 0;
        }
        if has_line_of_sight {
            self.see_time += 1;
        } else {
            self.see_time -= 1;
        }

        let distance_to_sqr = mob.distance_to_sqr(target.position());
        let needs_to_move = (distance_to_sqr > f64::from(self.attack_radius_sqr)
            || self.see_time < SEE_TIME_TO_STOP)
            && self.attack_delay == 0;
        if needs_to_move {
            self.update_path_delay -= 1;
            if self.update_path_delay <= 0 {
                let speed = if self.can_run() {
                    self.speed_modifier
                } else {
                    self.speed_modifier * 0.5
                };
                mob.move_to_entity(target.as_ref(), speed);
                self.update_path_delay =
                    rand::random_range(PATHFINDING_DELAY_MIN..=PATHFINDING_DELAY_MAX);
            }
        } else {
            self.update_path_delay = 0;
            mob.mob_base().navigation().lock().stop();
        }

        let target_position = target.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
            LOOK_MAX_ROTATION,
            LOOK_MAX_ROTATION,
        );

        match self.crossbow_state {
            CrossbowState::Uncharged => {
                if !needs_to_move {
                    crossbow_mob.start_using_held_item(weapon_holding_hand(
                        crossbow_mob,
                        &vanilla_items::CROSSBOW,
                    ));
                    self.crossbow_state = CrossbowState::Charging;
                    crossbow_mob.set_charging_crossbow(true);
                }
            }
            CrossbowState::Charging => {
                if !crossbow_mob.living_base().is_using_item() {
                    self.crossbow_state = CrossbowState::Uncharged;
                }
                let pull_time = crossbow_mob.ticks_using_item();
                let use_item = crossbow_mob.use_held_item();
                if pull_time >= get_charge_duration(&use_item) {
                    crossbow_mob.release_using_held_item(entity);
                    self.crossbow_state = CrossbowState::Charged;
                    self.attack_delay = 20 + rand::random_range(0..20);
                    crossbow_mob.set_charging_crossbow(false);
                }
            }
            CrossbowState::Charged => {
                self.attack_delay -= 1;
                if self.attack_delay == 0 {
                    self.crossbow_state = CrossbowState::ReadyToAttack;
                }
            }
            CrossbowState::ReadyToAttack => {
                if has_line_of_sight {
                    crossbow_mob.perform_ranged_attack(entity, &target, 1.0);
                    self.crossbow_state = CrossbowState::Uncharged;
                }
            }
        }
    }
}
