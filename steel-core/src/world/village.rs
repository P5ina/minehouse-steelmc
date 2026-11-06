//! Vanilla `ServerLevel` village distance queries over the POI storage.

use steel_utils::{BlockPos, SectionPos};

use super::World;
use crate::poi::MAX_VILLAGE_DISTANCE;

impl World {
    /// Vanilla `ServerLevel.sectionsToVillage`.
    #[must_use]
    pub fn sections_to_village(&self, section: SectionPos) -> i32 {
        self.poi_storage.lock().sections_to_village(section)
    }

    /// Vanilla `ServerLevel.isCloseToVillage`.
    #[must_use]
    pub fn is_close_to_village(&self, pos: BlockPos, section_distance: i32) -> bool {
        section_distance <= MAX_VILLAGE_DISTANCE
            && self.sections_to_village(SectionPos::from_block_pos(pos)) <= section_distance
    }

    /// Vanilla `ServerLevel.isVillage(BlockPos)`.
    #[must_use]
    pub fn is_village(&self, pos: BlockPos) -> bool {
        self.is_close_to_village(pos, 1)
    }
}
