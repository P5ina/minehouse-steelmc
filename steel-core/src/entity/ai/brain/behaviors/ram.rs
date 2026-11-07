//! Vanilla `PrepareRamNearestTarget`: walks to a run-up spot in line with
//! the nearest ramable target and lowers the head.

use std::sync::Arc;

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::sound_event::SoundEventRef;
use steel_utils::BlockPos;
use steel_utils::Direction;
use steel_utils::entity_events::EntityStatus;

use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker, RAM_COOLDOWN_TICKS, RAM_TARGET,
    WALK_TARGET, WalkTarget,
};
use crate::entity::ai::goal::random_pos::has_malus;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{PathfinderMob, SharedEntity};
use crate::world::World;

/// Vanilla `PrepareRamNearestTarget.TIME_OUT_DURATION`.
const TIME_OUT_DURATION: i32 = 160;

const PREPARE_RAM_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&RAM_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&RAM_TARGET, MemoryStatus::ValueAbsent),
];

/// Vanilla `PrepareRamNearestTarget.RamCandidate`.
struct RamCandidate {
    start_position: BlockPos,
    target_position: BlockPos,
    target: SharedEntity,
}

/// Vanilla `PrepareRamNearestTarget`.
pub struct PrepareRamNearestTarget<E> {
    get_cooldown_on_fail: fn(&E) -> i32,
    min_ram_distance: i32,
    max_ram_distance: i32,
    walk_speed: f32,
    ram_targeting: TargetingConditions,
    ram_prepare_time: i64,
    get_prepare_ram_sound: fn(&E) -> SoundEventRef,
    reached_ram_position_timestamp: Option<i64>,
    ram_candidate: Option<RamCandidate>,
}

impl<E: PathfinderMob> PrepareRamNearestTarget<E> {
    #[must_use]
    pub const fn new(
        get_cooldown_on_fail: fn(&E) -> i32,
        min_ram_distance: i32,
        max_ram_distance: i32,
        walk_speed: f32,
        ram_targeting: TargetingConditions,
        ram_prepare_time: i64,
        get_prepare_ram_sound: fn(&E) -> SoundEventRef,
    ) -> Self {
        Self {
            get_cooldown_on_fail,
            min_ram_distance,
            max_ram_distance,
            walk_speed,
            ram_targeting,
            ram_prepare_time,
            get_prepare_ram_sound,
            reached_ram_position_timestamp: None,
            ram_candidate: None,
        }
    }

    /// Vanilla `isWalkableBlock`.
    fn is_walkable_block(body: &E, pos: BlockPos) -> bool {
        body.is_stable_destination(pos) && !has_malus(body, pos)
    }

    /// Vanilla `calculateRammingStartPosition`.
    fn calculate_ramming_start_position(
        &self,
        body: &E,
        target: &SharedEntity,
    ) -> Option<BlockPos> {
        let target_pos = target.block_position();
        if !Self::is_walkable_block(body, target_pos) {
            return None;
        }
        let mut possible_ram_positions = Vec::new();
        for direction in Direction::HORIZONTAL {
            let mut furthest = target_pos;
            for _ in 0..self.max_ram_distance {
                furthest = furthest.relative(direction);
                if !Self::is_walkable_block(body, furthest) {
                    furthest = furthest.relative(direction.opposite());
                    break;
                }
            }
            let manhattan = (furthest.x() - target_pos.x()).abs()
                + (furthest.y() - target_pos.y()).abs()
                + (furthest.z() - target_pos.z()).abs();
            if manhattan >= self.min_ram_distance {
                possible_ram_positions.push(furthest);
            }
        }
        let body_pos = body.block_position();
        let dist_sqr = |pos: &BlockPos| {
            let dx = i64::from(pos.x() - body_pos.x());
            let dy = i64::from(pos.y() - body_pos.y());
            let dz = i64::from(pos.z() - body_pos.z());
            dx * dx + dy * dy + dz * dz
        };
        possible_ram_positions.sort_by_key(dist_sqr);
        possible_ram_positions.into_iter().find(|pos| {
            body.create_path_to(*pos, 0)
                .is_some_and(|path| path.can_reach())
        })
    }

    /// Vanilla `chooseRamPosition`.
    fn choose_ram_position(&mut self, body: &E, target: &SharedEntity) {
        self.reached_ram_position_timestamp = None;
        self.ram_candidate =
            self.calculate_ramming_start_position(body, target)
                .map(|start_position| RamCandidate {
                    start_position,
                    target_position: target.block_position(),
                    target: Arc::clone(target),
                });
    }
}

/// Vanilla `getEdgeOfBlock`: the near edge of the target's block.
fn edge_of_block(start_ram_pos: BlockPos, target_pos: BlockPos) -> DVec3 {
    let x_offset = 0.5 * f64::from((target_pos.x() - start_ram_pos.x()).signum());
    let z_offset = 0.5 * f64::from((target_pos.z() - start_ram_pos.z()).signum());
    let (x, y, z) = target_pos.get_bottom_center();
    DVec3::new(x + x_offset, y, z + z_offset)
}

impl<E: PathfinderMob> Behavior<E> for PrepareRamNearestTarget<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &PREPARE_RAM_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (TIME_OUT_DURATION, TIME_OUT_DURATION)
    }

    fn start(&mut self, world: &Arc<World>, body: &E, _time: i64) {
        let Some(visible) = body
            .brain_memories()
            .and_then(|memories| memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES))
        else {
            return;
        };
        let target = visible.find_closest(|entity| {
            entity
                .as_living_entity()
                .is_some_and(|target| self.ram_targeting.test(world, Some(body), target))
        });
        if let Some(target) = target {
            self.choose_ram_position(body, &target);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let has_ram_target = memories.lock().has_value(&RAM_TARGET);
        if !has_ram_target {
            body.broadcast_entity_event(EntityStatus::EndRam);
            let cooldown = (self.get_cooldown_on_fail)(body);
            memories.lock().set(&RAM_COOLDOWN_TICKS, Some(cooldown));
        }
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &E, _time: i64) -> bool {
        self.ram_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.target.is_alive())
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        let Some(candidate) = self.ram_candidate.as_ref() else {
            return;
        };
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let start_position = candidate.start_position;
        let target_position = candidate.target_position;
        let target = Arc::clone(&candidate.target);
        {
            let mut memories = memories.lock();
            memories.set(
                &WALK_TARGET,
                Some(WalkTarget::new(
                    PositionTracker::block(start_position),
                    self.walk_speed,
                    0,
                )),
            );
            memories.set(
                &LOOK_TARGET,
                Some(PositionTracker::entity(Arc::clone(&target), true)),
            );
        }
        if target.block_position() != target_position {
            body.broadcast_entity_event(EntityStatus::EndRam);
            body.mob_base().navigation().lock().stop();
            self.choose_ram_position(body, &target);
            return;
        }
        let start_ram_pos = body.block_position();
        if start_ram_pos != start_position {
            return;
        }
        body.broadcast_entity_event(EntityStatus::StartRam);
        let reached = *self.reached_ram_position_timestamp.get_or_insert(time);
        if time - reached >= self.ram_prepare_time {
            memories.lock().set(
                &RAM_TARGET,
                Some(edge_of_block(start_ram_pos, target_position)),
            );
            world.play_sound_at(
                (self.get_prepare_ram_sound)(body),
                SoundSource::Neutral,
                body.position(),
                1.0,
                body.voice_pitch(),
                None,
            );
            self.ram_candidate = None;
        }
    }
}
