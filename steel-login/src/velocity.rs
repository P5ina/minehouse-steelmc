//! Velocity modern forwarding: a proxy (Velocity, Gate) answers a login plugin query
//! with the player's profile and address, signed with a shared secret.

use std::{
    io::{Cursor, Read},
    net::IpAddr,
};

use sha2::{Digest, Sha256};
use steel_core::player::GameProfile;
use steel_utils::{
    Identifier,
    codec::VarInt,
    serial::{PrefixedRead as _, ReadFrom},
};

/// Forwarding version requested from the proxy: profile and address, no chat keys.
pub const MODERN_DEFAULT: u8 = 1;

const SIGNATURE_LEN: usize = 32;
const HMAC_BLOCK: usize = 64;
const MAX_ADDRESS_LEN: usize = 255;

/// The channel identifier for the forwarding query.
#[must_use]
pub const fn channel() -> Identifier {
    Identifier::new_static("velocity", "player_info")
}

/// The player data a proxy forwarded.
#[derive(Debug)]
pub struct ForwardedPlayer {
    /// The player's real address, as seen by the proxy.
    pub address: IpAddr,
    /// The profile the proxy authenticated.
    pub profile: GameProfile,
}

/// Why a forwarding answer was rejected.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForwardingError {
    /// The client did not understand the query, so it is not a proxy.
    #[error("the proxy did not answer the forwarding query")]
    NoAnswer,
    /// The answer was signed with another secret or altered.
    #[error("forwarding signature does not match the configured secret")]
    BadSignature,
    /// The proxy answered with a forwarding version we do not read.
    #[error("unsupported forwarding version {0}")]
    UnsupportedVersion(i32),
    /// The signed data could not be decoded.
    #[error("malformed forwarding data")]
    Malformed,
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let mut block = [0u8; HMAC_BLOCK];
    if key.len() > HMAC_BLOCK {
        block[..SIGNATURE_LEN].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; HMAC_BLOCK];
    let mut outer_pad = [0x5cu8; HMAC_BLOCK];
    for ((inner, outer), key_byte) in inner_pad.iter_mut().zip(&mut outer_pad).zip(block) {
        *inner ^= key_byte;
        *outer ^= key_byte;
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// Compares without an early exit so timing does not reveal the matching prefix.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// Verifies and decodes a forwarding answer.
///
/// # Errors
/// Returns an error if the answer is missing, unsigned, signed with another secret or malformed.
pub fn read_forwarded_player(
    secret: &[u8],
    payload: Option<&[u8]>,
) -> Result<ForwardedPlayer, ForwardingError> {
    let payload = payload.ok_or(ForwardingError::NoAnswer)?;
    if payload.len() < SIGNATURE_LEN {
        return Err(ForwardingError::Malformed);
    }
    let (signature, data) = payload.split_at(SIGNATURE_LEN);
    if !constant_time_eq(signature, &hmac_sha256(secret, data)) {
        return Err(ForwardingError::BadSignature);
    }

    let cursor = &mut Cursor::new(data);
    let version = VarInt::read(cursor)
        .map_err(|_| ForwardingError::Malformed)?
        .0;
    // Newer versions only append chat-key data after the profile; we asked for the default.
    if !(i32::from(MODERN_DEFAULT)..=4).contains(&version) {
        return Err(ForwardingError::UnsupportedVersion(version));
    }
    let address = String::read_prefixed_bound::<VarInt>(cursor, MAX_ADDRESS_LEN * 3)
        .map_err(|_| ForwardingError::Malformed)?;
    let address = address
        .parse::<IpAddr>()
        .map_err(|_| ForwardingError::Malformed)?;
    let profile = GameProfile::read(cursor).map_err(|_| ForwardingError::Malformed)?;
    if version == i32::from(MODERN_DEFAULT) && cursor.read(&mut [0u8; 1]).unwrap_or(0) != 0 {
        return Err(ForwardingError::Malformed);
    }
    Ok(ForwardedPlayer { address, profile })
}

#[cfg(test)]
mod tests {
    use steel_utils::serial::{PrefixedWrite as _, WriteTo as _};
    use uuid::Uuid;

    use super::*;

    const SECRET: &[u8] = b"forwarding-secret";

    fn body(version: i32, address: &str, id: Uuid, name: &str) -> Vec<u8> {
        let mut data = Vec::new();
        VarInt(version).write(&mut data).expect("write version");
        address
            .write_prefixed::<VarInt>(&mut data)
            .expect("write address");
        GameProfile::new(id, name.to_owned(), Vec::new())
            .expect("valid profile")
            .write(&mut data)
            .expect("write profile");
        data
    }

    fn signed(secret: &[u8], data: &[u8]) -> Vec<u8> {
        let mut payload = hmac_sha256(secret, data).to_vec();
        payload.extend_from_slice(data);
        payload
    }

    #[test]
    fn hmac_matches_rfc_4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn accepts_a_correctly_signed_answer() {
        let id = Uuid::from_u128(42);
        let payload = signed(SECRET, &body(1, "203.0.113.7", id, "Steve"));

        let player = read_forwarded_player(SECRET, Some(&payload)).expect("valid answer");
        assert_eq!(player.address, "203.0.113.7".parse::<IpAddr>().expect("ip"));
        assert_eq!(player.profile.id(), id);
        assert_eq!(player.profile.name(), "Steve");
    }

    #[test]
    fn rejects_another_secret() {
        let payload = signed(b"other", &body(1, "203.0.113.7", Uuid::nil(), "Steve"));
        assert_eq!(
            read_forwarded_player(SECRET, Some(&payload)).expect_err("answer should be rejected"),
            ForwardingError::BadSignature
        );
    }

    #[test]
    fn rejects_tampered_data() {
        let mut payload = signed(SECRET, &body(1, "203.0.113.7", Uuid::nil(), "Steve"));
        let last = payload.len() - 1;
        payload[last] ^= 1;
        assert_eq!(
            read_forwarded_player(SECRET, Some(&payload)).expect_err("answer should be rejected"),
            ForwardingError::BadSignature
        );
    }

    #[test]
    fn rejects_a_client_that_does_not_answer() {
        assert_eq!(
            read_forwarded_player(SECRET, None).expect_err("answer should be rejected"),
            ForwardingError::NoAnswer
        );
    }

    #[test]
    fn rejects_unknown_versions() {
        let payload = signed(SECRET, &body(9, "203.0.113.7", Uuid::nil(), "Steve"));
        assert_eq!(
            read_forwarded_player(SECRET, Some(&payload)).expect_err("answer should be rejected"),
            ForwardingError::UnsupportedVersion(9)
        );
    }
}
