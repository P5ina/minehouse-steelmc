//! Frame markers: where a map hangs in an item frame.

use simdnbt::borrow::NbtCompound as BorrowedNbtCompound;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_utils::BlockPos;

/// Vanilla `MapFrame`: the item frame a map is placed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapFrame {
    /// The block the frame occupies.
    pub pos: BlockPos,
    /// The frame's facing as a yaw in degrees, a multiple of 90.
    pub rotation: i32,
    /// The entity id of the frame.
    pub entity_id: i32,
}

impl MapFrame {
    /// Vanilla `MapFrame.frameId`: the frame marker key of the frame at `pos`.
    #[must_use]
    pub fn frame_id(pos: BlockPos) -> String {
        format!("frame-{},{},{}", pos.x(), pos.y(), pos.z())
    }

    /// The frame marker key of this frame.
    #[must_use]
    pub fn id(&self) -> String {
        Self::frame_id(self.pos)
    }

    /// Vanilla `MapItemSavedData.getFrameKey`: the decoration key of a frame entity.
    #[must_use]
    pub fn decoration_key(entity_id: i32) -> String {
        format!("frame-{entity_id}")
    }

    /// Encodes vanilla `MapFrame.CODEC`.
    #[must_use]
    pub fn to_nbt(self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert(
            "pos",
            NbtTag::IntArray(vec![self.pos.x(), self.pos.y(), self.pos.z()]),
        );
        nbt.insert("rotation", self.rotation);
        nbt.insert("entity_id", self.entity_id);
        nbt
    }

    /// Decodes vanilla `MapFrame.CODEC`; every field is required.
    #[must_use]
    pub fn from_nbt(nbt: BorrowedNbtCompound<'_, '_>) -> Option<Self> {
        let pos = nbt.int_array("pos")?;
        let [x, y, z] = pos[..] else {
            return None;
        };
        Some(Self {
            pos: BlockPos::new(x, y, z),
            rotation: nbt.int("rotation")?,
            entity_id: nbt.int("entity_id")?,
        })
    }
}
