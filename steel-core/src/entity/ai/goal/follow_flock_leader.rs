use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{AbstractSchoolingFish, PathfinderMob, SharedEntity};

const INTERVAL_TICKS: i32 = 200;
const RECALCULATE_PATH_TICKS: i32 = 10;

/// Vanilla `FollowFlockLeaderGoal`: a schooling fish joins or forms a school
/// and keeps pathing to its leader.
pub struct FollowFlockLeaderGoal {
    time_to_recalc_path: i32,
    next_start_tick: i32,
}

impl FollowFlockLeaderGoal {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            time_to_recalc_path: 0,
            next_start_tick: Self::next_start_tick(),
        }
    }

    /// Vanilla rolls `nextInt(200) % 20`, so the spread is 0..20 ticks.
    fn next_start_tick() -> i32 {
        reduced_tick_delay(INTERVAL_TICKS + rand::random_range(0..INTERVAL_TICKS) % 20)
    }

    fn find_school(fish: &dyn AbstractSchoolingFish) -> bool {
        let Some(world) = fish.level() else {
            return false;
        };
        let Some(this) = world.get_entity_by_id(fish.id()) else {
            return false;
        };
        let candidates: Vec<SharedEntity> = fish
            .nearby_school_fish(&world)
            .into_iter()
            .filter(|other| {
                other
                    .as_schooling_fish()
                    .is_some_and(|other| other.can_be_followed() || !other.is_follower())
            })
            .collect();
        let leader = candidates
            .iter()
            .find(|other| {
                other
                    .as_schooling_fish()
                    .is_some_and(AbstractSchoolingFish::can_be_followed)
            })
            .cloned()
            .unwrap_or(this);
        let Some(leader_fish) = leader.as_schooling_fish() else {
            return false;
        };
        let not_followers = candidates
            .into_iter()
            .filter(|other| {
                other
                    .as_schooling_fish()
                    .is_some_and(|other| !other.is_follower())
            })
            .collect();
        leader_fish.add_followers(&leader, not_followers);
        fish.is_follower()
    }
}

impl Goal for FollowFlockLeaderGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fish) = mob.as_schooling_fish() else {
            return false;
        };
        if fish.has_followers() {
            return false;
        }
        if fish.is_follower() {
            return true;
        }
        if self.next_start_tick > 0 {
            self.next_start_tick -= 1;
            return false;
        }

        self.next_start_tick = Self::next_start_tick();
        Self::find_school(fish)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.as_schooling_fish()
            .is_some_and(|fish| fish.is_follower() && fish.in_range_of_leader())
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.time_to_recalc_path = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fish) = mob.as_schooling_fish() {
            fish.stop_following();
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.time_to_recalc_path -= 1;
        if self.time_to_recalc_path > 0 {
            return;
        }
        self.time_to_recalc_path = reduced_tick_delay(RECALCULATE_PATH_TICKS);
        if let Some(fish) = mob.as_schooling_fish() {
            fish.path_to_leader();
        }
    }
}
