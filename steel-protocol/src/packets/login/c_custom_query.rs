use std::io::Write;

use steel_macros::ClientPacket;
use steel_registry::packets::login::C_CUSTOM_QUERY;
use steel_utils::{Identifier, codec::VarInt, serial::WriteTo};

/// Login plugin request. The payload is the rest of the packet, without a length prefix.
#[derive(ClientPacket, Clone, Debug)]
#[packet_id(Login = C_CUSTOM_QUERY)]
pub struct CCustomQuery {
    pub transaction_id: i32,
    pub identifier: Identifier,
    pub payload: Box<[u8]>,
}

impl CCustomQuery {
    #[must_use]
    pub const fn new(transaction_id: i32, identifier: Identifier, payload: Box<[u8]>) -> Self {
        Self {
            transaction_id,
            identifier,
            payload,
        }
    }
}

impl WriteTo for CCustomQuery {
    fn write(&self, writer: &mut impl Write) -> std::io::Result<()> {
        VarInt(self.transaction_id).write(writer)?;
        self.identifier.write(writer)?;
        writer.write_all(&self.payload)
    }
}
