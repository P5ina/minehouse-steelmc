//! Goals of vanilla llamas: `LlamaFollowCaravanGoal` and trader llamas'
//! `TraderLlamaDefendWanderingTraderGoal`.

use std::sync::Arc;

use steel_registry::vanilla_entities;
use steel_utils::Downcast as _;

use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use super::target_goal::TargetGoalBase;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::LeashFenceKnotEntity;
use crate::entity::entities::mobs::passive::equine::Llama;
use crate::entity::leash::Leashable;
use crate::entity::{Entity, PathfinderMob, SharedEntity};

/// Vanilla `LlamaFollowCaravanGoal.CARAVAN_LIMIT`.
const CARAVAN_LIMIT: i32 = 8;
/// Vanilla `inflate(9.0, 4.0, 9.0)` caravan search box.
const CARAVAN_SEARCH_XZ: f64 = 9.0;
const CARAVAN_SEARCH_Y: f64 = 4.0;
/// Llamas closer than two blocks never join a caravan.
const MIN_JOIN_DIST_SQR: f64 = 4.0;
/// Past 26 blocks the follower speeds up, then gives up.
const MAX_HEAD_DIST_SQR: f64 = 676.0;
const MAX_SPEED_MODIFIER: f64 = 3.0;
const SPEED_MODIFIER_BOOST: f64 = 1.2;
const DIST_CHECK_TICKS: i32 = 40;
/// Vanilla `wantedDistance` behind the caravan head.
const WANTED_DISTANCE: f64 = 2.0;
/// The speed `stop` resets to, vanilla's hard-coded 2.1.
const RESET_SPEED_MODIFIER: f64 = 2.1;

/// Vanilla `e.is(LLAMA) || e.is(TRADER_LLAMA)`.
fn is_llama_type(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::LLAMA || entity_type == &vanilla_entities::TRADER_LLAMA
}

/// Vanilla `isLeashed` on any entity.
pub(crate) fn is_leashed(entity: &dyn Entity) -> bool {
    entity.as_leashable().is_some_and(Leashable::is_leashed)
}

/// Vanilla `firstIsLeashed`: whether the caravan in front of `llama` ends in a
/// leashed llama within the caravan limit.
fn first_is_leashed(llama: &dyn Llama, counter: i32) -> bool {
    if counter > CARAVAN_LIMIT || !llama.in_caravan() {
        return false;
    }
    let Some(head) = llama.caravan_head() else {
        return false;
    };
    if is_leashed(head.as_ref()) {
        return true;
    }
    head.as_llama()
        .is_some_and(|head| first_is_leashed(head, counter + 1))
}

/// Vanilla `LlamaFollowCaravanGoal`: an unleashed llama follows the leashed
/// llama in front of it, forming a caravan.
pub(crate) struct LlamaFollowCaravanGoal {
    speed_modifier: f64,
    dist_check_counter: i32,
}

impl LlamaFollowCaravanGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            dist_check_counter: 0,
        }
    }
}

/// The closest llama in `candidates` that `accept` takes, the later one on a
/// tie as in vanilla's `!(distSquare > closestDistSquare)`.
fn closest_llama(
    mob: &dyn PathfinderMob,
    candidates: &[SharedEntity],
    accept: impl Fn(&SharedEntity, &dyn Llama) -> bool,
) -> Option<(SharedEntity, f64)> {
    let mut closest: Option<(SharedEntity, f64)> = None;
    for candidate in candidates {
        let Some(llama) = candidate.as_llama() else {
            continue;
        };
        if !accept(candidate, llama) {
            continue;
        }
        let dist_sqr = mob.distance_to_sqr(candidate.position());
        if closest
            .as_ref()
            .is_none_or(|(_, closest_dist)| dist_sqr <= *closest_dist)
        {
            closest = Some((Arc::clone(candidate), dist_sqr));
        }
    }
    closest
}

impl Goal for LlamaFollowCaravanGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(llama) = mob.as_llama() else {
            return false;
        };
        if is_leashed(mob) || llama.in_caravan() {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        let search =
            mob.bounding_box()
                .inflate_xyz(CARAVAN_SEARCH_XZ, CARAVAN_SEARCH_Y, CARAVAN_SEARCH_XZ);
        let my_id = mob.id();
        let llamas = world.get_entities_in_aabb_matching(&search, |entity| {
            entity.id() != my_id && is_llama_type(entity)
        });

        let closest = closest_llama(mob, &llamas, |_, candidate| {
            candidate.in_caravan() && !candidate.has_caravan_tail()
        })
        .or_else(|| {
            closest_llama(mob, &llamas, |entity, candidate| {
                is_leashed(entity.as_ref()) && !candidate.has_caravan_tail()
            })
        });
        let Some((head, dist_sqr)) = closest else {
            return false;
        };
        if dist_sqr < MIN_JOIN_DIST_SQR {
            return false;
        }
        let Some(head_llama) = head.as_llama() else {
            return false;
        };
        if !is_leashed(head.as_ref()) && !first_is_leashed(head_llama, 1) {
            return false;
        }
        let Some(me) = world.get_entity_by_id(my_id) else {
            return false;
        };
        llama.join_caravan(&me, &head);
        true
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(llama) = mob.as_llama() else {
            return false;
        };
        let Some(head) = llama.caravan_head().filter(|head| head.is_alive()) else {
            return false;
        };
        if !first_is_leashed(llama, 0) {
            return false;
        }
        if mob.distance_to_sqr(head.position()) > MAX_HEAD_DIST_SQR {
            if self.speed_modifier <= MAX_SPEED_MODIFIER {
                self.speed_modifier *= SPEED_MODIFIER_BOOST;
                self.dist_check_counter = reduced_tick_delay(DIST_CHECK_TICKS);
                return true;
            }
            if self.dist_check_counter == 0 {
                return false;
            }
        }
        if self.dist_check_counter > 0 {
            self.dist_check_counter -= 1;
        }
        true
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(llama) = mob.as_llama() {
            llama.leave_caravan();
        }
        self.speed_modifier = RESET_SPEED_MODIFIER;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(llama) = mob.as_llama() else {
            return;
        };
        if !llama.in_caravan() {
            return;
        }
        let tied_to_knot = mob
            .as_leashable()
            .and_then(Leashable::leash_holder)
            .is_some_and(|holder| holder.downcast_ref::<LeashFenceKnotEntity>().is_some());
        if tied_to_knot {
            return;
        }
        let Some(follows) = llama.caravan_head() else {
            return;
        };
        let position = mob.position();
        let offset = follows.position() - position;
        let distance_to = offset.length();
        let delta = offset.normalize_or_zero() * (distance_to - WANTED_DISTANCE).max(0.0);
        let _ = mob.move_to_pos(position + delta, self.speed_modifier);
    }
}

/// Vanilla `TraderLlama.TraderLlamaDefendWanderingTraderGoal`: a trader llama
/// leashed to a wandering trader targets whoever hurt the trader.
pub(crate) struct TraderLlamaDefendWanderingTraderGoal {
    target_goal: TargetGoalBase,
    owner_last_hurt_by: Option<SharedEntity>,
    timestamp: i32,
}

impl TraderLlamaDefendWanderingTraderGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            target_goal: TargetGoalBase::new(false, false),
            owner_last_hurt_by: None,
            timestamp: 0,
        }
    }
}

/// The wandering trader `mob` is leashed to, vanilla
/// `getLeashHolder() instanceof WanderingTrader`.
pub(crate) fn wandering_trader_holder(mob: &dyn Entity) -> Option<SharedEntity> {
    mob.as_leashable()
        .and_then(Leashable::leash_holder)
        .filter(|holder| holder.entity_type() == &vanilla_entities::WANDERING_TRADER)
}

impl Goal for TraderLlamaDefendWanderingTraderGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::TARGET
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(owner) = wandering_trader_holder(mob) else {
            return false;
        };
        let Some(owner) = owner.as_living_entity() else {
            return false;
        };
        self.owner_last_hurt_by = owner.last_hurt_by_mob();
        let timestamp = owner.last_hurt_by_mob_timestamp();
        let target = self
            .owner_last_hurt_by
            .as_ref()
            .and_then(|target| target.as_living_entity());
        timestamp != self.timestamp
            && self
                .target_goal
                .can_attack(mob, target, &TargetingConditions::for_combat())
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target_goal.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.set_target(self.owner_last_hurt_by.as_ref());
        if let Some(owner) = wandering_trader_holder(mob)
            && let Some(owner) = owner.as_living_entity()
        {
            self.timestamp = owner.last_hurt_by_mob_timestamp();
        }
        self.target_goal.start();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target_goal.stop(mob);
    }
}
