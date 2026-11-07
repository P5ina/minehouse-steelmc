//! JSON to NBT conversion, vanilla `JsonOps.convertTo(NbtOps.INSTANCE, ...)`.
//!
//! Datapack JSON is decoded by codecs that Steel implements over owned NBT, so
//! a JSON value is first turned into the NBT shape those codecs read.

use serde_json::Value;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};

/// Converts a JSON value into NBT.
///
/// Whole numbers become `Int` (or `Long` outside the `i32` range) and other
/// numbers `Double`; Steel's codecs read any numeric tag. Booleans become
/// bytes, as `NbtOps.createBoolean` does. Returns `None` for `null`, which
/// has no NBT form.
#[must_use]
pub fn json_to_nbt(value: &Value) -> Option<NbtTag> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(value) => NbtTag::Byte(i8::from(*value)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                i32::try_from(value).map_or(NbtTag::Long(value), NbtTag::Int)
            } else {
                NbtTag::Double(number.as_f64()?)
            }
        }
        Value::String(value) => NbtTag::String(value.as_str().into()),
        Value::Array(values) => {
            let tags = values.iter().map(json_to_nbt).collect::<Option<Vec<_>>>()?;
            if tags.is_empty() {
                NbtTag::List(NbtList::Empty)
            } else {
                NbtTag::List(NbtList::from(tags))
            }
        }
        Value::Object(entries) => {
            let mut compound = NbtCompound::new();
            for (key, value) in entries {
                compound.insert(key.as_str(), json_to_nbt(value)?);
            }
            NbtTag::Compound(compound)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_json_shapes_like_json_ops() {
        let tag = json_to_nbt(&json!({
            "count": 3,
            "ratio": 0.5,
            "lit": true,
            "name": "minecraft:stone",
            "levels": [1, 2],
            "empty": [],
        }))
        .expect("object converts");
        let NbtTag::Compound(compound) = tag else {
            panic!("expected a compound");
        };
        assert_eq!(compound.get("count"), Some(&NbtTag::Int(3)));
        assert_eq!(compound.get("ratio"), Some(&NbtTag::Double(0.5)));
        assert_eq!(compound.get("lit"), Some(&NbtTag::Byte(1)));
        assert_eq!(
            compound.get("levels"),
            Some(&NbtTag::List(NbtList::Int(vec![1, 2])))
        );
        assert_eq!(compound.get("empty"), Some(&NbtTag::List(NbtList::Empty)));
        assert!(json_to_nbt(&Value::Null).is_none());
    }
}
