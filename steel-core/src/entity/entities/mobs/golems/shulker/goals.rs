//! The shulker's own goals (vanilla `Shulker` inner classes).

use std::sync::Arc;

use glam::DVec3;
use steel_registry::sound_events;
use steel_utils::Downcast as _;
use steel_utils::types::Difficulty;

use super::ShulkerEntity;
use crate::entity::ai::goal::{
    Goal, GoalControls, NearestAttackableTargetGoal, reduced_tick_delay,
};
use crate::entity::entities::ShulkerBulletEntity;
use crate::entity::{Entity, LivingEntity, PathfinderMob, SharedEntity};

/// Vanilla attack range: shoots only within 20 blocks.
const ATTACK_DISTANCE_SQR: f64 = 400.0;

fn shulker(mob: &dyn PathfinderMob) -> Option<&ShulkerEntity> {
    mob.downcast_ref::<ShulkerEntity>()
}

fn is_peaceful(mob: &dyn PathfinderMob) -> bool {
    mob.level()
        .is_none_or(|world| world.difficulty() == Difficulty::Peaceful)
}

fn is_alive(entity: &SharedEntity) -> bool {
    entity
        .as_living_entity()
        .is_some_and(LivingEntity::is_alive)
}

/// Vanilla `ShulkerAttackGoal`: open up and fire a bullet every 1–5.5 s.
pub(super) struct ShulkerAttackGoal {
    attack_time: i32,
}

impl ShulkerAttackGoal {
    pub(super) const fn new() -> Self {
        Self { attack_time: 0 }
    }
}

impl Goal for ShulkerAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_some_and(|target| is_alive(&target)) && !is_peaceful(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.attack_time = 20;
        if let Some(shulker) = shulker(mob) {
            shulker.set_raw_peek_amount(100);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(shulker) = shulker(mob) {
            shulker.set_raw_peek_amount(0);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        if is_peaceful(mob) {
            return;
        }
        let Some(shulker) = shulker(mob) else {
            return;
        };
        self.attack_time -= 1;
        let Some(target) = mob.target() else {
            return;
        };
        let target_position = target.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
            180.0,
            180.0,
        );
        if mob.distance_to_sqr(target_position) >= ATTACK_DISTANCE_SQR {
            mob.set_target(None);
            return;
        }
        if self.attack_time > 0 {
            return;
        }
        self.attack_time = 20 + rand::random_range(0..10) * 20 / 2;
        let Some(world) = mob.level() else {
            return;
        };
        let bullet = Arc::new(ShulkerBulletEntity::fired_by(
            &world,
            entity,
            &target,
            shulker.attach_face().axis(),
        ));
        if let Err(error) = world.try_add_entity(bullet) {
            log::debug!("shulker bullet was not added to the world: {error:?}");
        }
        mob.play_sound(
            &sound_events::ENTITY_SHULKER_SHOOT,
            2.0,
            (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0,
        );
    }
}

/// Vanilla `ShulkerPeekGoal`: idly open the lid for 1–3 seconds.
pub(super) struct ShulkerPeekGoal {
    peek_time: i32,
}

impl ShulkerPeekGoal {
    pub(super) const fn new() -> Self {
        Self { peek_time: 0 }
    }
}

impl Goal for ShulkerPeekGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(shulker) = shulker(mob) else {
            return false;
        };
        mob.target().is_none()
            && rand::random_range(0..reduced_tick_delay(40)) == 0
            && shulker.can_stay_at(shulker.block_position(), shulker.attach_face())
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_none() && self.peek_time > 0
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.peek_time = reduced_tick_delay(20 * (1 + rand::random_range(0..3)));
        if let Some(shulker) = shulker(mob) {
            shulker.set_raw_peek_amount(30);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if mob.target().is_none()
            && let Some(shulker) = shulker(mob)
        {
            shulker.set_raw_peek_amount(0);
        }
    }

    fn tick(&mut self, _mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.peek_time -= 1;
    }
}

/// Vanilla `ShulkerNearestAttackGoal`: players, never on Peaceful.
///
/// Vanilla also narrows the search box to the attach axis, but the player
/// variant of `NearestAttackableTargetGoal` never reads that box.
pub(super) struct ShulkerNearestAttackGoal {
    inner: NearestAttackableTargetGoal,
}

impl ShulkerNearestAttackGoal {
    pub(super) fn new() -> Self {
        Self {
            inner: NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
        }
    }
}

impl Goal for ShulkerNearestAttackGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !is_peaceful(mob) && self.inner.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.inner.tick(mob, entity);
    }
}
