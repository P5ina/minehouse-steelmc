//! Ender chest menu.

use std::sync::Arc;

use steel_utils::{DowncastType, DowncastTypeKey};

use crate::inventory::ender_chest::{ENDER_CHEST_SLOTS, SyncPlayerEnderChest};
use crate::inventory::menu::kinds::chest_menu::chest_with_kind;
use crate::inventory::prelude::*;
use crate::player::player_inventory::PlayerInventory;

/// Builds the menu over a player's ender chest container.
#[must_use]
pub fn ender_chest(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: SyncPlayerEnderChest,
) -> Menu {
    chest_with_kind(
        inventory,
        container_id,
        Arc::clone(&container),
        ENDER_CHEST_SLOTS / 9,
        EnderChestKind { container },
    )
}

/// Per-menu ender chest state: the player's container, which knows which block
/// entity the menu was opened from.
pub struct EnderChestKind {
    container: SyncPlayerEnderChest,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl DowncastType for EnderChestKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/ender_chest");
}

impl MenuKind for EnderChestKind {
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.container.lock().still_valid(player)
    }

    /// `PlayerEnderChestContainer.startOpen`: the active chest counts the
    /// viewer. The container lock is released first, since the chest's viewer
    /// check reads it back.
    fn start_open(&mut self, player: &Player) {
        let active_chest = self.container.lock().active_chest();
        if let Some(active_chest) = active_chest {
            active_chest.start_open(player);
        }
    }

    /// `PlayerEnderChestContainer.stopOpen`: the active chest drops the viewer,
    /// then the container forgets it so a later validity check can't consult
    /// a stale one.
    fn removed(&mut self, _behavior: &mut MenuBehavior, player: &Player) {
        let active_chest = self.container.lock().active_chest();
        if let Some(active_chest) = active_chest {
            active_chest.stop_open(player);
        }
        self.container.lock().clear_active_chest();
    }
}
