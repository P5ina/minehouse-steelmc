//! Spell-casting illagers (Vanilla `SpellcasterIllager`): evokers and illusioners.

use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::sound_event::SoundEventRef;

use crate::entity::AbstractIllager;

const TAG_SPELL_TICKS: &str = "SpellTicks";

/// Vanilla `SpellcasterIllager.IllagerSpell`; the id is synced to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IllagerSpell {
    /// Not casting.
    None = 0,
    /// The evoker summons vexes.
    SummonVex = 1,
    /// The evoker conjures fangs.
    Fangs = 2,
    /// The evoker turns a blue sheep red.
    Wololo = 3,
    /// The illusioner turns invisible and splits into mirror images.
    Disappear = 4,
    /// The illusioner blinds its target.
    Blindness = 5,
}

impl IllagerSpell {
    /// Vanilla `IllagerSpell.byId` with `OutOfBoundsStrategy.ZERO`.
    #[must_use]
    pub const fn by_id(id: u8) -> Self {
        match id {
            1 => Self::SummonVex,
            2 => Self::Fangs,
            3 => Self::Wololo,
            4 => Self::Disappear,
            5 => Self::Blindness,
            _ => Self::None,
        }
    }

    /// The id vanilla syncs in `DATA_SPELL_CASTING_ID`.
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }
}

/// Server-side spell state of a spellcaster.
pub struct SpellcasterState {
    spell_casting_tick_count: AtomicI32,
    current_spell: AtomicU8,
}

impl SpellcasterState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            spell_casting_tick_count: AtomicI32::new(0),
            current_spell: AtomicU8::new(IllagerSpell::None.id()),
        }
    }
}

impl Default for SpellcasterState {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `SpellcasterIllager`.
pub trait SpellcasterIllager: AbstractIllager {
    fn spellcaster_state(&self) -> &SpellcasterState;

    /// Writes vanilla `DATA_SPELL_CASTING_ID` to the synced entity data.
    fn set_synced_spell(&self, spell_id: i8);

    /// Vanilla `getCastingSoundEvent`.
    fn casting_sound(&self) -> SoundEventRef;

    /// Vanilla `getSpellCastingTime`.
    fn spell_casting_time(&self) -> i32 {
        self.spellcaster_state()
            .spell_casting_tick_count
            .load(Ordering::Relaxed)
    }

    /// Sets vanilla `spellCastingTickCount`.
    fn set_spell_casting_time(&self, ticks: i32) {
        self.spellcaster_state()
            .spell_casting_tick_count
            .store(ticks, Ordering::Relaxed);
    }

    /// Vanilla `isCastingSpell` on the server.
    fn is_casting_spell(&self) -> bool {
        self.spell_casting_time() > 0
    }

    /// Vanilla `getCurrentSpell` on the server.
    fn current_spell(&self) -> IllagerSpell {
        IllagerSpell::by_id(
            self.spellcaster_state()
                .current_spell
                .load(Ordering::Relaxed),
        )
    }

    /// Vanilla `setIsCastingSpell`.
    fn set_is_casting_spell(&self, spell: IllagerSpell) {
        self.spellcaster_state()
            .current_spell
            .store(spell.id(), Ordering::Relaxed);
        self.set_synced_spell(spell.id() as i8);
    }

    /// Vanilla `SpellcasterIllager.customServerAiStep`: counts the cast down.
    fn custom_server_ai_step_spellcaster(&self) {
        let ticks = self.spell_casting_time();
        if ticks > 0 {
            self.set_spell_casting_time(ticks - 1);
        }
    }

    /// Vanilla `SpellcasterIllager.addAdditionalSaveData`, after the raider fields.
    fn save_spellcaster(&self, nbt: &mut NbtCompound) {
        self.save_raider(nbt);
        nbt.insert(TAG_SPELL_TICKS, self.spell_casting_time());
    }

    /// Vanilla `SpellcasterIllager.readAdditionalSaveData`, after the raider fields.
    fn load_spellcaster(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_raider(nbt);
        self.set_spell_casting_time(nbt.int(TAG_SPELL_TICKS).unwrap_or(0));
    }
}
