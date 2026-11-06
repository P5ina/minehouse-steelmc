//! Vanilla `ZombieAttackGoal`: melee that raises the arms shortly before each hit.

use super::melee_attack::MeleeAttackGoal;
use super::selector::{Goal, GoalControls};
use crate::entity::{PathfinderMob, SharedEntity};

const RAISE_ARM_DELAY_TICKS: i32 = 5;

pub(crate) struct ZombieAttackGoal {
    melee: MeleeAttackGoal,
    raise_arm_ticks: i32,
}

impl ZombieAttackGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64, track_target: bool) -> Self {
        Self {
            melee: MeleeAttackGoal::new(speed_modifier, track_target),
            raise_arm_ticks: 0,
        }
    }
}

impl Goal for ZombieAttackGoal {
    fn controls(&self) -> GoalControls {
        self.melee.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.melee.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.melee.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.melee.start(mob);
        self.raise_arm_ticks = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.melee.stop(mob);
        mob.set_aggressive(false);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.melee.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.melee.tick(mob, entity);
        self.raise_arm_ticks += 1;
        let raise = self.raise_arm_ticks >= RAISE_ARM_DELAY_TICKS
            && self.melee.get_ticks_until_next_attack() < MeleeAttackGoal::attack_interval() / 2;
        mob.set_aggressive(raise);
    }
}
