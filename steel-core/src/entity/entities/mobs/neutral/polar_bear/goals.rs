//! The polar bear's own goals, vanilla `PolarBear`'s inner goal classes.

use steel_utils::Downcast as _;

use super::PolarBearEntity;
use crate::entity::ai::goal::{
    Goal, GoalControls, HurtByTargetGoal, MeleeAttackGoal, NearestAttackableTargetGoal,
};
use crate::entity::{AgeableMob, Entity, PathfinderMob, SharedEntity};

/// Vanilla `PolarBearAttackPlayersGoal` range around the bear in which a cub
/// makes it attack players.
const CUB_SEARCH_XZ: f64 = 8.0;
const CUB_SEARCH_Y: f64 = 4.0;
/// How many blocks beyond the target's width the bear stands up and roars.
const WARNING_DISTANCE: f64 = 3.0;
/// The bear stands up this many ticks before its next attack.
const STAND_UP_TICKS: i32 = 10;

fn is_baby(mob: &dyn PathfinderMob) -> bool {
    mob.as_ageable_mob().is_some_and(AgeableMob::is_baby)
}

fn is_adult_polar_bear(entity: &dyn Entity) -> bool {
    entity
        .downcast_ref::<PolarBearEntity>()
        .is_some_and(|bear| !AgeableMob::is_baby(bear))
}

/// Vanilla `PolarBearMeleeAttackGoal`: stands up and roars when close, and
/// strikes without swinging.
pub(super) struct PolarBearMeleeAttackGoal {
    attack: MeleeAttackGoal,
}

impl PolarBearMeleeAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            attack: MeleeAttackGoal::new(1.25, true),
        }
    }

    fn check_and_perform_attack(
        &mut self,
        mob: &dyn PathfinderMob,
        bear: &PolarBearEntity,
        entity: &SharedEntity,
        target: &SharedEntity,
    ) {
        let Some(target_living) = target.as_living_entity() else {
            return;
        };
        if self.attack.can_perform_attack(mob, target_living) {
            self.attack.reset_attack_cooldown();
            if let Some(world) = mob.level() {
                let _ = mob.do_hurt_target(entity, &world, target);
            }
            bear.set_standing(false);
            return;
        }

        let reach = f64::from(target.base().dimensions().width) + WARNING_DISTANCE;
        if mob.position().distance_squared(target.position()) < reach * reach {
            if self.attack.is_time_to_attack() {
                bear.set_standing(false);
                self.attack.reset_attack_cooldown();
            }
            if self.attack.get_ticks_until_next_attack() <= STAND_UP_TICKS {
                bear.set_standing(true);
                bear.play_warning_sound();
            }
        } else {
            self.attack.reset_attack_cooldown();
            bear.set_standing(false);
        }
    }
}

impl Goal for PolarBearMeleeAttackGoal {
    fn controls(&self) -> GoalControls {
        self.attack.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.attack.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(bear) = mob.downcast_ref::<PolarBearEntity>() {
            bear.set_standing(false);
        }
        self.attack.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.attack.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        self.attack.tick_pursuit(mob, &target);
        if let Some(bear) = mob.downcast_ref::<PolarBearEntity>() {
            self.check_and_perform_attack(mob, bear, entity, &target);
        }
    }
}

/// Vanilla `PolarBearHurtByTargetGoal`: a hurt cub calls the adults nearby and
/// keeps out of the fight itself.
pub(super) struct PolarBearHurtByTargetGoal {
    hurt_by: HurtByTargetGoal,
}

impl PolarBearHurtByTargetGoal {
    pub(super) const fn new() -> Self {
        Self {
            hurt_by: HurtByTargetGoal::new().with_alert_matcher(is_adult_polar_bear),
        }
    }
}

impl Goal for PolarBearHurtByTargetGoal {
    fn controls(&self) -> GoalControls {
        self.hurt_by.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.hurt_by.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.hurt_by.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.start(mob);
        if is_baby(mob) {
            if let Some(hurt_by_mob) = mob.last_hurt_by_mob() {
                self.hurt_by.alert_others(mob, &hurt_by_mob);
            }
            self.hurt_by.stop(mob);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.stop(mob);
    }
}

/// Vanilla `PolarBearAttackPlayersGoal`: an adult attacks players that come
/// close while a cub is nearby.
pub(super) struct PolarBearAttackPlayersGoal {
    nearest: NearestAttackableTargetGoal,
}

impl PolarBearAttackPlayersGoal {
    pub(super) fn new() -> Self {
        Self {
            nearest: NearestAttackableTargetGoal::new_for_players_with_interval(
                20,
                true,
                true,
                |_, _| true,
            )
            .with_follow_distance_scale(0.5),
        }
    }
}

impl Goal for PolarBearAttackPlayersGoal {
    fn controls(&self) -> GoalControls {
        self.nearest.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if is_baby(mob) || !self.nearest.can_use(mob) {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        let area = mob
            .bounding_box()
            .inflate_xyz(CUB_SEARCH_XZ, CUB_SEARCH_Y, CUB_SEARCH_XZ);
        world.has_entity_in_aabb_matching(&area, |entity| {
            entity
                .downcast_ref::<PolarBearEntity>()
                .is_some_and(AgeableMob::is_baby)
        })
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.nearest.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.stop(mob);
    }
}
