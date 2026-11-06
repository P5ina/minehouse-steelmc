use std::io::{Cursor, Read};

use steel_macros::ServerPacket;
use steel_utils::{codec::VarInt, serial::ReadFrom};

/// Vanilla caps the answer payload at 1 MiB.
const MAX_PAYLOAD_SIZE: u64 = 1_048_576;

/// Login plugin response. `payload` is `None` when the client did not understand the query.
#[derive(ServerPacket, Clone, Debug)]
pub struct SCustomQueryAnswer {
    pub transaction_id: i32,
    pub payload: Option<Vec<u8>>,
}

impl ReadFrom for SCustomQueryAnswer {
    fn read(data: &mut Cursor<&[u8]>) -> std::io::Result<Self> {
        let transaction_id = VarInt::read(data)?.0;
        let payload = if bool::read(data)? {
            let mut buf = Vec::new();
            data.take(MAX_PAYLOAD_SIZE + 1).read_to_end(&mut buf)?;
            if buf.len() as u64 > MAX_PAYLOAD_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "custom query answer payload too large",
                ));
            }
            Some(buf)
        } else {
            None
        };
        Ok(Self {
            transaction_id,
            payload,
        })
    }
}
