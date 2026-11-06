//! Goals only equines use.

use glam::DVec3;

use super::AbstractHorse;
use crate::entity::ai::goal::random_pos::default_random_pos;
use crate::entity::ai::goal::{Goal, GoalControls, PanicGoal, reduced_tick_delay};
use crate::entity::{PathfinderMob, SharedEntity};
use steel_utils::entity_events::EntityStatus;

/// Vanilla `RunAroundLikeCrazyGoal`: an untamed horse bucks its rider off
/// until its temper lets the rider tame it.
pub(super) struct RunAroundLikeCrazyGoal {
    speed_modifier: f64,
    wanted_position: DVec3,
}

impl RunAroundLikeCrazyGoal {
    pub(super) const fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            wanted_position: DVec3::ZERO,
        }
    }
}

impl Goal for RunAroundLikeCrazyGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(horse) = mob.as_abstract_horse() else {
            return false;
        };
        if horse.is_mob_controlled() || horse.is_tamed() || !mob.is_vehicle() {
            return false;
        }
        let Some(position) = default_random_pos(mob, 5, 4) else {
            return false;
        };
        self.wanted_position = position;
        true
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.move_to_pos(self.wanted_position, self.speed_modifier);
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.as_abstract_horse()
            .is_some_and(|horse| !horse.is_tamed())
            && !mob.mob_base().navigation().lock().is_done()
            && mob.is_vehicle()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(horse) = mob.as_abstract_horse() else {
            return;
        };
        if horse.is_tamed() || rand::random_range(0..reduced_tick_delay(50)) != 0 {
            return;
        }
        let Some(passenger) = mob.first_passenger() else {
            return;
        };

        if passenger.as_player().is_some() {
            let temper = horse.temper();
            let max_temper = horse.max_temper();
            if max_temper > 0 && rand::random_range(0..max_temper) < temper {
                horse.tame_with_name(&passenger);
                return;
            }
            horse.modify_temper(5);
        }

        for passenger in mob.passengers() {
            passenger.stop_riding();
        }
        horse.make_mad();
        mob.broadcast_entity_event(EntityStatus::TamingFailed);
    }
}

/// Vanilla `RandomStandGoal`: an idle horse rears now and then.
pub(super) struct RandomStandGoal {
    next_stand: i32,
}

impl RandomStandGoal {
    /// Starts the countdown at vanilla's `-getAmbientStandInterval()`.
    pub(super) const fn new(ambient_stand_interval: i32) -> Self {
        Self {
            next_stand: -ambient_stand_interval,
        }
    }
}

impl Goal for RandomStandGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(horse) = mob.as_abstract_horse() else {
            return false;
        };
        self.next_stand += 1;
        if self.next_stand > 0 && rand::random_range(0..1000) < self.next_stand {
            self.next_stand = -horse.ambient_stand_interval();
            return !mob.is_immobile() && rand::random_range(0..10) == 0;
        }
        false
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(horse) = mob.as_abstract_horse() else {
            return;
        };
        horse.stand_if_possible();
        if let Some(sound) = horse.ambient_stand_sound() {
            mob.make_sound(Some(sound));
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }
}

/// Vanilla `AbstractHorse.MountPanicGoal`: no panic while a mob rides it.
pub(super) struct MountPanicGoal(PanicGoal);

impl MountPanicGoal {
    pub(super) const fn new(speed_modifier: f64) -> Self {
        Self(PanicGoal::new(speed_modifier))
    }
}

impl Goal for MountPanicGoal {
    fn controls(&self) -> GoalControls {
        self.0.controls()
    }

    fn is_panic_goal(&self) -> bool {
        self.0.is_panic_goal()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        // Vanilla's override only gates `shouldPanic`, the first check.
        !mob.as_abstract_horse()
            .is_some_and(AbstractHorse::is_mob_controlled)
            && self.0.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.0.stop(mob);
    }
}
