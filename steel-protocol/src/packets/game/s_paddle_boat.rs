use steel_macros::{ReadFrom, ServerPacket};

/// Which boat paddles the controlling player is moving (vanilla `ServerboundPaddleBoatPacket`).
#[derive(ReadFrom, ServerPacket, Clone, Debug)]
pub struct SPaddleBoat {
    pub left: bool,
    pub right: bool,
}
