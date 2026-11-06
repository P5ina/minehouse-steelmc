//! Vanilla `BreakDoorGoal`: a mob stuck at a wooden door hacks through it.

use steel_registry::{level_events, vanilla_game_rules::MOB_GRIEFING};
use steel_utils::types::{Difficulty, InteractionHand};

use super::door_interact::DoorInteractGoal;
use super::selector::{Goal, GoalControls};
use crate::entity::{PathfinderMob, SharedEntity};

/// Vanilla `BreakDoorGoal.DEFAULT_DOOR_BREAK_TIME`, in ticks.
const DEFAULT_DOOR_BREAK_TIME: i32 = 240;
const DOOR_REACH_DISTANCE: f64 = 2.0;
const BREAK_PROGRESS_STAGES: f32 = 10.0;
const KNOCK_CHANCE: i32 = 20;

pub(crate) struct BreakDoorGoal {
    door_interact: DoorInteractGoal,
    valid_difficulty: fn(Difficulty) -> bool,
    break_time: i32,
    last_break_progress: i32,
    door_break_time: i32,
}

impl BreakDoorGoal {
    #[must_use]
    pub(crate) const fn new(valid_difficulty: fn(Difficulty) -> bool) -> Self {
        Self {
            door_interact: DoorInteractGoal::new(),
            valid_difficulty,
            break_time: 0,
            last_break_progress: -1,
            door_break_time: -1,
        }
    }

    /// Vanilla `BreakDoorGoal(Mob, int, Predicate)`: a custom break time, never
    /// shorter than the default.
    #[must_use]
    pub(crate) const fn with_door_break_time(
        valid_difficulty: fn(Difficulty) -> bool,
        door_break_time: i32,
    ) -> Self {
        let mut goal = Self::new(valid_difficulty);
        goal.door_break_time = door_break_time;
        goal
    }

    const fn door_break_time(&self) -> i32 {
        if self.door_break_time > DEFAULT_DOOR_BREAK_TIME {
            self.door_break_time
        } else {
            DEFAULT_DOOR_BREAK_TIME
        }
    }

    fn is_valid_difficulty(&self, mob: &dyn PathfinderMob) -> bool {
        mob.level()
            .is_some_and(|world| (self.valid_difficulty)(world.difficulty()))
    }
}

impl Goal for BreakDoorGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn is_break_door_goal(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if !self.door_interact.can_use(mob) {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        world.get_game_rule(&MOB_GRIEFING)
            && self.is_valid_difficulty(mob)
            && !self.door_interact.is_open(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.door_interact.start(mob);
        self.break_time = 0;
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.break_time <= self.door_break_time()
            && !self.door_interact.is_open(mob)
            && self
                .door_interact
                .door_pos()
                .closer_to_center_than(mob.position(), DOOR_REACH_DISTANCE)
            && self.is_valid_difficulty(mob)
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(world) = mob.level() {
            world.broadcast_block_destruction(mob.id(), self.door_interact.door_pos(), -1);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.door_interact.tick(mob);
        let Some(world) = mob.level() else {
            return;
        };
        let door_pos = self.door_interact.door_pos();
        if rand::random_range(0..KNOCK_CHANCE) == 0 {
            world.level_event(level_events::SOUND_ZOMBIE_WOODEN_DOOR, door_pos, 0, None);
            if !mob.living_base().swing_state().swinging() {
                // Mobs never use items here, so vanilla's `getUsedItemHand` is the main hand.
                mob.swing_for_attack(InteractionHand::MainHand);
            }
        }

        self.break_time += 1;
        let progress =
            (self.break_time as f32 / self.door_break_time() as f32 * BREAK_PROGRESS_STAGES) as i32;
        if progress != self.last_break_progress {
            world.broadcast_block_destruction(mob.id(), door_pos, progress);
            self.last_break_progress = progress;
        }

        if self.break_time == self.door_break_time() && self.is_valid_difficulty(mob) {
            let _ = world.remove_block(door_pos, false);
            world.level_event(level_events::SOUND_ZOMBIE_DOOR_CRASH, door_pos, 0, None);
            // Vanilla reads the state after removing the door, so the particles use it.
            let state = world.get_block_state(door_pos);
            world.destroy_block_effect(door_pos, u32::from(state.0), None);
        }
    }
}
