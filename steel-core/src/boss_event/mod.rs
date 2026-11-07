//! Vanilla `ServerBossEvent`: a boss bar shown to a tracked set of players.

use std::sync::Arc;
use std::{mem, ptr};

use steel_protocol::packets::game::{
    BossBarColor, BossBarFlags, BossBarOverlay, BossEventOperation, CBossEvent,
};
use steel_utils::locks::SyncMutex;
use text_components::TextComponent;
use uuid::Uuid;

use crate::player::Player;

struct BossEventState {
    name: TextComponent,
    progress: f32,
    color: BossBarColor,
    overlay: BossBarOverlay,
    flags: BossBarFlags,
    visible: bool,
    players: Vec<Arc<Player>>,
}

impl BossEventState {
    fn add_operation(&self) -> BossEventOperation {
        BossEventOperation::Add {
            name: self.name.clone(),
            progress: self.progress,
            color: self.color,
            overlay: self.overlay,
            flags: self.flags,
        }
    }
}

/// Vanilla `ServerBossEvent`.
pub struct ServerBossEvent {
    id: Uuid,
    state: SyncMutex<BossEventState>,
}

impl ServerBossEvent {
    /// Creates a visible bar with no players, full progress and no flags.
    #[must_use]
    pub fn new(
        id: Uuid,
        name: TextComponent,
        color: BossBarColor,
        overlay: BossBarOverlay,
    ) -> Self {
        Self {
            id,
            state: SyncMutex::new(BossEventState {
                name,
                progress: 1.0,
                color,
                overlay,
                flags: BossBarFlags::default(),
                visible: true,
                players: Vec::new(),
            }),
        }
    }

    /// Returns the bar's network id.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the bar's title.
    #[must_use]
    pub fn name(&self) -> TextComponent {
        self.state.lock().name.clone()
    }

    /// Returns the bar's fill fraction.
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.state.lock().progress
    }

    /// Returns whether the bar is shown to its players.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.state.lock().visible
    }

    /// Returns the players this bar is tracked for.
    #[must_use]
    pub fn players(&self) -> Vec<Arc<Player>> {
        self.state.lock().players.clone()
    }

    fn send(&self, player: &Player, operation: BossEventOperation) {
        player.send_packet(CBossEvent {
            id: self.id,
            operation,
        });
    }

    fn broadcast(&self, state: &BossEventState, operation: &BossEventOperation) {
        if state.visible {
            for player in &state.players {
                self.send(player, operation.clone());
            }
        }
    }

    /// Vanilla `setProgress`.
    #[expect(clippy::float_cmp, reason = "Vanilla resends only on an exact change")]
    pub fn set_progress(&self, progress: f32) {
        let mut state = self.state.lock();
        if progress != state.progress {
            state.progress = progress;
            self.broadcast(&state, &BossEventOperation::UpdateProgress(progress));
        }
    }

    /// Vanilla `setName`.
    pub fn set_name(&self, name: TextComponent) {
        let mut state = self.state.lock();
        if state.name != name {
            state.name = name.clone();
            self.broadcast(&state, &BossEventOperation::UpdateName(name));
        }
    }

    /// Vanilla `setColor`.
    pub fn set_color(&self, color: BossBarColor) {
        let mut state = self.state.lock();
        if state.color != color {
            state.color = color;
            let operation = BossEventOperation::UpdateStyle {
                color,
                overlay: state.overlay,
            };
            self.broadcast(&state, &operation);
        }
    }

    /// Vanilla `setOverlay`.
    pub fn set_overlay(&self, overlay: BossBarOverlay) {
        let mut state = self.state.lock();
        if state.overlay != overlay {
            state.overlay = overlay;
            let operation = BossEventOperation::UpdateStyle {
                color: state.color,
                overlay,
            };
            self.broadcast(&state, &operation);
        }
    }

    /// Vanilla `setDarkenScreen` / `setPlayBossMusic` / `setCreateWorldFog`.
    pub fn set_flags(&self, flags: BossBarFlags) {
        let mut state = self.state.lock();
        if state.flags != flags {
            state.flags = flags;
            self.broadcast(&state, &BossEventOperation::UpdateProperties(flags));
        }
    }

    /// Vanilla `addPlayer`.
    pub fn add_player(&self, player: &Arc<Player>) {
        let mut state = self.state.lock();
        if state
            .players
            .iter()
            .any(|tracked| Arc::ptr_eq(tracked, player))
        {
            return;
        }
        state.players.push(Arc::clone(player));
        if state.visible {
            self.send(player, state.add_operation());
        }
    }

    /// Vanilla `removePlayer`.
    pub fn remove_player(&self, player: &Player) {
        let mut state = self.state.lock();
        let Some(index) = state
            .players
            .iter()
            .position(|tracked| ptr::eq(Arc::as_ptr(tracked), player))
        else {
            return;
        };
        state.players.swap_remove(index);
        if state.visible {
            self.send(player, BossEventOperation::Remove);
        }
    }

    /// Vanilla `removeAllPlayers`.
    pub fn remove_all_players(&self) {
        let mut state = self.state.lock();
        let players = mem::take(&mut state.players);
        if state.visible {
            for player in &players {
                self.send(player, BossEventOperation::Remove);
            }
        }
    }

    /// Vanilla `setVisible`.
    pub fn set_visible(&self, visible: bool) {
        let mut state = self.state.lock();
        if state.visible == visible {
            return;
        }
        state.visible = visible;
        for player in &state.players {
            let operation = if visible {
                state.add_operation()
            } else {
                BossEventOperation::Remove
            };
            self.send(player, operation);
        }
    }
}

#[cfg(test)]
mod tests;
