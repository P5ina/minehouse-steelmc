//! Clientbound map contents update.

use std::io::{Result, Write};

use steel_macros::ClientPacket;
use steel_registry::RegistryReference;
use steel_registry::map_decoration_type::MapDecorationType;
use steel_registry::packets::play::C_MAP_ITEM_DATA;
use steel_utils::{codec::VarInt, serial::WriteTo};
use text_components::TextComponent;

/// Vanilla `MapDecoration.STREAM_CODEC`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDecorationData {
    pub decoration_type: RegistryReference<MapDecorationType>,
    pub x: i8,
    pub y: i8,
    /// Vanilla stores `rot & 15`.
    pub rot: i8,
    pub name: Option<TextComponent>,
}

impl WriteTo for MapDecorationData {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.decoration_type.write(writer)?;
        self.x.write(writer)?;
        self.y.write(writer)?;
        self.rot.write(writer)?;
        self.name.write(writer)
    }
}

/// Vanilla `MapItemSavedData.MapPatch`: a rectangle of packed map colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPatch {
    pub start_x: u8,
    pub start_y: u8,
    pub width: u8,
    pub height: u8,
    /// Row-major colors, `width * height` bytes.
    pub colors: Vec<u8>,
}

/// Vanilla `ClientboundMapItemDataPacket`.
#[derive(ClientPacket, Clone, Debug)]
#[packet_id(Play = C_MAP_ITEM_DATA)]
pub struct CMapItemData {
    pub map_id: i32,
    pub scale: i8,
    pub locked: bool,
    pub decorations: Option<Vec<MapDecorationData>>,
    pub color_patch: Option<MapPatch>,
}

impl WriteTo for CMapItemData {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        VarInt(self.map_id).write(writer)?;
        self.scale.write(writer)?;
        self.locked.write(writer)?;
        self.decorations.write(writer)?;
        // Vanilla `MapPatch.STREAM_CODEC`: a zero width means no patch.
        match &self.color_patch {
            Some(patch) => {
                patch.width.write(writer)?;
                patch.height.write(writer)?;
                patch.start_x.write(writer)?;
                patch.start_y.write(writer)?;
                VarInt(i32::try_from(patch.colors.len()).map_err(std::io::Error::other)?)
                    .write(writer)?;
                writer.write_all(&patch.colors)
            }
            None => 0u8.write(writer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_encodes_like_vanilla() {
        let packet = CMapItemData {
            map_id: 300,
            scale: 2,
            locked: true,
            decorations: None,
            color_patch: Some(MapPatch {
                start_x: 1,
                start_y: 2,
                width: 2,
                height: 1,
                colors: vec![7, 9],
            }),
        };
        let mut bytes = Vec::new();
        packet.write(&mut bytes).expect("encodes");
        assert_eq!(bytes, [0xAC, 0x02, 2, 1, 0, 2, 1, 1, 2, 2, 7, 9]);

        let empty = CMapItemData {
            map_id: 0,
            scale: 0,
            locked: false,
            decorations: Some(Vec::new()),
            color_patch: None,
        };
        let mut bytes = Vec::new();
        empty.write(&mut bytes).expect("encodes");
        assert_eq!(bytes, [0, 0, 0, 1, 0, 0]);
    }
}
