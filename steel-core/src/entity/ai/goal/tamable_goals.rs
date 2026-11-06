//! Goals of vanilla `TamableAnimal`s: sitting on command, following and
//! defending the owner, and hunting while still wild.

use glam::DVec3;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _};
use steel_utils::Identifier;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use super::target_goal::TargetGoalBase;
use super::{NearestAttackableTargetGoal, PanicGoal};
use crate::entity::ai::path::PathType;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{LivingEntity, PathfinderMob, SharedEntity, TamableAnimal};
use crate::world::World;

/// Owners further than 12 blocks away no longer keep a pet from sitting.
const OWNER_NEARBY_DISTANCE_SQR: f64 = 144.0;
const FOLLOW_OWNER_RECALC_TICKS: i32 = 10;
/// Vanilla `OwnerHurtByTargetGoal` looks back this far for the owner's damage.
const OWNER_DAMAGE_TIMEOUT_TICKS: i64 = 100;
const NON_TAME_TARGET_INTERVAL: i32 = 10;

fn tamable(mob: &dyn PathfinderMob) -> Option<&dyn TamableAnimal> {
    mob.as_tamable_animal()
}

fn same_level(mob: &dyn PathfinderMob, owner: &SharedEntity) -> bool {
    match (mob.level(), owner.level()) {
        (Some(mob_level), Some(owner_level)) => mob_level.domain() == owner_level.domain(),
        _ => false,
    }
}

/// Vanilla `SitWhenOrderedToGoal`.
pub(crate) struct SitWhenOrderedToGoal;

impl SitWhenOrderedToGoal {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl Goal for SitWhenOrderedToGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(tamable) = tamable(mob) else {
            return false;
        };
        let ordered_to_sit = tamable.is_ordered_to_sit();
        if !ordered_to_sit && !tamable.is_tame() {
            return false;
        }
        if mob.is_in_water() || !mob.on_ground() {
            return false;
        }
        let Some(owner) = tamable.owner() else {
            return true;
        };
        if !same_level(mob, &owner) {
            return true;
        }
        let owner_hurt = owner
            .as_living_entity()
            .is_some_and(|owner| owner.last_hurt_by_mob().is_some());
        if mob.position().distance_squared(owner.position()) < OWNER_NEARBY_DISTANCE_SQR
            && owner_hurt
        {
            return false;
        }
        ordered_to_sit
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        tamable(mob).is_some_and(TamableAnimal::is_ordered_to_sit)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
        if let Some(tamable) = tamable(mob) {
            tamable.set_in_sitting_pose(true);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(tamable) = tamable(mob) {
            tamable.set_in_sitting_pose(false);
        }
    }
}

/// Vanilla `FollowOwnerGoal`: walks after an owner who wandered off and
/// teleports to one that is far away.
pub(crate) struct FollowOwnerGoal {
    owner: Option<SharedEntity>,
    speed_modifier: f64,
    time_to_recalc_path: i32,
    start_distance: f32,
    stop_distance: f32,
    old_water_cost: f32,
}

impl FollowOwnerGoal {
    pub(crate) const fn new(speed_modifier: f64, start_distance: f32, stop_distance: f32) -> Self {
        Self {
            owner: None,
            speed_modifier,
            time_to_recalc_path: 0,
            start_distance,
            stop_distance,
            old_water_cost: 0.0,
        }
    }
}

impl Goal for FollowOwnerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(tamable) = tamable(mob) else {
            return false;
        };
        let Some(owner) = tamable.owner() else {
            return false;
        };
        if tamable.unable_to_move_to_owner() {
            return false;
        }
        let start = f64::from(self.start_distance);
        if mob.position().distance_squared(owner.position()) < start * start {
            return false;
        }
        self.owner = Some(owner);
        true
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if mob.mob_base().navigation().lock().is_done() {
            return false;
        }
        let Some(tamable) = tamable(mob) else {
            return false;
        };
        if tamable.unable_to_move_to_owner() {
            return false;
        }
        let stop = f64::from(self.stop_distance);
        self.owner
            .as_ref()
            .is_some_and(|owner| mob.position().distance_squared(owner.position()) > stop * stop)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.time_to_recalc_path = 0;
        self.old_water_cost = mob.get_pathfinding_malus(PathType::Water);
        mob.set_pathfinding_malus(PathType::Water, 0.0);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.owner = None;
        mob.mob_base().navigation().lock().stop();
        mob.set_pathfinding_malus(PathType::Water, self.old_water_cost);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(tamable) = tamable(mob) else {
            return;
        };
        let Some(owner) = self.owner.clone() else {
            return;
        };
        let owner_far_away = tamable.should_try_teleport_to_owner();
        if !owner_far_away {
            let owner_position = owner.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(owner_position.x, owner.get_eye_y(), owner_position.z),
                10.0,
                mob.max_head_x_rot(),
            );
        }

        self.time_to_recalc_path -= 1;
        if self.time_to_recalc_path <= 0 {
            self.time_to_recalc_path = reduced_tick_delay(FOLLOW_OWNER_RECALC_TICKS);
            if owner_far_away {
                tamable.try_to_teleport_to_owner();
            } else {
                let _ = mob.move_to_entity(owner.as_ref(), self.speed_modifier);
            }
        }
    }
}

/// What an owner-defense goal reads from the owner.
#[derive(Clone, Copy)]
enum OwnerFight {
    /// Vanilla `OwnerHurtByTargetGoal`: whoever last hurt the owner.
    HurtBy,
    /// Vanilla `OwnerHurtTargetGoal`: whatever the owner last hurt.
    Hurt,
}

/// Shared body of vanilla `OwnerHurtByTargetGoal` and `OwnerHurtTargetGoal`.
pub(crate) struct OwnerTargetGoal {
    target_goal: TargetGoalBase,
    fight: OwnerFight,
    target: Option<SharedEntity>,
    timestamp: i32,
}

impl OwnerTargetGoal {
    /// Vanilla `OwnerHurtByTargetGoal`: attacks whoever hurt the owner.
    pub(crate) const fn owner_hurt_by() -> Self {
        Self::new(OwnerFight::HurtBy)
    }

    /// Vanilla `OwnerHurtTargetGoal`: attacks whatever the owner attacks.
    pub(crate) const fn owner_hurt() -> Self {
        Self::new(OwnerFight::Hurt)
    }

    const fn new(fight: OwnerFight) -> Self {
        Self {
            target_goal: TargetGoalBase::new(false, false),
            fight,
            target: None,
            timestamp: 0,
        }
    }

    fn owner_fight(&self, owner: &dyn LivingEntity) -> (Option<SharedEntity>, i32) {
        match self.fight {
            OwnerFight::HurtBy => (owner.last_hurt_by_mob(), owner.last_hurt_by_mob_timestamp()),
            OwnerFight::Hurt => (owner.last_hurt_mob(), owner.last_hurt_mob_timestamp()),
        }
    }
}

fn is_no_wolf_retaliation(tag: &Identifier, owner: &dyn LivingEntity) -> bool {
    owner
        .last_damage_type_within(OWNER_DAMAGE_TIMEOUT_TICKS)
        .is_none_or(|damage_type| REGISTRY.damage_types.is_in_tag(damage_type, tag))
}

impl Goal for OwnerTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(tamable) = tamable(mob) else {
            return false;
        };
        if !tamable.is_tame() || tamable.is_ordered_to_sit() {
            return false;
        }
        let Some(owner_entity) = tamable.owner() else {
            return false;
        };
        let Some(owner) = owner_entity.as_living_entity() else {
            return false;
        };
        if matches!(self.fight, OwnerFight::HurtBy)
            && is_no_wolf_retaliation(&DamageTypeTag::NO_WOLF_RETALIATION, owner)
        {
            return false;
        }
        let (target, timestamp) = self.owner_fight(owner);
        self.target = target;
        let Some(target) = self.target.as_ref() else {
            return false;
        };
        let Some(target_living) = target.as_living_entity() else {
            return false;
        };
        timestamp != self.timestamp
            && self.target_goal.can_attack(
                mob,
                Some(target_living),
                &TargetingConditions::for_combat(),
            )
            && tamable.wants_to_attack(target_living, owner)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target_goal.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.set_target(self.target.as_ref());
        if let Some(owner) = tamable(mob).and_then(TamableAnimal::owner)
            && let Some(owner) = owner.as_living_entity()
        {
            self.timestamp = self.owner_fight(owner).1;
        }
        self.target_goal.start();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target_goal.stop(mob);
    }
}

/// Vanilla `NonTameRandomTargetGoal`: hunts prey only while still wild.
pub(crate) struct NonTameRandomTargetGoal {
    nearest: NearestAttackableTargetGoal,
}

impl NonTameRandomTargetGoal {
    pub(crate) fn new(
        must_see: bool,
        selector: impl Fn(&dyn LivingEntity, &World) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            nearest: NearestAttackableTargetGoal::new_with_interval(
                NON_TAME_TARGET_INTERVAL,
                must_see,
                false,
                selector,
            ),
        }
    }
}

impl Goal for NonTameRandomTargetGoal {
    fn controls(&self) -> GoalControls {
        self.nearest.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !tamable(mob).is_some_and(TamableAnimal::is_tame) && self.nearest.can_use(mob)
    }

    /// Vanilla tests only the target conditions here, not the usual target checks.
    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.nearest.target_still_matches(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.nearest.stop(mob);
    }
}

/// Vanilla `TamableAnimal.TamableAnimalPanicGoal`: a panicking pet still
/// teleports to an owner who left it behind.
pub(crate) struct TamableAnimalPanicGoal {
    panic: PanicGoal,
}

impl TamableAnimalPanicGoal {
    pub(crate) const fn new(speed_modifier: f64) -> Self {
        Self {
            panic: PanicGoal::new(speed_modifier),
        }
    }

    /// Panics on `panic_causes` instead of vanilla `PANIC_CAUSES`.
    pub(crate) const fn with_panic_causes(
        mut self,
        panic_causes: fn(&dyn PathfinderMob) -> Identifier,
    ) -> Self {
        self.panic = self.panic.with_panic_causes(panic_causes);
        self
    }
}

impl Goal for TamableAnimalPanicGoal {
    fn controls(&self) -> GoalControls {
        self.panic.controls()
    }

    fn is_panic_goal(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.panic.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.panic.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.panic.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.panic.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        if let Some(tamable) = tamable(mob)
            && !tamable.unable_to_move_to_owner()
            && tamable.should_try_teleport_to_owner()
        {
            tamable.try_to_teleport_to_owner();
        }
        self.panic.tick(mob, entity);
    }
}
