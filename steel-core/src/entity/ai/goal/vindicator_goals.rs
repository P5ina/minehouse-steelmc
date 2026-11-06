//! The vindicator's own goals (Vanilla `Vindicator.VindicatorBreakDoorGoal` and
//! `Vindicator.VindicatorJohnnyAttackGoal`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use steel_utils::types::Difficulty;

use super::break_door::BreakDoorGoal;
use super::nearest_attackable_target::NearestAttackableTargetGoal;
use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{PathfinderMob, Raider, SharedEntity};

/// Vanilla `new BreakDoorGoal(mob, 6, ...)`; shorter than the default, so the
/// default 240 ticks apply.
const VINDICATOR_DOOR_BREAK_TIME: i32 = 6;
/// Vanilla `random.nextInt(reducedTickDelay(10))`.
const BREAK_DOOR_CHANCE_TICKS: i32 = 10;

/// Vanilla `Vindicator.DOOR_BREAKING_PREDICATE`.
fn door_breaking_difficulty(difficulty: Difficulty) -> bool {
    difficulty == Difficulty::Normal || difficulty == Difficulty::Hard
}

/// Vanilla `Vindicator.VindicatorBreakDoorGoal`: vindicators hack through
/// doors, but only during a raid.
pub(crate) struct VindicatorBreakDoorGoal {
    inner: BreakDoorGoal,
}

impl VindicatorBreakDoorGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            inner: BreakDoorGoal::with_door_break_time(
                door_breaking_difficulty,
                VINDICATOR_DOOR_BREAK_TIME,
            ),
        }
    }
}

fn has_active_raid(mob: &dyn PathfinderMob) -> bool {
    mob.as_raider().is_some_and(Raider::has_active_raid)
}

impl Goal for VindicatorBreakDoorGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn is_break_door_goal(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        has_active_raid(mob)
            && rand::random_range(0..reduced_tick_delay(BREAK_DOOR_CHANCE_TICKS)) == 0
            && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        has_active_raid(mob) && self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
        mob.set_no_action_time(0);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.inner.tick(mob, entity);
    }
}

/// Vanilla `Vindicator.isJohnny`, shared between the vindicator and its goal.
#[derive(Clone, Default)]
pub(crate) struct JohnnyFlag(Arc<AtomicBool>);

impl JohnnyFlag {
    #[must_use]
    pub(crate) fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn set(&self, johnny: bool) {
        self.0.store(johnny, Ordering::Relaxed);
    }
}

/// Vanilla `Vindicator.VindicatorJohnnyAttackGoal`: a vindicator named
/// "Johnny" attacks every living thing it can reach.
pub(crate) struct VindicatorJohnnyAttackGoal {
    inner: NearestAttackableTargetGoal,
    johnny: JohnnyFlag,
}

impl VindicatorJohnnyAttackGoal {
    #[must_use]
    pub(crate) fn new(johnny: JohnnyFlag) -> Self {
        Self {
            inner: NearestAttackableTargetGoal::new_with_interval(0, true, true, |target, _| {
                target.attackable()
            }),
            johnny,
        }
    }
}

impl Goal for VindicatorJohnnyAttackGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.johnny.get() && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
        mob.set_no_action_time(0);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }
}
