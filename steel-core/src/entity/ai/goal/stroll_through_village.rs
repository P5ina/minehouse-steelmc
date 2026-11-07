//! Vanilla `StrollThroughVillageGoal`: at night, a mob near a village wanders
//! toward its center.

use glam::DVec3;
use steel_utils::{BlockPos, SectionPos};

use super::random_pos::land_random_pos_weighted;
use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::chunk::heightmap::HeightmapType;
use crate::entity::{PathfinderMob, SharedEntity};

const DISTANCE_THRESHOLD: f64 = 10.0;
const VILLAGE_SECTION_DISTANCE: i32 = 6;

pub(crate) struct StrollThroughVillageGoal {
    interval: i32,
    wanted_pos: Option<BlockPos>,
}

impl StrollThroughVillageGoal {
    #[must_use]
    pub(crate) const fn new(interval: i32) -> Self {
        Self {
            interval: reduced_tick_delay(interval),
            wanted_pos: None,
        }
    }

    fn move_randomly(mob: &dyn PathfinderMob) {
        let Some(world) = mob.level() else {
            return;
        };
        let offset = mob.block_position().offset(
            rand::random_range(0..16) - 8,
            0,
            rand::random_range(0..16) - 8,
        );
        let target = world.heightmap_pos(HeightmapType::MotionBlockingNoLeaves, offset);
        mob.move_to_pos(block_xyz(target), 1.0);
    }
}

fn block_xyz(pos: BlockPos) -> DVec3 {
    DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()))
}

impl Goal for StrollThroughVillageGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if mob.has_controlling_passenger() {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        if world.is_bright_outside() || rand::random_range(0..self.interval.max(1)) != 0 {
            return false;
        }
        if !world.is_close_to_village(mob.block_position(), VILLAGE_SECTION_DISTANCE) {
            return false;
        }
        let land_pos = land_random_pos_weighted(mob, 15, 7, |pos| {
            -f64::from(world.sections_to_village(SectionPos::from_block_pos(pos)))
        });
        self.wanted_pos = land_pos.map(|pos| BlockPos::containing(pos.x, pos.y, pos.z));
        self.wanted_pos.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(wanted_pos) = self.wanted_pos else {
            return false;
        };
        let navigation = mob.mob_base().navigation().lock();
        !navigation.is_done() && navigation.target_pos() == Some(wanted_pos)
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(wanted_pos) = self.wanted_pos else {
            return;
        };
        let done = mob.mob_base().navigation().lock().is_done();
        if !done || wanted_pos.closer_to_center_than(mob.position(), DISTANCE_THRESHOLD) {
            return;
        }
        let Some(world) = mob.level() else {
            return;
        };
        let (x, y, z) = wanted_pos.get_bottom_center();
        let mut long_distance_target = DVec3::new(x, y, z);
        let position = mob.position();
        long_distance_target += (position - long_distance_target) * 0.4;
        let move_target = (long_distance_target - position).normalize_or_zero() * 10.0 + position;
        let path_target = world.heightmap_pos(
            HeightmapType::MotionBlockingNoLeaves,
            BlockPos::containing(move_target.x, move_target.y, move_target.z),
        );
        if !mob.move_to_pos(block_xyz(path_target), 1.0) {
            Self::move_randomly(mob);
        }
    }
}
