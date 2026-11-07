//! Ender chest container implementation.

use std::ptr;
use std::sync::{Arc, Weak};

use steel_registry::item_stack::ItemStack;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::inventory::container::Container;
use crate::player::Player;

/// Number of slots in an ender chest (3 rows of 9).
pub const ENDER_CHEST_SLOTS: usize = 27;

type WeakBlockEntity = Weak<dyn BlockEntity>;

/// Thread-safe reference to a player's ender chest container.
pub type SyncPlayerEnderChest = Arc<SyncMutex<PlayerEnderChestContainer>>;

/// The player's ender chest inventory.
pub struct PlayerEnderChestContainer {
    items: Vec<ItemStack>,
    active_chest: Option<WeakBlockEntity>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PlayerEnderChestContainer`.
unsafe impl DowncastType for PlayerEnderChestContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:inventory/player_ender_chest");
}

impl Default for PlayerEnderChestContainer {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerEnderChestContainer {
    /// Creates a new, empty ender chest inventory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: vec![ItemStack::empty(); ENDER_CHEST_SLOTS],
            active_chest: None,
        }
    }

    /// Sets the block entity this container was most recently opened from.
    pub fn set_active_chest(&mut self, active_chest: WeakBlockEntity) {
        self.active_chest = Some(active_chest);
    }

    /// Clears the active block entity.
    pub fn clear_active_chest(&mut self) {
        self.active_chest = None;
    }

    /// The block entity this container was opened from, if it still exists.
    #[must_use]
    pub fn active_chest(&self) -> Option<Arc<dyn BlockEntity>> {
        self.active_chest.as_ref().and_then(Weak::upgrade)
    }

    /// Whether this container was opened from the block entity owning
    /// `chest`. Vanilla `isActiveChest`.
    #[must_use]
    pub fn is_active_chest(&self, chest: &BlockEntityBase) -> bool {
        self.active_chest()
            .is_some_and(|active| ptr::eq(active.base(), chest))
    }

    /// Checks if the container is still valid for the given player.
    #[must_use]
    pub fn still_valid(&self, player: &Player) -> bool {
        let Some(weak_chest) = &self.active_chest else {
            return true;
        };
        // A dropped weak handle means the chest was destroyed while open.
        let Some(chest) = weak_chest.upgrade() else {
            return false;
        };
        chest.base().is_valid_container_for(player)
    }
}

impl Container for PlayerEnderChestContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn set_changed(&mut self) {
        // Player data saving handles change tracking for this inventory.
    }
}
