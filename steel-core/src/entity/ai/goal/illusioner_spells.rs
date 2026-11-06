//! Illusioner spells (vanilla `Illusioner.IllusionerMirrorSpellGoal` and
//! `Illusioner.IllusionerBlindnessSpellGoal`).

use steel_registry::sound_event::SoundEventRef;
use steel_registry::{sound_events, vanilla_mob_effects};
use steel_utils::types::Difficulty;

use super::spellcaster_goals::Spell;
use crate::entity::{IllagerSpell, MobEffectInstance, PathfinderMob, SharedEntity};

/// Vanilla `MobEffectInstance(MobEffects.INVISIBILITY, 1200)`.
const MIRROR_INVISIBILITY_TICKS: i32 = 1200;
/// Vanilla `MobEffectInstance(MobEffects.BLINDNESS, 400)`.
const BLINDNESS_TICKS: i32 = 400;

/// Vanilla `Illusioner.IllusionerMirrorSpellGoal`: turn invisible.
pub(crate) struct MirrorSpell;

impl MirrorSpell {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl Spell for MirrorSpell {
    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Disappear
    }

    fn casting_time(&self) -> i32 {
        20
    }

    fn casting_interval(&self) -> i32 {
        340
    }

    fn prepare_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ILLUSIONER_PREPARE_MIRROR)
    }

    fn can_use(
        &mut self,
        mob: &dyn PathfinderMob,
        default_can_use: bool,
        _next_attack_tick_count: i32,
    ) -> bool {
        default_can_use && !mob.has_mob_effect(vanilla_mob_effects::INVISIBILITY)
    }

    fn perform(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        mob.add_mob_effect(MobEffectInstance::with_duration(
            vanilla_mob_effects::INVISIBILITY,
            MIRROR_INVISIBILITY_TICKS,
            0,
        ));
    }
}

/// Vanilla `Illusioner.IllusionerBlindnessSpellGoal`: blind each new target
/// once, on hard local difficulty.
pub(crate) struct BlindnessSpell {
    last_target_id: i32,
}

impl BlindnessSpell {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { last_target_id: 0 }
    }
}

impl Spell for BlindnessSpell {
    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Blindness
    }

    fn casting_time(&self) -> i32 {
        20
    }

    fn casting_interval(&self) -> i32 {
        180
    }

    fn prepare_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ILLUSIONER_PREPARE_BLINDNESS)
    }

    fn can_use(
        &mut self,
        mob: &dyn PathfinderMob,
        default_can_use: bool,
        _next_attack_tick_count: i32,
    ) -> bool {
        if !default_can_use {
            return false;
        }
        let Some(target) = mob.target() else {
            return false;
        };
        if target.id() == self.last_target_id {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        // VANILLA: `isHarderThan(Difficulty.NORMAL.ordinal())`, the ordinal as a float.
        world
            .current_difficulty_at(mob.block_position())
            .is_harder_than(f32::from(Difficulty::Normal as u8))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(target) = mob.target() {
            self.last_target_id = target.id();
        }
    }

    fn perform(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        if let Some(living) = target.as_living_entity() {
            living.add_mob_effect_with_source(
                MobEffectInstance::with_duration(
                    vanilla_mob_effects::BLINDNESS,
                    BLINDNESS_TICKS,
                    0,
                ),
                Some(mob),
            );
        }
    }
}
