//! Serverbound packet for picking a trade in the merchant screen.

use steel_macros::{ReadFrom, ServerPacket};

/// Vanilla `ServerboundSelectTradePacket`: the index of the offer the player clicked.
#[derive(ServerPacket, ReadFrom, Clone, Debug)]
pub struct SSelectTrade {
    #[read(as = VarInt)]
    pub item: i32,
}
