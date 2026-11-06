//! NBT helpers for vanilla save data.

use std::io::Cursor;

use simdnbt::owned::{Nbt, NbtCompound, NbtList, NbtTag, read};
use steel_utils::Identifier;

/// Reads a named root compound as vanilla writes it to region and `.dat` files.
pub fn read_root(bytes: &[u8]) -> Result<NbtCompound, String> {
    match read(&mut Cursor::new(bytes)) {
        Ok(Nbt::Some(root)) => Ok(root.as_compound()),
        Ok(Nbt::None) => Err("NBT root is empty".to_owned()),
        Err(error) => Err(format!("malformed NBT: {error}")),
    }
}

/// Returns the elements of a list as tags.
///
/// Vanilla writes a list whose elements have different types as a list of compounds, wrapping
/// each element that is not a compound into a compound with a single empty key.
pub fn list_elements(list: &NbtList) -> Vec<NbtTag> {
    list.as_nbt_tags()
        .into_iter()
        .map(|tag| match tag {
            NbtTag::Compound(compound) if compound.len() == 1 && compound.contains("") => compound
                .get("")
                .cloned()
                .unwrap_or(NbtTag::Compound(NbtCompound::new())),
            other => other,
        })
        .collect()
}

/// Reads a string field.
pub fn string(compound: &NbtCompound, key: &str) -> Option<String> {
    compound
        .string(key)
        .map(|value| value.to_str().into_owned())
}

/// Reads a string tag.
pub fn tag_string(tag: &NbtTag) -> Option<String> {
    tag.string().map(|value| value.to_str().into_owned())
}

/// Parses a namespaced id.
pub fn identifier(value: &str) -> Option<Identifier> {
    value.parse().ok()
}

/// Reads a byte as a boolean the way vanilla's `getBooleanOr` does.
pub fn boolean(compound: &NbtCompound, key: &str) -> bool {
    compound.byte(key).is_some_and(|value| value != 0)
}

/// Reads a list of doubles with an exact length.
pub fn doubles<const N: usize>(compound: &NbtCompound, key: &str) -> Option<[f64; N]> {
    compound.list(key)?.doubles()?.try_into().ok()
}

/// Reads a list of floats with an exact length.
pub fn floats<const N: usize>(compound: &NbtCompound, key: &str) -> Option<[f32; N]> {
    compound.list(key)?.floats()?.try_into().ok()
}

/// Number of bits needed to store `value` distinct values, `Mth.ceillog2`.
pub const fn ceil_log2(value: usize) -> u32 {
    if value <= 1 {
        0
    } else {
        (value - 1).bit_width()
    }
}

/// Unpacks vanilla `SimpleBitStorage` data: values never span two longs and fill each long
/// from the least significant bits.
pub fn unpack_bit_storage(data: &[i64], bits: u32, count: usize) -> Result<Vec<u32>, String> {
    if bits == 0 || bits > 32 {
        return Err(format!("invalid bit storage width {bits}"));
    }
    let values_per_long = (64 / bits) as usize;
    let expected = count.div_ceil(values_per_long);
    if data.len() != expected {
        return Err(format!(
            "bit storage has {} longs, expected {expected} for {count} values of {bits} bits",
            data.len()
        ));
    }
    let mask = (1u64 << bits) - 1;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let word = data[index / values_per_long] as u64;
        let shift = (index % values_per_long) as u32 * bits;
        values.push(((word >> shift) & mask) as u32);
    }
    Ok(values)
}

/// Serializes a compound the way Steel stores NBT payloads, or nothing for an empty compound.
pub fn compound_bytes(compound: &NbtCompound) -> Vec<u8> {
    if compound.is_empty() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    compound.write(&mut bytes);
    bytes
}

/// Removes the given keys from a compound.
pub fn without_keys(compound: &NbtCompound, keys: &[&str]) -> NbtCompound {
    let mut copy = compound.clone();
    for key in keys {
        let _ = copy.remove(key);
    }
    copy
}
