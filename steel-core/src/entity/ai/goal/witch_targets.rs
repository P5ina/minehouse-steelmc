//! The witch's two target goals (Vanilla `NearestHealableRaiderTargetGoal` and
//! `NearestAttackableWitchTargetGoal`).
//!
//! The witch's `aiStep` drives them: it counts the heal cooldown down and only lets
//! the player goal run once the cooldown is over. Both sides share that state.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use super::nearest_attackable_target::NearestAttackableTargetGoal;
use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{LivingEntity, PathfinderMob, Raider};
use crate::world::World;

/// Vanilla `NearestHealableRaiderTargetGoal` random interval.
const HEAL_RAIDER_RANDOM_INTERVAL: i32 = 500;
/// Vanilla `NearestHealableRaiderTargetGoal.DEFAULT_COOLDOWN`.
const HEAL_RAIDER_COOLDOWN: i32 = 200;

/// Heal cooldown shared between the witch and its heal goal.
#[derive(Clone, Default)]
pub(crate) struct HealCooldown(Arc<AtomicI32>);

impl HealCooldown {
    /// Vanilla `getCooldown`.
    #[must_use]
    pub(crate) fn get(&self) -> i32 {
        self.0.load(Ordering::Relaxed)
    }

    /// Vanilla `decrementCooldown`.
    pub(crate) fn decrement(&self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }

    fn set(&self, cooldown: i32) {
        self.0.store(cooldown, Ordering::Relaxed);
    }
}

/// Whether the witch may look for players, shared with its player goal.
#[derive(Clone)]
pub(crate) struct CanAttackFlag(Arc<AtomicBool>);

impl CanAttackFlag {
    /// Vanilla `setCanAttack`.
    pub(crate) fn set(&self, can_attack: bool) {
        self.0.store(can_attack, Ordering::Relaxed);
    }

    fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl Default for CanAttackFlag {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

/// Vanilla `NearestHealableRaiderTargetGoal`: targets a fellow raider to heal.
pub(crate) struct NearestHealableRaiderTargetGoal {
    inner: NearestAttackableTargetGoal,
    cooldown: HealCooldown,
}

impl NearestHealableRaiderTargetGoal {
    /// Vanilla `new NearestHealableRaiderTargetGoal<>(raider, Raider.class, mustSee, selector)`;
    /// the selector only ever sees raiders.
    #[must_use]
    pub(crate) fn new(
        must_see: bool,
        cooldown: HealCooldown,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: NearestAttackableTargetGoal::new_with_interval(
                HEAL_RAIDER_RANDOM_INTERVAL,
                must_see,
                false,
                move |target, world| target.as_raider().is_some() && selector(target, world),
            ),
            cooldown,
        }
    }
}

impl Goal for NearestHealableRaiderTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self.cooldown.get() > 0 || !rand::random::<bool>() {
            return false;
        }
        if !mob.as_raider().is_some_and(Raider::has_active_raid) {
            return false;
        }
        self.inner.find_target(mob);
        self.inner.has_target()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.cooldown.set(reduced_tick_delay(HEAL_RAIDER_COOLDOWN));
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }
}

/// Vanilla `NearestAttackableWitchTargetGoal`: a player target goal the witch can pause.
pub(crate) struct NearestAttackableWitchTargetGoal {
    inner: NearestAttackableTargetGoal,
    can_attack: CanAttackFlag,
}

impl NearestAttackableWitchTargetGoal {
    /// Vanilla `new NearestAttackableWitchTargetGoal<>(this, Player.class, interval, mustSee,
    /// mustReach, null)`.
    #[must_use]
    pub(crate) fn new_for_players(
        random_interval: i32,
        must_see: bool,
        must_reach: bool,
        can_attack: CanAttackFlag,
    ) -> Self {
        Self {
            inner: NearestAttackableTargetGoal::new_for_players_with_interval(
                random_interval,
                must_see,
                must_reach,
                |_, _| true,
            ),
            can_attack,
        }
    }
}

impl Goal for NearestAttackableWitchTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_attack.get() && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }
}
