use steel_macros::{ClientPacket, WriteTo};
use steel_registry::packets::play::C_MOUNT_SCREEN_OPEN;

/// Vanilla `ClientboundMountScreenOpenPacket`: opens a mount's inventory screen.
#[derive(ClientPacket, WriteTo, Clone, Debug)]
#[packet_id(Play = C_MOUNT_SCREEN_OPEN)]
pub struct CMountScreenOpen {
    #[write(as = VarInt)]
    pub container_id: i32,
    /// Columns of the mount's own storage; 0 when it has none.
    #[write(as = VarInt)]
    pub inventory_columns: i32,
    pub entity_id: i32,
}
