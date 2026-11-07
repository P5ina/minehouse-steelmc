use std::io::{Result, Write};

use steel_macros::ClientPacket;
use steel_registry::packets::play::C_BOSS_EVENT;
use steel_utils::{codec::VarInt, serial::WriteTo};
use text_components::TextComponent;
use uuid::Uuid;

/// Vanilla `BossEvent.BossBarColor`, in network id order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BossBarColor {
    /// `pink`
    Pink,
    /// `blue`
    Blue,
    /// `red`
    Red,
    /// `green`
    Green,
    /// `yellow`
    Yellow,
    /// `purple`
    Purple,
    /// `white`
    White,
}

/// Vanilla `BossEvent.BossBarOverlay`, in network id order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BossBarOverlay {
    /// `progress`
    Progress,
    /// `notched_6`
    Notched6,
    /// `notched_10`
    Notched10,
    /// `notched_12`
    Notched12,
    /// `notched_20`
    Notched20,
}

/// Vanilla boss bar property flags.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BossBarFlags {
    /// Darkens the sky (wither).
    pub darken_screen: bool,
    /// Plays boss music (ender dragon).
    pub play_boss_music: bool,
    /// Creates world fog (wither, ender dragon).
    pub create_world_fog: bool,
}

impl BossBarFlags {
    /// Vanilla `ClientboundBossEventPacket.encodeProperties`.
    #[must_use]
    pub const fn encode(self) -> u8 {
        let mut flags = 0;
        if self.darken_screen {
            flags |= 1;
        }
        if self.play_boss_music {
            flags |= 2;
        }
        if self.create_world_fog {
            flags |= 4;
        }
        flags
    }
}

/// One `ClientboundBossEventPacket` operation.
#[derive(Clone, Debug)]
pub enum BossEventOperation {
    /// Shows a new bar.
    Add {
        /// Bar title.
        name: TextComponent,
        /// Fill fraction in `0..=1`.
        progress: f32,
        /// Bar color.
        color: BossBarColor,
        /// Bar segment overlay.
        overlay: BossBarOverlay,
        /// Property flags.
        flags: BossBarFlags,
    },
    /// Hides the bar.
    Remove,
    /// Changes the fill fraction.
    UpdateProgress(f32),
    /// Changes the title.
    UpdateName(TextComponent),
    /// Changes color and overlay.
    UpdateStyle {
        /// Bar color.
        color: BossBarColor,
        /// Bar segment overlay.
        overlay: BossBarOverlay,
    },
    /// Changes the property flags.
    UpdateProperties(BossBarFlags),
}

/// Adds, removes or updates a boss bar on the client.
#[derive(ClientPacket, Clone, Debug)]
#[packet_id(Play = C_BOSS_EVENT)]
pub struct CBossEvent {
    /// Bar identity.
    pub id: Uuid,
    /// What changes.
    pub operation: BossEventOperation,
}

impl WriteTo for CBossEvent {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.id.write(writer)?;
        match &self.operation {
            BossEventOperation::Add {
                name,
                progress,
                color,
                overlay,
                flags,
            } => {
                VarInt(0).write(writer)?;
                name.write(writer)?;
                progress.write(writer)?;
                VarInt(*color as i32).write(writer)?;
                VarInt(*overlay as i32).write(writer)?;
                flags.encode().write(writer)?;
            }
            BossEventOperation::Remove => VarInt(1).write(writer)?,
            BossEventOperation::UpdateProgress(progress) => {
                VarInt(2).write(writer)?;
                progress.write(writer)?;
            }
            BossEventOperation::UpdateName(name) => {
                VarInt(3).write(writer)?;
                name.write(writer)?;
            }
            BossEventOperation::UpdateStyle { color, overlay } => {
                VarInt(4).write(writer)?;
                VarInt(*color as i32).write(writer)?;
                VarInt(*overlay as i32).write(writer)?;
            }
            BossEventOperation::UpdateProperties(flags) => {
                VarInt(5).write(writer)?;
                flags.encode().write(writer)?;
            }
        }
        Ok(())
    }
}
