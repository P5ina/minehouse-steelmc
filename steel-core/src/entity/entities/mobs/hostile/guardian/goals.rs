//! Goals of vanilla `Guardian` and `ElderGuardian`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glam::DVec3;
use steel_registry::vanilla_damage_types;
use steel_utils::entity_events::EntityStatus;
use steel_utils::types::Difficulty;

use super::{GUARDIAN_ATTACK_LOOK_ROT, GUARDIAN_MIN_ATTACK_DISTANCE_SQR};
use crate::entity::ai::goal::{Goal, GoalControls, RandomStrollGoal};
use crate::entity::damage::DamageSource;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};

/// First attack tick: the beam starts ten ticks after the goal starts.
const ATTACK_WARMUP_TICKS: i32 = -10;
/// Vanilla beam damage before difficulty and elder bonuses.
const BASE_BEAM_DAMAGE: f32 = 1.0;
/// Extra beam damage on hard difficulty, and again for an elder guardian.
const BEAM_DAMAGE_BONUS: f32 = 2.0;

/// The `MOVE | LOOK` flags guardians set on their stroll and restriction goals.
fn move_and_look() -> GoalControls {
    GoalControls::MOVE | GoalControls::LOOK
}

/// Points the look control at `target`'s eyes, as vanilla
/// `LookControl.setLookAt(Entity, 90, 90)` does.
fn look_at_target(mob: &dyn PathfinderMob, target: &SharedEntity) {
    let position = target.position();
    mob.mob_base().controls().lock().look_control.set_look_at(
        DVec3::new(position.x, target.get_eye_y(), position.z),
        GUARDIAN_ATTACK_LOOK_ROT,
        GUARDIAN_ATTACK_LOOK_ROT,
    );
}

/// Vanilla `Guardian.GuardianAttackGoal`: charges the beam while the target
/// stays in sight, then deals magic damage plus a melee hit.
pub(super) struct GuardianAttackGoal {
    attack_time: i32,
    elder: bool,
}

impl GuardianAttackGoal {
    pub(super) const fn new(elder: bool) -> Self {
        Self {
            attack_time: 0,
            elder,
        }
    }
}

impl Goal for GuardianAttackGoal {
    fn controls(&self) -> GoalControls {
        move_and_look()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_some_and(|target| {
            target
                .as_living_entity()
                .is_some_and(LivingEntity::is_alive)
        })
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob)
            && (self.elder
                || mob.target().is_some_and(|target| {
                    mob.position().distance_squared(target.position())
                        > GUARDIAN_MIN_ATTACK_DISTANCE_SQR
                }))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.attack_time = ATTACK_WARMUP_TICKS;
        mob.mob_base().navigation().lock().stop();
        if let Some(target) = mob.target() {
            look_at_target(mob, &target);
        }
        mob.mark_velocity_sync();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(guardian) = mob.as_guardian() else {
            return;
        };
        guardian.set_active_attack_target(0);
        mob.set_target(None);
        guardian.trigger_random_stroll();
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        let Some(guardian) = mob.as_guardian() else {
            return;
        };

        mob.mob_base().navigation().lock().stop();
        look_at_target(mob, &target);
        if !mob.has_line_of_sight(target.as_ref()) {
            mob.set_target(None);
            return;
        }

        self.attack_time += 1;
        if self.attack_time == 0 {
            guardian.set_active_attack_target(target.id());
            if !mob.is_silent() {
                mob.broadcast_entity_event(EntityStatus::GuardianAttackSound);
            }
        } else if self.attack_time >= guardian.attack_duration() {
            let Some(world) = mob.level() else {
                return;
            };
            let mut magic_damage = BASE_BEAM_DAMAGE;
            if world.difficulty() == Difficulty::Hard {
                magic_damage += BEAM_DAMAGE_BONUS;
            }
            if self.elder {
                magic_damage += BEAM_DAMAGE_BONUS;
            }

            let beam = DamageSource::environment(&vanilla_damage_types::INDIRECT_MAGIC)
                .with_direct_entity(Arc::clone(entity))
                .with_causing_entity(Arc::clone(entity));
            target.hurt(&world, &beam, magic_damage);
            let _ = mob.do_hurt_target(entity, &world, &target);
            mob.set_target(None);
        }
    }
}

/// Vanilla guardian `RandomStrollGoal`: flagged `MOVE | LOOK`, and forced to
/// run at once when the guardian is hurt or loses its target.
pub(super) struct GuardianRandomStrollGoal {
    stroll: RandomStrollGoal,
    trigger: Arc<AtomicBool>,
}

impl GuardianRandomStrollGoal {
    pub(super) const fn new(stroll: RandomStrollGoal, trigger: Arc<AtomicBool>) -> Self {
        Self { stroll, trigger }
    }
}

impl Goal for GuardianRandomStrollGoal {
    fn controls(&self) -> GoalControls {
        move_and_look()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self.trigger.swap(false, Ordering::Relaxed) {
            self.stroll.trigger();
        }
        self.stroll.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.stroll.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.stroll.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.stroll.stop(mob);
    }
}

/// A goal whose flags vanilla replaces with `MOVE | LOOK` through `setFlags`.
pub(super) struct MoveAndLookGoal<G> {
    goal: G,
}

impl<G> MoveAndLookGoal<G> {
    pub(super) const fn new(goal: G) -> Self {
        Self { goal }
    }
}

impl<G: Goal> Goal for MoveAndLookGoal<G> {
    fn controls(&self) -> GoalControls {
        move_and_look()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.goal.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.goal.can_continue_to_use(mob)
    }

    fn is_interruptable(&self) -> bool {
        self.goal.is_interruptable()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.goal.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.goal.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.goal.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.goal.tick(mob, entity);
    }
}
