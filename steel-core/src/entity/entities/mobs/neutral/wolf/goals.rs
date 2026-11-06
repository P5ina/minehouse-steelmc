//! The wolf's own goals: vanilla `Wolf.WolfAvoidEntityGoal` and `BegGoal`.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::{vanilla_entities, vanilla_items};
use steel_utils::Downcast as _;
use steel_utils::types::InteractionHand;

use super::WolfEntity;
use crate::entity::ai::goal::{AvoidEntityGoal, Goal, GoalControls, reduced_tick_delay};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{Animal, Entity, PathfinderMob, SharedEntity, TamableAnimal};
use crate::player::Player;

const LLAMA_AVOID_DISTANCE: f32 = 24.0;
const LLAMA_AVOID_SPEED: f64 = 1.5;
/// Vanilla `avoidLlama`: a llama scares a wild wolf when its strength beats `nextInt(5)`.
const LLAMA_STRENGTH_ROLL: i32 = 5;
const BEG_MIN_TICKS: i32 = 40;

fn is_llama(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::LLAMA || entity_type == &vanilla_entities::TRADER_LLAMA
}

/// Vanilla `Wolf.WolfAvoidEntityGoal` for llamas: a wild wolf flees strong llamas.
pub(super) struct WolfAvoidLlamaGoal {
    avoid: AvoidEntityGoal,
}

impl WolfAvoidLlamaGoal {
    pub(super) fn new() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(
                LLAMA_AVOID_DISTANCE,
                LLAMA_AVOID_SPEED,
                LLAMA_AVOID_SPEED,
                |target, _| is_llama(target.as_entity_event_source()),
            ),
        }
    }
}

impl Goal for WolfAvoidLlamaGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if !self.avoid.can_use(mob) {
            return false;
        }
        let Some(strength) = self
            .avoid
            .to_avoid()
            .and_then(|llama| llama.llama_strength())
        else {
            return false;
        };
        !mob.as_tamable_animal().is_some_and(TamableAnimal::is_tame)
            && strength >= rand::random_range(0..LLAMA_STRENGTH_ROLL)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.avoid.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.set_target(None);
        self.avoid.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.avoid.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let _ = mob.set_target(None);
        self.avoid.tick(mob, entity);
    }
}

/// Vanilla `BegGoal`: a wolf tilts its head at a nearby player holding a bone or its food.
pub(super) struct BegGoal {
    player: Option<Arc<Player>>,
    look_distance: f32,
    look_time: i32,
    beg_targeting: TargetingConditions,
}

impl BegGoal {
    pub(super) fn new(look_distance: f32) -> Self {
        Self {
            player: None,
            look_distance,
            look_time: 0,
            beg_targeting: TargetingConditions::for_non_combat().range(f64::from(look_distance)),
        }
    }

    fn player_holding_interesting(mob: &dyn PathfinderMob, player: &Player) -> bool {
        let Some(wolf) = mob.downcast_ref::<WolfEntity>() else {
            return false;
        };
        let inventory = player.inventory.lock();
        [InteractionHand::MainHand, InteractionHand::OffHand]
            .into_iter()
            .any(|hand| {
                let item_stack = inventory.get_item_in_hand(hand);
                item_stack.is(&vanilla_items::BONE) || Animal::is_food(wolf, item_stack)
            })
    }

    fn set_interested(mob: &dyn PathfinderMob, interested: bool) {
        if let Some(wolf) = mob.downcast_ref::<WolfEntity>() {
            wolf.set_is_interested(interested);
        }
    }
}

impl Goal for BegGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        let level = world.as_ref();
        self.player =
            level.nearest_player(mob.position(), f64::from(self.look_distance), |player| {
                self.beg_targeting.test(level, Some(mob), player)
            });
        self.player
            .as_ref()
            .is_some_and(|player| Self::player_holding_interesting(mob, player))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(player) = self.player.as_ref() else {
            return false;
        };
        if !player.is_alive() {
            return false;
        }
        let look_distance = f64::from(self.look_distance);
        if mob.position().distance_squared(player.position()) > look_distance * look_distance {
            return false;
        }
        self.look_time > 0 && Self::player_holding_interesting(mob, player)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        Self::set_interested(mob, true);
        self.look_time = reduced_tick_delay(BEG_MIN_TICKS + rand::random_range(0..BEG_MIN_TICKS));
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        Self::set_interested(mob, false);
        self.player = None;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(player) = self.player.as_ref() {
            let position = player.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(position.x, player.get_eye_y(), position.z),
                10.0,
                mob.max_head_x_rot(),
            );
        }
        self.look_time -= 1;
    }
}
