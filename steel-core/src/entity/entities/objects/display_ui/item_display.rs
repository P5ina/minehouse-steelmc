//! Vanilla `Display.ItemDisplay`: renders one item stack.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_data::ItemDisplayEntityData;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::display::{load_display, save_display, tick_display};
use crate::entity::{Entity, EntityBase, EntityBaseLoad, EntitySyncedData};
use crate::world::World;

const TAG_ITEM: &str = "item";
const TAG_ITEM_DISPLAY: &str = "item_display";

/// Vanilla `ItemDisplayContext`: which item model transform the display uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemDisplayContext {
    /// No transform.
    None,
    /// Held in the left hand, seen by others.
    ThirdPersonLeftHand,
    /// Held in the right hand, seen by others.
    ThirdPersonRightHand,
    /// Held in the left hand, seen by the holder.
    FirstPersonLeftHand,
    /// Held in the right hand, seen by the holder.
    FirstPersonRightHand,
    /// Worn on the head.
    Head,
    /// Drawn in an inventory slot.
    Gui,
    /// Lying on the ground.
    Ground,
    /// In an item frame.
    Fixed,
    /// On a shelf.
    OnShelf,
}

impl ItemDisplayContext {
    const ALL: [Self; 10] = [
        Self::None,
        Self::ThirdPersonLeftHand,
        Self::ThirdPersonRightHand,
        Self::FirstPersonLeftHand,
        Self::FirstPersonRightHand,
        Self::Head,
        Self::Gui,
        Self::Ground,
        Self::Fixed,
        Self::OnShelf,
    ];

    /// The synced byte id.
    #[must_use]
    pub const fn id(self) -> i8 {
        self as i8
    }

    /// Vanilla `BY_ID`: out-of-range ids fall back to `NONE`.
    #[must_use]
    pub fn by_id(id: i8) -> Self {
        usize::try_from(id)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
            .unwrap_or(Self::None)
    }

    /// The serialized name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ThirdPersonLeftHand => "thirdperson_lefthand",
            Self::ThirdPersonRightHand => "thirdperson_righthand",
            Self::FirstPersonLeftHand => "firstperson_lefthand",
            Self::FirstPersonRightHand => "firstperson_righthand",
            Self::Head => "head",
            Self::Gui => "gui",
            Self::Ground => "ground",
            Self::Fixed => "fixed",
            Self::OnShelf => "on_shelf",
        }
    }

    fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.name() == name)
    }
}

/// Vanilla `Display.ItemDisplay`.
#[entity_behavior(class = "ItemDisplay")]
pub struct ItemDisplayEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ItemDisplayEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ItemDisplayEntity`.
unsafe impl DowncastType for ItemDisplayEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/item_display");
}

impl ItemDisplayEntity {
    /// Creates an item display showing nothing.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(ItemDisplayEntityData::new()),
        }
    }

    /// Creates an item display from saved base data; `load_additional` follows.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(ItemDisplayEntityData::new()),
        }
    }

    /// Vanilla `ItemDisplay.getItemStack`.
    #[must_use]
    pub fn item_stack(&self) -> ItemStack {
        self.entity_data.lock().item_stack.get().clone()
    }

    /// Vanilla `ItemDisplay.setItemStack`.
    pub fn set_item_stack(&self, item: ItemStack) {
        self.entity_data.lock().item_stack.set(item);
    }

    /// Vanilla `ItemDisplay.getItemTransform`.
    #[must_use]
    pub fn item_transform(&self) -> ItemDisplayContext {
        ItemDisplayContext::by_id(*self.entity_data.lock().item_display.get())
    }

    /// Vanilla `ItemDisplay.setItemTransform`.
    pub fn set_item_transform(&self, transform: ItemDisplayContext) {
        self.entity_data.lock().item_display.set(transform.id());
    }
}

impl Entity for ItemDisplayEntity {
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
        let item = data.item_stack.get();
        if !item.is_empty() {
            nbt.insert(TAG_ITEM, item.to_nbt_tag_ref());
        }
        nbt.insert(
            TAG_ITEM_DISPLAY,
            ItemDisplayContext::by_id(*data.item_display.get()).name(),
        );
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let mut data = self.entity_data.lock();
        load_display(&mut data.display, nbt);
        data.item_stack.set(
            nbt.compound(TAG_ITEM)
                .and_then(|item| ItemStack::from_borrowed_compound(&item))
                .unwrap_or_else(ItemStack::empty),
        );
        data.item_display.set(
            nbt.string(TAG_ITEM_DISPLAY)
                .and_then(|name| ItemDisplayContext::by_name(&name.to_str()))
                .unwrap_or(ItemDisplayContext::None)
                .id(),
        );
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

    use super::super::display::tests::with_borrowed;
    use super::*;

    fn item_display() -> ItemDisplayEntity {
        ItemDisplayEntity::new(&vanilla_entities::ITEM_DISPLAY, 1, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn item_and_transform_round_trip() {
        init_vanilla_registry();
        let display = item_display();
        display.set_item_stack(ItemStack::with_count(&vanilla_items::DIAMOND, 3));
        display.set_item_transform(ItemDisplayContext::OnShelf);

        let mut nbt = NbtCompound::new();
        display.save_additional(&mut nbt);
        assert_eq!(
            nbt.string("item_display").map(|name| name.to_str()),
            Some("on_shelf".into())
        );

        let loaded = item_display();
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        let item = loaded.item_stack();
        assert!(item.is(&vanilla_items::DIAMOND));
        assert_eq!(item.count(), 3);
        assert_eq!(loaded.item_transform(), ItemDisplayContext::OnShelf);
    }

    #[test]
    fn an_empty_display_omits_the_item_and_unknown_transforms_load_none() {
        init_vanilla_registry();
        let display = item_display();
        let mut nbt = NbtCompound::new();
        display.save_additional(&mut nbt);
        assert!(!nbt.contains("item"));
        assert_eq!(
            nbt.string("item_display").map(|name| name.to_str()),
            Some("none".into())
        );

        let mut nbt = NbtCompound::new();
        nbt.insert("item_display", "upside_down");
        let loaded = item_display();
        loaded.set_item_transform(ItemDisplayContext::Head);
        with_borrowed(&nbt, |view| loaded.load_additional(view));
        assert_eq!(loaded.item_transform(), ItemDisplayContext::None);
        assert!(loaded.item_stack().is_empty());
    }
}
