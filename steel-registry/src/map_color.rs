//! Vanilla `MapColor` ids and brightness packing.

/// Vanilla `MapColor`, identified by its id (0 to 61).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapColor(u8);

impl MapColor {
    /// Vanilla `MapColor.NONE`.
    pub const NONE: Self = Self(0);
    /// Vanilla `MapColor.WATER`.
    pub const WATER: Self = Self(12);
    /// Vanilla `MapColor.COLOR_ORANGE`.
    pub const COLOR_ORANGE: Self = Self(15);
    /// Vanilla `MapColor.COLOR_BROWN`.
    pub const COLOR_BROWN: Self = Self(26);

    /// Wraps an extracted map color id.
    #[must_use]
    pub const fn from_id(id: u8) -> Self {
        Self(id)
    }

    /// Vanilla `MapColor.id`.
    #[must_use]
    pub const fn id(self) -> u8 {
        self.0
    }

    /// Vanilla `MapColor.getPackedId`.
    #[must_use]
    pub const fn packed_id(self, brightness: MapColorBrightness) -> u8 {
        (self.0 << 2) | (brightness as u8 & 3)
    }
}

/// Vanilla `MapColor.Brightness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MapColorBrightness {
    Low = 0,
    Normal = 1,
    High = 2,
    Lowest = 3,
}

#[cfg(test)]
mod tests {
    use super::{MapColor, MapColorBrightness};

    #[test]
    fn packed_ids_match_vanilla() {
        assert_eq!(MapColor::WATER.packed_id(MapColorBrightness::High), 50);
        assert_eq!(MapColor::NONE.packed_id(MapColorBrightness::Normal), 1);
        assert_eq!(
            MapColor::COLOR_BROWN.packed_id(MapColorBrightness::Lowest),
            107
        );
    }
}
