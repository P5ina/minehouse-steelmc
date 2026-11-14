//! Beds: sleeping in the home bed (vanilla `SleepInBed`), and babies
//! finding and jumping on beds (`NearestBedSensor`, `JumpOnBed`).

use std::sync::Arc;

use rustc_hash::FxHashMap;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_utils::BlockPos;

use super::super::VillagerEntity;
use super::poi::{closer_to_center_than, is_home};
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::behaviors::{
    SCAN_RANGE, close_doors_that_i_have_opened_or_passed_through, find_path_to_pois,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    CANT_REACH_WALK_TARGET_SINCE, DOORS_TO_CLOSE, HOME, LAST_SLEPT, LAST_WOKEN, NEAREST_BED,
    NEAREST_LIVING_ENTITIES, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{Activity, Memories, MemoryStatus, Sensor};
use crate::entity::ai::path::Path;
use crate::entity::{AgeableMob, Entity as _, LivingEntity as _, Mob as _};
use crate::poi::OccupationStatus;
use crate::world::World;

/// Vanilla `SleepInBed.COOLDOWN_AFTER_BEING_WOKEN`.
const COOLDOWN_AFTER_BEING_WOKEN: i64 = 100;

const SLEEP_IN_BED_ENTRY: [MemoryRequirement; 5] = [
    MemoryRequirement::new(&HOME, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&LAST_WOKEN, MemoryStatus::Registered),
    MemoryRequirement::new(&LAST_SLEPT, MemoryStatus::Registered),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
];

/// Vanilla `SleepInBed`: lies down in the home bed once next to it, and
/// keeps sleeping while it rests and stays in the bed.
pub(super) struct SleepInBed {
    next_ok_start_time: i64,
}

impl SleepInBed {
    pub(super) const fn new() -> Self {
        Self {
            next_ok_start_time: 0,
        }
    }
}

impl Behavior<VillagerEntity> for SleepInBed {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SLEEP_IN_BED_ENTRY
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        if body.is_passenger() {
            return false;
        }
        let (home, last_woken) = {
            let memories = body.brain.memories().lock();
            (memories.get(&HOME), memories.get(&LAST_WOKEN))
        };
        let Some(home) = home else {
            return false;
        };
        if home.dimension != world.key {
            return false;
        }
        if let Some(last_woken) = last_woken {
            let time_since_last_woken = world.game_time() - last_woken;
            if time_since_last_woken > 0 && time_since_last_woken < COOLDOWN_AFTER_BEING_WOKEN {
                return false;
            }
        }
        let state = world.get_block_state(home.pos);
        closer_to_center_than(home.pos, body, 2.0)
            && state
                .get_block()
                .has_tag(&BlockTag::VILLAGERS_CAN_SLEEP_ON_BED)
            && !state.get_value(&BlockStateProperties::OCCUPIED)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        let Some(home) = body.brain.memories().lock().get(&HOME) else {
            return false;
        };
        body.brain.is_active(Activity::REST)
            && body.position().y > f64::from(home.pos.y()) + 0.4
            && closer_to_center_than(home.pos, body, 1.14)
    }

    /// Vanilla `timedOut` is always false: it sleeps until it stops resting.
    fn timed_out(&self, _time: i64, _end_time: i64) -> bool {
        false
    }

    fn start(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        if time <= self.next_ok_start_time {
            return;
        }
        let (doors, nearest, home) = {
            let memories = body.brain.memories().lock();
            (
                memories.get(&DOORS_TO_CLOSE),
                memories.get(&NEAREST_LIVING_ENTITIES),
                memories.get(&HOME),
            )
        };
        if let Some(mut doors) = doors {
            close_doors_that_i_have_opened_or_passed_through(
                world,
                body,
                None,
                None,
                &mut doors,
                nearest.as_deref(),
            );
            body.brain
                .memories()
                .lock()
                .set(&DOORS_TO_CLOSE, Some(doors));
        }
        let slept = home.is_some_and(|home| body.start_sleeping(home.pos).is_ok());
        let mut memories = body.brain.memories().lock();
        if slept {
            memories.set(&LAST_SLEPT, Some(time));
        }
        memories.erase(&WALK_TARGET);
        memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        if body.is_sleeping() {
            body.stop_sleeping();
            self.next_ok_start_time = time + 40;
        }
    }
}

/// Vanilla `NearestBedSensor`: every second a baby looks for the bed it can
/// reach most easily among up to four new beds within 48 blocks.
pub(super) struct NearestBedSensor {
    batch_cache: FxHashMap<BlockPos, i64>,
    tried_count: i32,
    last_update: i64,
}

impl NearestBedSensor {
    /// Vanilla `NearestBedSensor.CACHE_TIMEOUT`.
    const CACHE_TIMEOUT: i64 = 40;
    /// Vanilla `NearestBedSensor.BATCH_SIZE`.
    const BATCH_SIZE: i32 = 5;
    /// Vanilla `NearestBedSensor.RATE`.
    const RATE: i32 = 20;

    pub(super) fn new() -> Self {
        Self {
            batch_cache: FxHashMap::default(),
            tried_count: 0,
            last_update: 0,
        }
    }
}

impl Sensor<VillagerEntity> for NearestBedSensor {
    fn scan_rate(&self) -> i32 {
        Self::RATE
    }

    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_BED);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &VillagerEntity) {
        if !AgeableMob::is_baby(body) {
            return;
        }
        self.tried_count = 0;
        self.last_update = world.game_time() + i64::from(rand::random_range(0..Self::RATE));
        let body_pos = body.block_position();
        let in_range = world.poi_storage.lock().get_in_range(
            &is_home,
            body_pos,
            SCAN_RANGE,
            OccupationStatus::Any,
        );
        let pois: Vec<(BlockPos, usize)> = in_range
            .into_iter()
            .filter(|(pos, _)| {
                if self.batch_cache.contains_key(pos) {
                    return false;
                }
                self.tried_count += 1;
                if self.tried_count >= Self::BATCH_SIZE {
                    return false;
                }
                self.batch_cache
                    .insert(*pos, self.last_update + Self::CACHE_TIMEOUT);
                true
            })
            .collect();
        let path = find_path_to_pois(world, body, &pois);
        if let Some(path) = path.filter(Path::can_reach) {
            let target = path.target();
            if world.poi_storage.lock().get_type(target).is_some() {
                body.brain.memories().lock().set(&NEAREST_BED, Some(target));
            }
        } else if self.tried_count < Self::BATCH_SIZE {
            let last_update = self.last_update;
            self.batch_cache.retain(|_, expiry| *expiry >= last_update);
        }
    }
}

const JUMP_ON_BED_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&NEAREST_BED, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
];

/// Vanilla `JumpOnBed`: a baby walks to the nearest bed and jumps on it
/// three to six times.
pub(super) struct JumpOnBed {
    speed_modifier: f32,
    target_bed: Option<BlockPos>,
    remaining_time_to_reach_bed: i32,
    remaining_jumps: i32,
    remaining_cooldown_until_next_jump: i32,
}

impl JumpOnBed {
    /// Vanilla `JumpOnBed.MAX_TIME_TO_REACH_BED`.
    const MAX_TIME_TO_REACH_BED: i32 = 100;
    /// Vanilla `JumpOnBed.MIN_JUMPS`.
    const MIN_JUMPS: i32 = 3;
    /// Vanilla `JumpOnBed.MAX_JUMPS`.
    const MAX_JUMPS: i32 = 6;
    /// Vanilla `JumpOnBed.COOLDOWN_BETWEEN_JUMPS`.
    const COOLDOWN_BETWEEN_JUMPS: i32 = 5;

    pub(super) const fn new(speed_modifier: f32) -> Self {
        Self {
            speed_modifier,
            target_bed: None,
            remaining_time_to_reach_bed: 0,
            remaining_jumps: 0,
            remaining_cooldown_until_next_jump: 0,
        }
    }

    fn nearest_bed(body: &VillagerEntity) -> Option<BlockPos> {
        body.brain.memories().lock().get(&NEAREST_BED)
    }

    fn is_jumpable(world: &World, pos: BlockPos) -> bool {
        world
            .get_block_state(pos)
            .get_block()
            .has_tag(&BlockTag::VILLAGER_BABIES_CAN_JUMP_ON_BED)
    }

    fn on_or_over_bed(world: &World, body: &VillagerEntity) -> bool {
        let body_pos = body.block_position();
        Self::is_jumpable(world, body_pos) || Self::is_jumpable(world, body_pos.below())
    }

    fn on_bed_surface(world: &World, body: &VillagerEntity) -> bool {
        Self::is_jumpable(world, body.block_position())
    }

    fn tired_of_walking(&self, world: &World, body: &VillagerEntity) -> bool {
        !Self::on_or_over_bed(world, body) && self.remaining_time_to_reach_bed <= 0
    }

    fn tired_of_jumping(&self, world: &World, body: &VillagerEntity) -> bool {
        Self::on_or_over_bed(world, body) && self.remaining_jumps <= 0
    }
}

impl Behavior<VillagerEntity> for JumpOnBed {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &JUMP_ON_BED_ENTRY
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        AgeableMob::is_baby(body)
            && (Self::on_or_over_bed(world, body) || Self::nearest_bed(body).is_some())
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if let Some(target_bed) = Self::nearest_bed(body) {
            self.target_bed = Some(target_bed);
            self.remaining_time_to_reach_bed = Self::MAX_TIME_TO_REACH_BED;
            self.remaining_jumps =
                Self::MIN_JUMPS + rand::random_range(0..=Self::MAX_JUMPS - Self::MIN_JUMPS);
            self.remaining_cooldown_until_next_jump = 0;
            body.brain.memories().lock().set(
                &WALK_TARGET,
                Some(WalkTarget::new(
                    PositionTracker::block(target_bed),
                    self.speed_modifier,
                    0,
                )),
            );
        }
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        AgeableMob::is_baby(body)
            && self
                .target_bed
                .is_some_and(|bed| Self::is_jumpable(world, bed))
            && !self.tired_of_walking(world, body)
            && !self.tired_of_jumping(world, body)
    }

    /// Vanilla `timedOut` is always false.
    fn timed_out(&self, _time: i64, _end_time: i64) -> bool {
        false
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if !Self::on_or_over_bed(world, body) {
            self.remaining_time_to_reach_bed -= 1;
        } else if self.remaining_cooldown_until_next_jump > 0 {
            self.remaining_cooldown_until_next_jump -= 1;
        } else if Self::on_bed_surface(world, body) {
            body.mob_base().controls().lock().jump_control.jump();
            self.remaining_jumps -= 1;
            self.remaining_cooldown_until_next_jump = Self::COOLDOWN_BETWEEN_JUMPS;
        }
    }

    fn stop(&mut self, _world: &Arc<World>, _body: &VillagerEntity, _time: i64) {
        self.target_bed = None;
        self.remaining_time_to_reach_bed = 0;
        self.remaining_jumps = 0;
        self.remaining_cooldown_until_next_jump = 0;
    }
}
