//! The enderman's own goals (vanilla `Enderman` inner classes).

use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{vanilla_attributes, vanilla_blocks, vanilla_game_events};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, WorldAabb};

use super::EndermanEntity;
use crate::behavior::BLOCK_BEHAVIORS;
use crate::entity::ai::control::DEFAULT_LOOK_Y_MAX_ROT_SPEED;
use crate::entity::ai::goal::{
    Goal, GoalControls, NearestAttackableTargetGoal, reduced_tick_delay,
};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{Entity, LivingEntity, NeutralMob as _, PathfinderMob, SharedEntity};
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{ClipBlockShape, ClipFluid};

/// Squared distance within which a stare freezes the enderman (16 blocks).
const FREEZE_DISTANCE_SQR: f64 = 256.0;
/// Squared distance under which a stared-at enderman teleports away.
const STARED_TELEPORT_AWAY_DISTANCE_SQR: f64 = 16.0;
/// Squared distance beyond which the enderman teleports toward its target.
const TELEPORT_TOWARDS_DISTANCE_SQR: f64 = 256.0;
const LEAVE_BLOCK_CHANCE: i32 = 2000;
const TAKE_BLOCK_CHANCE: i32 = 20;

fn enderman(mob: &dyn PathfinderMob) -> Option<&EndermanEntity> {
    mob.downcast_ref::<EndermanEntity>()
}

/// Vanilla `Goal.adjustedTickDelay` for goals that do not tick every tick.
const fn adjusted_tick_delay(ticks: i32) -> i32 {
    reduced_tick_delay(ticks)
}

fn mob_griefing(mob: &dyn PathfinderMob) -> bool {
    mob.level()
        .is_some_and(|world| world.get_game_rule(&MOB_GRIEFING))
}

/// Vanilla `EndermanFreezeWhenLookedAt`: stand still while the target stares.
pub(super) struct EndermanFreezeWhenLookedAt {
    target: Option<SharedEntity>,
}

impl EndermanFreezeWhenLookedAt {
    pub(super) const fn new() -> Self {
        Self { target: None }
    }
}

impl Goal for EndermanFreezeWhenLookedAt {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target = mob.target();
        let Some(enderman) = enderman(mob) else {
            return false;
        };
        let Some(player) = self.target.as_ref().and_then(|target| target.as_player()) else {
            return false;
        };
        player.position().distance_squared(mob.position()) <= FREEZE_DISTANCE_SQR
            && enderman.is_being_stared_by(player)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(target) = &self.target else {
            return;
        };
        let position = target.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(position.x, target.get_eye_y(), position.z),
            DEFAULT_LOOK_Y_MAX_ROT_SPEED,
            mob.max_head_x_rot(),
        );
    }
}

/// Vanilla `EndermanLeaveBlockGoal`: now and then, set the carried block down.
pub(super) struct EndermanLeaveBlockGoal;

impl EndermanLeaveBlockGoal {
    fn can_place_block(enderman: &EndermanEntity, pos: BlockPos, carried: BlockStateId) -> bool {
        let Some(world) = enderman.level() else {
            return false;
        };
        let below = pos.below();
        let below_state = world.get_block_state(below);
        let unit_cube = WorldAabb::new(
            f64::from(pos.x()),
            f64::from(pos.y()),
            f64::from(pos.z()),
            f64::from(pos.x()) + 1.0,
            f64::from(pos.y()) + 1.0,
            f64::from(pos.z()) + 1.0,
        );
        world.get_block_state(pos).is_air()
            && !below_state.is_air()
            && below_state.get_block() != &vanilla_blocks::BEDROCK
            && world.is_collision_shape_full_block_at(below, below_state)
            && BLOCK_BEHAVIORS
                .get_behavior(carried.get_block())
                .can_survive(carried, world.as_ref(), pos)
            && !world.has_entity_in_aabb_matching(&unit_cube, |entity| {
                entity.id() != enderman.id() && !entity.is_spectator()
            })
    }
}

impl Goal for EndermanLeaveBlockGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        enderman(mob).is_some_and(|enderman| enderman.carried_block().is_some())
            && mob_griefing(mob)
            && rand::random_range(0..reduced_tick_delay(LEAVE_BLOCK_CHANCE)) == 0
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(enderman) = enderman(mob) else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        let position = mob.position();
        let pos = BlockPos::new(
            (position.x - 1.0 + rand::random::<f64>() * 2.0).floor() as i32,
            (position.y + rand::random::<f64>() * 2.0).floor() as i32,
            (position.z - 1.0 + rand::random::<f64>() * 2.0).floor() as i32,
        );
        let Some(carried) = enderman.carried_block() else {
            return;
        };
        let carried = world.update_from_neighbor_shapes(carried, pos);
        if Self::can_place_block(enderman, pos, carried) {
            world.set_block(pos, carried, UpdateFlags::UPDATE_ALL);
            world.game_event(
                &vanilla_game_events::BLOCK_PLACE,
                pos,
                &GameEventContext::new(Some(enderman), Some(carried)),
            );
            enderman.set_carried_block(None);
        }
    }
}

/// Vanilla `EndermanTakeBlockGoal`: pick up a nearby holdable block it can reach.
pub(super) struct EndermanTakeBlockGoal;

impl Goal for EndermanTakeBlockGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        enderman(mob).is_some_and(|enderman| enderman.carried_block().is_none())
            && mob_griefing(mob)
            && rand::random_range(0..reduced_tick_delay(TAKE_BLOCK_CHANCE)) == 0
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(enderman) = enderman(mob) else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        let position = mob.position();
        let xt = (position.x - 2.0 + rand::random::<f64>() * 4.0).floor() as i32;
        let yt = (position.y + rand::random::<f64>() * 3.0).floor() as i32;
        let zt = (position.z - 2.0 + rand::random::<f64>() * 4.0).floor() as i32;
        let pos = BlockPos::new(xt, yt, zt);
        let block_state = world.get_block_state(pos);
        let block_position = mob.block_position();
        let from = DVec3::new(
            f64::from(block_position.x()) + 0.5,
            f64::from(yt) + 0.5,
            f64::from(block_position.z()) + 0.5,
        );
        let to = DVec3::new(
            f64::from(xt) + 0.5,
            f64::from(yt) + 0.5,
            f64::from(zt) + 0.5,
        );
        let reachable = world
            .clip(from, to, ClipBlockShape::Outline, ClipFluid::None)
            .block_pos
            == pos;
        if block_state
            .get_block()
            .has_tag(&BlockTag::ENDERMAN_HOLDABLE)
            && reachable
        {
            world.remove_block(pos, false);
            world.game_event(
                &vanilla_game_events::BLOCK_DESTROY,
                pos,
                &GameEventContext::new(Some(enderman), Some(block_state)),
            );
            enderman.set_carried_block(Some(block_state.get_block().default_state()));
        }
    }
}

/// Vanilla `EndermanLookForPlayerGoal`: a player who stares (or angered it)
/// becomes the target after a short delay; a stared-at enderman teleports
/// away up close and toward a far target.
pub(super) struct EndermanLookForPlayerGoal {
    inner: NearestAttackableTargetGoal,
    pending_target: Option<Arc<Player>>,
    aggro_time: i32,
    teleport_time: i32,
    start_aggro_target_conditions: TargetingConditions,
    continue_aggro_target_conditions: TargetingConditions,
}

/// Vanilla `isAngerInducing`.
fn is_anger_inducing(enderman: &EndermanEntity, target: &dyn LivingEntity) -> bool {
    let Some(world) = enderman.level() else {
        return false;
    };
    (target
        .as_player()
        .is_some_and(|player| enderman.is_being_stared_by(player))
        || enderman.is_angry_at(target, &world))
        && !enderman.has_indirect_passenger(target.as_entity_event_source())
}

impl EndermanLookForPlayerGoal {
    pub(super) fn new() -> Self {
        Self {
            inner: NearestAttackableTargetGoal::new_for_players_with_interval(
                10,
                false,
                false,
                |_, _| true,
            ),
            pending_target: None,
            aggro_time: 0,
            teleport_time: 0,
            start_aggro_target_conditions: TargetingConditions::for_combat().targeter_selector(
                |target, targeter, _| {
                    targeter
                        .downcast_ref::<EndermanEntity>()
                        .is_some_and(|enderman| is_anger_inducing(enderman, target))
                },
            ),
            continue_aggro_target_conditions: TargetingConditions::for_combat()
                .ignore_line_of_sight(),
        }
    }
}

fn follow_distance(mob: &dyn PathfinderMob) -> f64 {
    mob.attributes()
        .lock()
        .required_value(vanilla_attributes::FOLLOW_RANGE)
}

impl Goal for EndermanLookForPlayerGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        let conditions = self
            .start_aggro_target_conditions
            .clone()
            .range(follow_distance(mob));
        self.pending_target = world.nearest_player(mob.position(), -1.0, |player| {
            conditions.test(world.as_ref(), Some(mob), player)
        });
        self.pending_target.is_some()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.aggro_time = adjusted_tick_delay(5);
        self.teleport_time = 0;
        if let Some(enderman) = enderman(mob) {
            enderman.set_being_stared_at();
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.pending_target = None;
        self.inner.stop(mob);
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(enderman) = enderman(mob) else {
            return false;
        };
        if let Some(pending) = &self.pending_target {
            if !is_anger_inducing(enderman, pending.as_ref()) {
                return false;
            }
            mob.mob_look_at(pending.as_ref(), 10.0, 10.0);
            return true;
        }
        if let Some(target) = self.inner.found_target() {
            if enderman.has_indirect_passenger(target.as_ref()) {
                return false;
            }
            if let (Some(world), Some(living)) = (mob.level(), target.as_living_entity())
                && self
                    .continue_aggro_target_conditions
                    .test(world.as_ref(), Some(mob), living)
            {
                return true;
            }
        }
        self.inner.can_continue_to_use(mob)
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        if mob.target().is_none() {
            self.inner.set_target(None);
        }
        if let Some(pending) = self.pending_target.clone() {
            self.aggro_time -= 1;
            if self.aggro_time <= 0 {
                self.inner.set_target(Some(pending));
                self.pending_target = None;
                self.inner.start(mob);
            }
            return;
        }
        if let Some(enderman) = enderman(mob)
            && let Some(target) = self.inner.found_target().cloned()
            && !enderman.is_passenger()
        {
            let distance_sqr = target.position().distance_squared(mob.position());
            let stared = target
                .as_player()
                .is_some_and(|player| enderman.is_being_stared_by(player));
            if stared {
                if distance_sqr < STARED_TELEPORT_AWAY_DISTANCE_SQR {
                    enderman.teleport_randomly();
                }
                self.teleport_time = 0;
            } else if distance_sqr > TELEPORT_TOWARDS_DISTANCE_SQR
                && {
                    let waited = self.teleport_time;
                    self.teleport_time += 1;
                    waited >= adjusted_tick_delay(30)
                }
                && enderman.teleport_towards(target.as_ref())
            {
                self.teleport_time = 0;
            }
        }
        self.inner.tick(mob, entity);
    }
}
