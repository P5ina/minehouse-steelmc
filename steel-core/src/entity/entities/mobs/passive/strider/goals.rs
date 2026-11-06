//! Strider-specific goals.

use crate::entity::ai::goal::{Goal, GoalControls, MoveToBlockGoal};
use crate::entity::{PathfinderMob, SharedEntity};

use super::is_lava_target;

/// Vanilla `Strider.StriderGoToLavaGoal`: walk back to the lava when out of it.
pub(super) struct StriderGoToLavaGoal {
    inner: MoveToBlockGoal,
}

impl StriderGoToLavaGoal {
    pub(super) fn new(speed_modifier: f64) -> Self {
        Self {
            inner: MoveToBlockGoal::with_vertical_search_range(
                speed_modifier,
                8,
                2,
                is_lava_target,
            )
            .with_move_to_target(|pos| pos)
            .with_recalculate_path_interval(20),
        }
    }
}

impl Goal for StriderGoToLavaGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.is_in_lava() && self.inner.can_use(mob)
    }

    /// Unlike `MoveToBlockGoal`, vanilla never gives up on the way back.
    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.is_in_lava()
            && mob
                .level()
                .is_some_and(|world| is_lava_target(&world, self.inner.block_pos()))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.inner.tick(mob, entity);
    }
}
