//! Vanilla `ServerLevel` village distance queries over the POI storage.

use std::sync::Arc;

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

impl World {
    /// Vanilla `ServerLevel.getRaids`.
    #[must_use]
    pub const fn raids(&self) -> &super::raid::Raids {
        &self.raids
    }

    /// Vanilla `ServerLevel.getRaidAt`.
    #[must_use]
    pub fn get_raid_at(&self, pos: BlockPos) -> Option<Arc<super::raid::Raid>> {
        self.raids
            .get_nearby_raid(pos, super::raid::VALID_RAID_RADIUS_SQR)
    }

    /// Vanilla `ServerLevel.isRaided`.
    #[must_use]
    pub fn is_raided(&self, pos: BlockPos) -> bool {
        self.get_raid_at(pos).is_some()
    }
}
