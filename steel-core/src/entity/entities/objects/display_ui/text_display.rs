//! Vanilla `Display.TextDisplay`: renders a text component.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_entity_data::TextDisplayEntityData;
use steel_utils::locks::SyncMutex;
use steel_utils::nbt::NbtNumeric as _;
use steel_utils::{DowncastType, DowncastTypeKey};
use text_components::TextComponent;

use super::display::{load_display, save_display, tick_display};
use crate::entity::{Entity, EntityBase, EntityBaseLoad, EntitySyncedData};
use crate::world::World;

const TAG_TEXT: &str = "text";
const TAG_LINE_WIDTH: &str = "line_width";
const TAG_TEXT_OPACITY: &str = "text_opacity";
const TAG_BACKGROUND_COLOR: &str = "background";
const TAG_SHADOW: &str = "shadow";
const TAG_SEE_THROUGH: &str = "see_through";
const TAG_USE_DEFAULT_BACKGROUND: &str = "default_background";
const TAG_ALIGNMENT: &str = "alignment";

/// Vanilla `TextDisplay.FLAG_SHADOW`.
pub const FLAG_SHADOW: i8 = 1;
/// Vanilla `TextDisplay.FLAG_SEE_THROUGH`.
pub const FLAG_SEE_THROUGH: i8 = 2;
/// Vanilla `TextDisplay.FLAG_USE_DEFAULT_BACKGROUND`.
pub const FLAG_USE_DEFAULT_BACKGROUND: i8 = 4;
/// Vanilla `TextDisplay.FLAG_ALIGN_LEFT`.
pub const FLAG_ALIGN_LEFT: i8 = 8;
/// Vanilla `TextDisplay.FLAG_ALIGN_RIGHT`.
pub const FLAG_ALIGN_RIGHT: i8 = 16;

/// Vanilla `TextDisplay.INITIAL_TEXT_OPACITY`.
const INITIAL_TEXT_OPACITY: i8 = -1;
/// Vanilla `TextDisplay.INITIAL_BACKGROUND`.
const INITIAL_BACKGROUND: i32 = 0x4000_0000;
/// Vanilla `TextDisplay.INITIAL_LINE_WIDTH`.
const INITIAL_LINE_WIDTH: i32 = 200;

/// Vanilla `TextDisplay.Align`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Lines are centered.
    Center,
    /// Lines start at the left edge.
    Left,
    /// Lines end at the right edge.
    Right,
}

impl Align {
    /// Vanilla `TextDisplay.getAlign`: `LEFT` wins over `RIGHT`.
    #[must_use]
    pub const fn from_flags(flags: i8) -> Self {
        if flags & FLAG_ALIGN_LEFT != 0 {
            Self::Left
        } else if flags & FLAG_ALIGN_RIGHT != 0 {
            Self::Right
        } else {
            Self::Center
        }
    }

    /// The serialized name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    fn by_name(name: &str) -> Option<Self> {
        [Self::Center, Self::Left, Self::Right]
            .into_iter()
            .find(|value| value.name() == name)
    }
}

/// Vanilla `Display.TextDisplay`.
#[entity_behavior(class = "TextDisplay")]
pub struct TextDisplayEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<TextDisplayEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TextDisplayEntity`.
unsafe impl DowncastType for TextDisplayEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/text_display");
}

impl TextDisplayEntity {
    /// Creates a text display with empty text.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(TextDisplayEntityData::new()),
        }
    }

    /// Creates a text display from saved base data; `load_additional` follows.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(TextDisplayEntityData::new()),
        }
    }

    /// Vanilla `TextDisplay.getText`.
    #[must_use]
    pub fn text(&self) -> TextComponent {
        (**self.entity_data.lock().text.get()).clone()
    }

    /// Vanilla `TextDisplay.setText`.
    pub fn set_text(&self, text: TextComponent) {
        self.entity_data.lock().text.set(Box::new(text));
    }

    /// Vanilla `TextDisplay.getFlags`.
    #[must_use]
    pub fn flags(&self) -> i8 {
        *self.entity_data.lock().style_flags.get()
    }

    /// Vanilla `TextDisplay.setFlags`.
    pub fn set_flags(&self, flags: i8) {
        self.entity_data.lock().style_flags.set(flags);
    }
}

impl Entity for TextDisplayEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn tick(self: Arc<Self>) {
        tick_display(&*self);
    }

    fn no_physics(&self) -> bool {
        true
    }

    fn piston_push_reaction(&self) -> PushReaction {
        PushReaction::IgnoreEntity
    }

    fn is_ignoring_block_triggers(&self) -> bool {
        true
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let data = self.entity_data.lock();
        save_display(&data.display, nbt);
        nbt.insert(TAG_TEXT, data.text.get().to_codec_nbt());
        nbt.insert(TAG_LINE_WIDTH, *data.line_width.get());
        nbt.insert(TAG_BACKGROUND_COLOR, *data.background_color.get());
        nbt.insert(TAG_TEXT_OPACITY, *data.text_opacity.get());
        let flags = *data.style_flags.get();
        nbt.insert(TAG_SHADOW, i8::from(flags & FLAG_SHADOW != 0));
        nbt.insert(TAG_SEE_THROUGH, i8::from(flags & FLAG_SEE_THROUGH != 0));
        nbt.insert(
            TAG_USE_DEFAULT_BACKGROUND,
            i8::from(flags & FLAG_USE_DEFAULT_BACKGROUND != 0),
        );
        nbt.insert(TAG_ALIGNMENT, Align::from_flags(flags).name());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let flag = |key, mask| {
            if nbt
                .get(key)
                .and_then(|tag| tag.byte_value())
                .is_some_and(|value| value != 0)
            {
                mask
            } else {
                0
            }
        };
        let mut data = self.entity_data.lock();
        load_display(&mut data.display, nbt);
        data.line_width.set(
            nbt.get(TAG_LINE_WIDTH)
                .and_then(|tag| tag.int_value())
                .unwrap_or(INITIAL_LINE_WIDTH),
        );
        data.text_opacity.set(
            nbt.get(TAG_TEXT_OPACITY)
                .and_then(|tag| tag.byte_value())
                .unwrap_or(INITIAL_TEXT_OPACITY),
        );
        data.background_color.set(
            nbt.get(TAG_BACKGROUND_COLOR)
                .and_then(|tag| tag.int_value())
                .unwrap_or(INITIAL_BACKGROUND),
        );
        let mut flags = flag(TAG_SHADOW, FLAG_SHADOW)
            | flag(TAG_SEE_THROUGH, FLAG_SEE_THROUGH)
            | flag(TAG_USE_DEFAULT_BACKGROUND, FLAG_USE_DEFAULT_BACKGROUND);
        match nbt
            .string(TAG_ALIGNMENT)
            .and_then(|name| Align::by_name(&name.to_str()))
        {
            Some(Align::Left) => flags |= FLAG_ALIGN_LEFT,
            Some(Align::Right) => flags |= FLAG_ALIGN_RIGHT,
            Some(Align::Center) | None => {}
        }
        data.style_flags.set(flags);
        // TODO: vanilla resolves selector, score and NBT contents here through
        // `ComponentUtils.resolve` with the display's own command source at
        // gamemaster permission; Steel has no entity command source yet.
        if let Some(text) = nbt
            .get(TAG_TEXT)
            .and_then(|tag| TextComponent::from_nbt(&tag.to_owned()))
        {
            data.text.set(Box::new(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_entities};

    use super::super::display::tests::with_borrowed;
    use super::*;

    fn text_display() -> TextDisplayEntity {
        TextDisplayEntity::new(&vanilla_entities::TEXT_DISPLAY, 1, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn style_flags_save_as_named_booleans_and_load_back() {
        init_vanilla_registry();
        let display = text_display();
        display.set_text(TextComponent::plain("Hello"));
        display.set_flags(FLAG_SHADOW | FLAG_USE_DEFAULT_BACKGROUND | FLAG_ALIGN_RIGHT);

        let mut nbt = NbtCompound::new();
        display.save_additional(&mut nbt);
        assert_eq!(nbt.byte("shadow"), Some(1));
        assert_eq!(nbt.byte("see_through"), Some(0));
        assert_eq!(nbt.byte("default_background"), Some(1));
        assert_eq!(
            nbt.string("alignment").map(|name| name.to_str()),
            Some("right".into())
        );
        assert_eq!(nbt.int("background"), Some(INITIAL_BACKGROUND));
        assert_eq!(nbt.int("line_width"), Some(INITIAL_LINE_WIDTH));

        let loaded = text_display();
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        assert_eq!(loaded.text(), TextComponent::plain("Hello"));
        assert_eq!(
            loaded.flags(),
            FLAG_SHADOW | FLAG_USE_DEFAULT_BACKGROUND | FLAG_ALIGN_RIGHT
        );
    }

    #[test]
    fn left_alignment_wins_over_right_and_missing_text_keeps_the_current_one() {
        init_vanilla_registry();
        assert_eq!(
            Align::from_flags(FLAG_ALIGN_LEFT | FLAG_ALIGN_RIGHT),
            Align::Left
        );

        let loaded = text_display();
        loaded.set_text(TextComponent::plain("kept"));
        let mut nbt = NbtCompound::new();
        nbt.insert("alignment", "left");
        nbt.insert("text_opacity", 300);
        with_borrowed(&nbt, |view| loaded.load_additional(view));

        assert_eq!(loaded.text(), TextComponent::plain("kept"));
        assert_eq!(loaded.flags(), FLAG_ALIGN_LEFT);
        assert_eq!(
            *loaded.entity_data.lock().text_opacity.get(),
            300_i32 as i8,
            "vanilla keeps the low byte of an out-of-range opacity"
        );
    }
}
