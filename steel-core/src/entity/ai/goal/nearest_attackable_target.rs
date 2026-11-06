use glam::DVec3;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use super::target_goal::{TargetGoalBase, follow_distance};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

const DEFAULT_RANDOM_INTERVAL: i32 = 10;

/// A target filter that also sees the searching mob, for vanilla selectors that are
/// method references on the mob (such as `this::isAngryAt`).
pub(crate) type MobTargetSelector = fn(&dyn PathfinderMob, &dyn LivingEntity, &World) -> bool;

enum TargetSearch {
    Players,
    Entities,
}

pub(crate) struct NearestAttackableTargetGoal {
    target_goal: TargetGoalBase,
    random_interval: i32,
    target: Option<SharedEntity>,
    target_conditions: TargetingConditions,
    mob_selector: Option<MobTargetSelector>,
    search: TargetSearch,
}

impl NearestAttackableTargetGoal {
    #[must_use]
    pub(crate) fn new(
        must_see: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_with_interval(DEFAULT_RANDOM_INTERVAL, must_see, false, selector)
    }

    #[must_use]
    pub(crate) fn new_with_interval(
        random_interval: i32,
        must_see: bool,
        must_reach: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_with_options(
            random_interval,
            must_see,
            must_reach,
            selector,
            TargetSearch::Entities,
        )
    }

    #[must_use]
    pub(crate) fn new_for_players(
        must_see: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_for_players_with_interval(DEFAULT_RANDOM_INTERVAL, must_see, false, selector)
    }

    #[must_use]
    pub(crate) fn new_for_players_with_interval(
        random_interval: i32,
        must_see: bool,
        must_reach: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_with_options(
            random_interval,
            must_see,
            must_reach,
            selector,
            TargetSearch::Players,
        )
    }

    /// Like [`Self::new_for_players_with_interval`], with a selector that also
    /// receives the targeting mob.
    #[must_use]
    pub(crate) fn new_for_players_from_mob(
        random_interval: i32,
        must_see: bool,
        must_reach: bool,
        selector: impl Fn(&dyn LivingEntity, &dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        let mut goal = Self::new_with_options(
            random_interval,
            must_see,
            must_reach,
            |_, _| true,
            TargetSearch::Players,
        );
        goal.target_conditions = goal.target_conditions.targeter_selector(selector);
        goal
    }

    fn new_with_options(
        random_interval: i32,
        must_see: bool,
        must_reach: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
        search: TargetSearch,
    ) -> Self {
        Self {
            target_goal: TargetGoalBase::new(must_see, must_reach),
            random_interval: reduced_tick_delay(random_interval),
            target: None,
            target_conditions: TargetingConditions::for_combat().selector(selector),
            mob_selector: None,
            search,
        }
    }

    /// Vanilla `TargetGoal.setUnseenMemoryTicks`.
    #[must_use]
    pub(crate) const fn with_unseen_memory_ticks(mut self, unseen_memory_ticks: i32) -> Self {
        self.target_goal
            .set_unseen_memory_ticks(unseen_memory_ticks);
        self
    }

    /// Adds a filter that can read the searching mob's own state.
    #[must_use]
    pub(crate) const fn with_mob_selector(mut self, mob_selector: MobTargetSelector) -> Self {
        self.mob_selector = Some(mob_selector);
        self
    }

    fn passes_mob_selector(
        &self,
        mob: &dyn PathfinderMob,
        target: &dyn LivingEntity,
        world: &World,
    ) -> bool {
        self.mob_selector
            .is_none_or(|selector| selector(mob, target, world))
    }

    pub(super) fn find_target(&mut self, mob: &dyn PathfinderMob) {
        let Some(world) = mob.level() else {
            self.target = None;
            return;
        };

        let follow = follow_distance(mob);
        let position = mob.position();
        let origin = DVec3::new(position.x, mob.get_eye_y(), position.z);
        let target_conditions = self.target_conditions.clone().range(follow);
        let level = world.as_ref();

        self.target = match self.search {
            TargetSearch::Players => level
                .nearest_player(origin, follow, |player| {
                    target_conditions.test(level, Some(mob), player)
                        && self.passes_mob_selector(mob, player, level)
                })
                .map(|player| -> SharedEntity { player }),
            TargetSearch::Entities => {
                let search_box = mob.bounding_box().inflate_xyz(follow, follow, follow);
                level.nearest_entity_in_aabb_matching(&search_box, origin, |entity| {
                    entity.as_living_entity().is_some_and(|target| {
                        target_conditions.test(level, Some(mob), target)
                            && self.passes_mob_selector(mob, target, level)
                    })
                })
            }
        };
    }

    pub(crate) fn set_target(&mut self, target: Option<SharedEntity>) {
        self.target = target;
    }

    /// Whether the last search found a target.
    pub(super) const fn has_target(&self) -> bool {
        self.target.is_some()
    }
}

impl Goal for NearestAttackableTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self.random_interval > 0 && rand::random_range(0..self.random_interval) != 0 {
            return false;
        }

        self.find_target(mob);
        self.target.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target_goal.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.set_target(self.target.as_ref());
        self.target_goal.start();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target_goal.stop(mob);
        self.target = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use steel_utils::ChunkPos;

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::{CowEntity, PigEntity};
    use crate::entity::{Entity, Mob};
    use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};

    fn animal_fixture(
        name: &'static str,
    ) -> (TestWorld, Arc<PigEntity>, Arc<PigEntity>, Arc<CowEntity>) {
        init_vanilla_registry();
        init_behaviors();
        let world_fixture = fresh_test_world(name);
        let world = &world_fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));

        let pig_hunter = Arc::new(PigEntity::new(
            &vanilla_entities::PIG,
            1,
            DVec3::new(8.0, 65.0, 8.0),
            Arc::downgrade(world),
        ));
        let nearer_pig = Arc::new(PigEntity::new(
            &vanilla_entities::PIG,
            2,
            DVec3::new(9.0, 65.0, 8.0),
            Arc::downgrade(world),
        ));
        let farther_cow = Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            3,
            DVec3::new(10.0, 65.0, 8.0),
            Arc::downgrade(world),
        ));

        let entities: [SharedEntity; 3] = [
            Arc::<PigEntity>::clone(&pig_hunter),
            Arc::<PigEntity>::clone(&nearer_pig),
            Arc::<CowEntity>::clone(&farther_cow),
        ];
        for entity in entities {
            world
                .try_add_entity(entity)
                .expect("test entity should attach to the loaded chunk");
        }

        (world_fixture, pig_hunter, nearer_pig, farther_cow)
    }

    #[test]
    fn selects_nearest_living_entity_matching_selector() {
        let (_world, pig_hunter, nearer_pig, _farther_cow) =
            animal_fixture("nearest_attackable_selector");
        let mut goal =
            NearestAttackableTargetGoal::new_with_interval(0, false, false, |target, _| {
                target.as_animal().is_some()
            });

        assert!(goal.can_use(pig_hunter.as_ref()));
        goal.start(pig_hunter.as_ref());

        let Some(target) = pig_hunter.target() else {
            panic!("selector goal should assign a target");
        };
        assert_eq!(target.uuid(), nearer_pig.uuid());
    }

    #[test]
    fn selector_can_reject_all_candidates() {
        let (_world, pig_hunter, _nearer_pig, _farther_cow) =
            animal_fixture("nearest_attackable_selector_rejects");
        let mut goal = NearestAttackableTargetGoal::new(false, |_, _| false);

        assert!(!goal.can_use(pig_hunter.as_ref()));
    }
}
