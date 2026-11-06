use super::random_swimming::RandomSwimmingGoal;
use super::selector::{Goal, GoalControls};
use crate::entity::{AbstractFish, PathfinderMob};

const FISH_SWIM_SPEED_MODIFIER: f64 = 1.0;
const FISH_SWIM_INTERVAL: i32 = 40;

/// Vanilla `AbstractFish.FishSwimGoal`: random swimming unless the fish
/// follows a school leader.
pub struct FishSwimGoal {
    swimming: RandomSwimmingGoal,
}

impl FishSwimGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            swimming: RandomSwimmingGoal::new(FISH_SWIM_SPEED_MODIFIER, FISH_SWIM_INTERVAL),
        }
    }
}

impl Goal for FishSwimGoal {
    fn controls(&self) -> GoalControls {
        self.swimming.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.as_abstract_fish()
            .is_some_and(AbstractFish::can_random_swim)
            && self.swimming.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.swimming.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.swimming.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.swimming.stop(mob);
    }
}
