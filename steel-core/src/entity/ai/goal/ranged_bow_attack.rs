//! Vanilla `RangedBowAttackGoal`: draws the bow for a second, shoots, and
//! strafes around the target while in range.

use glam::DVec3;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_items;
use steel_utils::types::InteractionHand;

use super::selector::{Goal, GoalControls};
use crate::behavior::items::bow::get_power_for_time;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};
use crate::inventory::equipment::EquipmentSlot;

/// Line-of-sight ticks before the mob stops walking and starts strafing.
const SEE_TIME_TO_STRAFE: i32 = 20;
/// Ticks out of sight after which a drawn bow is lowered.
const LOST_SIGHT_TICKS: i32 = -60;
/// Ticks between strafe direction rolls.
const STRAFE_ROLL_TICKS: i32 = 20;
const STRAFE_FLIP_CHANCE: f64 = 0.3;
const STRAFE_SPEED: f32 = 0.5;
/// Ticks the bow is drawn before it is released.
const FULL_DRAW_TICKS: i32 = 20;
const LOOK_MAX_ROTATION: f32 = 30.0;

/// Vanilla `RangedBowAttackGoal`.
///
/// The mob must implement [`crate::entity::RangedAttackMob`].
pub(crate) struct RangedBowAttackGoal {
    speed_modifier: f64,
    attack_interval_min: i32,
    attack_radius_sqr: f32,
    attack_time: i32,
    see_time: i32,
    strafing_clockwise: bool,
    strafing_backwards: bool,
    strafing_time: i32,
}

impl RangedBowAttackGoal {
    #[must_use]
    pub(crate) const fn new(
        speed_modifier: f64,
        attack_interval_min: i32,
        attack_radius: f32,
    ) -> Self {
        Self {
            speed_modifier,
            attack_interval_min,
            attack_radius_sqr: attack_radius * attack_radius,
            attack_time: -1,
            see_time: 0,
            strafing_clockwise: false,
            strafing_backwards: false,
            strafing_time: -1,
        }
    }

    /// Vanilla `setMinAttackInterval`.
    pub(crate) const fn set_min_attack_interval(&mut self, ticks: i32) {
        self.attack_interval_min = ticks;
    }

    fn is_holding_bow(mob: &dyn PathfinderMob) -> bool {
        mob.is_holding(&mut |stack| stack.is(&vanilla_items::BOW))
    }
}

/// Vanilla `ProjectileUtil.getWeaponHoldingHand`.
#[must_use]
pub(crate) fn weapon_holding_hand(mob: &dyn LivingEntity, weapon: ItemRef) -> InteractionHand {
    let mut main_hand = false;
    mob.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| {
        main_hand = stack.is(weapon);
    });
    if main_hand {
        InteractionHand::MainHand
    } else {
        InteractionHand::OffHand
    }
}

impl Goal for RangedBowAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_some() && Self::is_holding_bow(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        (self.can_use(mob) || !mob.mob_base().navigation().lock().is_done())
            && Self::is_holding_bow(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.set_aggressive(true);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        mob.set_aggressive(false);
        self.see_time = 0;
        self.attack_time = -1;
        mob.stop_using_held_item();
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        let target_position = target.position();
        let target_dist_sqr = mob.distance_to_sqr(target_position);
        let has_line_of_sight = mob.has_line_of_sight_cached(target.as_ref());
        let saw_target_before = self.see_time > 0;
        if has_line_of_sight != saw_target_before {
            self.see_time = 0;
        }
        if has_line_of_sight {
            self.see_time += 1;
        } else {
            self.see_time -= 1;
        }

        let attack_radius_sqr = f64::from(self.attack_radius_sqr);
        if target_dist_sqr <= attack_radius_sqr && self.see_time >= SEE_TIME_TO_STRAFE {
            mob.mob_base().navigation().lock().stop();
            self.strafing_time += 1;
        } else {
            let _ = mob.move_to_entity(target.as_ref(), self.speed_modifier);
            self.strafing_time = -1;
        }

        if self.strafing_time >= STRAFE_ROLL_TICKS {
            if f64::from(rand::random::<f32>()) < STRAFE_FLIP_CHANCE {
                self.strafing_clockwise = !self.strafing_clockwise;
            }
            if f64::from(rand::random::<f32>()) < STRAFE_FLIP_CHANCE {
                self.strafing_backwards = !self.strafing_backwards;
            }
            self.strafing_time = 0;
        }

        if self.strafing_time > -1 {
            if target_dist_sqr > f64::from(self.attack_radius_sqr * 0.75) {
                self.strafing_backwards = false;
            } else if target_dist_sqr < f64::from(self.attack_radius_sqr * 0.25) {
                self.strafing_backwards = true;
            }
            mob.mob_base().controls().lock().move_control.strafe(
                if self.strafing_backwards {
                    -STRAFE_SPEED
                } else {
                    STRAFE_SPEED
                },
                if self.strafing_clockwise {
                    STRAFE_SPEED
                } else {
                    -STRAFE_SPEED
                },
            );
            if let Some(vehicle) = mob.controlled_vehicle()
                && let Some(vehicle_mob) = vehicle.as_mob()
            {
                vehicle_mob.mob_look_at(target.as_ref(), LOOK_MAX_ROTATION, LOOK_MAX_ROTATION);
            }
            mob.mob_look_at(target.as_ref(), LOOK_MAX_ROTATION, LOOK_MAX_ROTATION);
        } else {
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
                LOOK_MAX_ROTATION,
                LOOK_MAX_ROTATION,
            );
        }

        if mob.is_using_item() {
            if !has_line_of_sight && self.see_time < LOST_SIGHT_TICKS {
                mob.stop_using_held_item();
            } else if has_line_of_sight {
                let pull_time = mob.ticks_using_item();
                if pull_time >= FULL_DRAW_TICKS {
                    mob.stop_using_held_item();
                    if let Some(ranged) = mob.as_ranged_attack_mob() {
                        ranged.perform_ranged_attack(
                            entity,
                            &target,
                            get_power_for_time(pull_time),
                        );
                    }
                    self.attack_time = self.attack_interval_min;
                }
            }
        } else {
            self.attack_time -= 1;
            if self.attack_time <= 0 && self.see_time >= LOST_SIGHT_TICKS {
                mob.start_using_held_item(weapon_holding_hand(mob, &vanilla_items::BOW));
            }
        }
    }
}
