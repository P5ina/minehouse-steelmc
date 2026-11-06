//! Vanilla `SpellcasterIllager.SpellcasterCastingSpellGoal` and
//! `SpellcasterIllager.SpellcasterUseSpellGoal`.
//!
//! A spell goal starts a cast, waits out the warmup and then performs the
//! spell; the casting goal keeps the caster still and looking at its target
//! while the cast lasts.

use glam::DVec3;
use steel_registry::sound_event::SoundEventRef;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{IllagerSpell, PathfinderMob, SharedEntity};

/// Vanilla `SpellcasterUseSpellGoal.getCastWarmupTime` default.
pub(crate) const DEFAULT_CAST_WARMUP_TIME: i32 = 20;

/// Looks the caster at `entity`'s eyes with its head limits, like vanilla
/// `getLookControl().setLookAt(entity, getMaxHeadYRot(), getMaxHeadXRot())`.
pub(crate) fn look_at_entity(mob: &dyn PathfinderMob, entity: &SharedEntity) {
    let position = entity.position();
    let eye_y = entity.get_eye_y();
    let y_max = mob.max_head_y_rot();
    let x_max = mob.max_head_x_rot();
    mob.mob_base().controls().lock().look_control.set_look_at(
        DVec3::new(position.x, eye_y, position.z),
        y_max,
        x_max,
    );
}

/// What the caster looks at while casting when it has no target.
pub(crate) type CastingLookFallback =
    Box<dyn Fn(&dyn PathfinderMob) -> Option<SharedEntity> + Send + Sync>;

/// Vanilla `SpellcasterCastingSpellGoal`.
pub(crate) struct SpellcasterCastingSpellGoal {
    look_fallback: Option<CastingLookFallback>,
}

impl SpellcasterCastingSpellGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            look_fallback: None,
        }
    }

    /// Vanilla `Evoker.EvokerCastingSpellGoal`: also looks at `fallback` when
    /// there is no target.
    #[must_use]
    pub(crate) fn with_look_fallback(fallback: CastingLookFallback) -> Self {
        Self {
            look_fallback: Some(fallback),
        }
    }
}

impl Goal for SpellcasterCastingSpellGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.as_spellcaster_illager()
            .is_some_and(|caster| caster.spell_casting_time() > 0)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(caster) = mob.as_spellcaster_illager() {
            caster.set_is_casting_spell(IllagerSpell::None);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(target) = mob.target() {
            look_at_entity(mob, &target);
        } else if let Some(fallback) = self.look_fallback.as_ref()
            && let Some(entity) = fallback(mob)
        {
            look_at_entity(mob, &entity);
        }
    }
}

/// One spell of a [`SpellcasterUseSpellGoal`] (the abstract methods of vanilla
/// `SpellcasterUseSpellGoal`).
pub(crate) trait Spell: Send + Sync {
    /// Vanilla `getSpell`.
    fn spell(&self) -> IllagerSpell;

    /// Vanilla `getCastingTime`.
    fn casting_time(&self) -> i32;

    /// Vanilla `getCastingInterval`.
    fn casting_interval(&self) -> i32;

    /// Vanilla `getCastWarmupTime`.
    fn cast_warmup_time(&self) -> i32 {
        DEFAULT_CAST_WARMUP_TIME
    }

    /// Vanilla `getSpellPrepareSound`.
    fn prepare_sound(&self) -> Option<SoundEventRef>;

    /// Vanilla `canUse`; `default_can_use` is the `SpellcasterUseSpellGoal.canUse` body.
    fn can_use(
        &mut self,
        _mob: &dyn PathfinderMob,
        default_can_use: bool,
        _next_attack_tick_count: i32,
    ) -> bool {
        default_can_use
    }

    /// Vanilla `canContinueToUse`, given the remaining warmup.
    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob, attack_warmup_delay: i32) -> bool {
        mob.target().is_some_and(|target| target.is_alive()) && attack_warmup_delay > 0
    }

    /// Vanilla `stop`.
    fn stop(&mut self, _mob: &dyn PathfinderMob) {}

    /// Vanilla `performSpellCasting`; `entity` is the caster itself.
    fn perform(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity);
}

/// Vanilla `SpellcasterUseSpellGoal`.
pub(crate) struct SpellcasterUseSpellGoal<S: Spell> {
    spell: S,
    attack_warmup_delay: i32,
    next_attack_tick_count: i32,
}

impl<S: Spell> SpellcasterUseSpellGoal<S> {
    #[must_use]
    pub(crate) const fn new(spell: S) -> Self {
        Self {
            spell,
            attack_warmup_delay: 0,
            next_attack_tick_count: 0,
        }
    }

    /// Vanilla `SpellcasterUseSpellGoal.canUse`.
    fn default_can_use(&self, mob: &dyn PathfinderMob) -> bool {
        let Some(caster) = mob.as_spellcaster_illager() else {
            return false;
        };
        mob.target().is_some_and(|target| target.is_alive())
            && !caster.is_casting_spell()
            && mob.tick_count() >= self.next_attack_tick_count
    }
}

impl<S: Spell> Goal for SpellcasterUseSpellGoal<S> {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let default_can_use = self.default_can_use(mob);
        self.spell
            .can_use(mob, default_can_use, self.next_attack_tick_count)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.spell
            .can_continue_to_use(mob, self.attack_warmup_delay)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(caster) = mob.as_spellcaster_illager() else {
            return;
        };
        // Vanilla `adjustedTickDelay`: this goal is not ticked every tick.
        self.attack_warmup_delay = reduced_tick_delay(self.spell.cast_warmup_time());
        caster.set_spell_casting_time(self.spell.casting_time());
        self.next_attack_tick_count = mob.tick_count() + self.spell.casting_interval();
        if let Some(sound) = self.spell.prepare_sound() {
            mob.play_sound(sound, 1.0, 1.0);
        }
        caster.set_is_casting_spell(self.spell.spell());
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.spell.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.attack_warmup_delay -= 1;
        if self.attack_warmup_delay == 0 {
            self.spell.perform(mob, entity);
            if let Some(caster) = mob.as_spellcaster_illager() {
                mob.play_sound(caster.casting_sound(), 1.0, 1.0);
            }
        }
    }
}
