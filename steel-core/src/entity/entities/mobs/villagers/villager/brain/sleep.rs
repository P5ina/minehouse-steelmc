//! Sleeping in the home bed (vanilla `SleepInBed`).

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::vanilla_block_tags::BlockTag;

use super::super::VillagerEntity;
use super::poi::closer_to_center_than;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::behaviors::close_doors_that_i_have_opened_or_passed_through;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    CANT_REACH_WALK_TARGET_SINCE, DOORS_TO_CLOSE, HOME, LAST_SLEPT, LAST_WOKEN,
    NEAREST_LIVING_ENTITIES, WALK_TARGET,
};
use crate::entity::ai::brain::{Activity, MemoryStatus};
use crate::entity::{Entity as _, LivingEntity as _};
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
