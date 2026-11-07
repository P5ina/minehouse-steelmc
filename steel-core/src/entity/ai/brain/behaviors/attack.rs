//! Vanilla `StartAttacking` and `ChargeAttack`.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{vanilla_attributes, vanilla_mob_effects};

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::ai::brain::BehaviorControl;
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::{Behavior, OneShot};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, CANT_REACH_WALK_TARGET_SINCE, CHARGE_COOLDOWN_TICKS,
};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{Mob, SharedEntity, TamableAnimal};
use crate::inventory::prelude::{EquipmentSlot, ItemStack};
use crate::world::World;

/// Picks an attack target with `target_finder` when the body has none
/// (vanilla `StartAttacking.create`).
pub fn start_attacking<E: Mob + ?Sized>(
    mut target_finder: impl FnMut(&Arc<World>, &E) -> Option<SharedEntity> + Send + 'static,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&ATTACK_TARGET) {
                return false;
            }
            let Some(target) = target_finder(world, body) else {
                return false;
            };
            let Some(living) = target.as_living_entity() else {
                return false;
            };
            if !Mob::can_attack(body, living) {
                return false;
            }
            let mut memories = memories.lock();
            memories.set(&ATTACK_TARGET, Some(target));
            memories.erase(&CANT_REACH_WALK_TARGET_SINCE);
            true
        },
    )
}

/// Rams the attack target in a straight line, hurting and knocking back the
/// first matching entity it touches (vanilla `ChargeAttack`).
pub struct ChargeAttack {
    time_between_attacks: i32,
    charge_targeting: TargetingConditions,
    speed: f32,
    knockback_force: f32,
    max_charge_distance: f64,
    max_target_detection_distance: f64,
    charge_sound: SoundEventRef,
    charge_velocity: DVec3,
    start_position: DVec3,
    entry_condition: [MemoryRequirement; 2],
}

impl ChargeAttack {
    #[must_use]
    pub fn new(
        time_between_attacks: i32,
        charge_targeting: TargetingConditions,
        speed: f32,
        knockback_force: f32,
        max_charge_distance: f64,
        max_target_detection_distance: f64,
        charge_sound: SoundEventRef,
    ) -> Self {
        Self {
            time_between_attacks,
            charge_targeting,
            speed,
            knockback_force,
            max_charge_distance,
            max_target_detection_distance,
            charge_sound,
            charge_velocity: DVec3::ZERO,
            start_position: DVec3::ZERO,
            entry_condition: [
                MemoryRequirement::new(&CHARGE_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
                MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
            ],
        }
    }

    fn attack_target(body: &(impl Mob + ?Sized)) -> Option<SharedEntity> {
        body.brain_memories()?.lock().get(&ATTACK_TARGET)
    }

    fn effect_level(body: &(impl Mob + ?Sized), speed_up: bool) -> i32 {
        let effect = if speed_up {
            vanilla_mob_effects::SPEED
        } else {
            vanilla_mob_effects::SLOWNESS
        };
        body.mob_effect(effect)
            .map_or(0, |effect| effect.amplifier() + 1)
    }

    fn hit(&self, world: &World, body: &(impl Mob + ?Sized), target: &SharedEntity) {
        let Some(attacker) = world.get_entity_by_id(body.id()) else {
            return;
        };
        let (damage, movement_speed) = {
            let attributes = body.attributes().lock();
            (
                attributes.required_value(vanilla_attributes::ATTACK_DAMAGE) as f32,
                attributes.required_value(vanilla_attributes::MOVEMENT_SPEED) as f32,
            )
        };
        // Vanilla `dealDamageToTarget`.
        let damage_source = body.mob_attack_damage_source(&ItemStack::empty(), &attacker);
        if target.hurt(world, &damage_source, damage) {
            let context = EnchantmentPostAttackContext::new(target, &damage_source);
            enchantment_helper::do_post_attack_effects_with_item_source(
                world,
                target.as_ref(),
                &body.equipment_in_slot(EquipmentSlot::MainHand),
                &context,
            );
        }
        // Vanilla `dealKnockBack`.
        let speed_boost_power =
            0.25 * (Self::effect_level(body, true) - Self::effect_level(body, false)) as f32;
        let speed_factor = (self.speed * movement_speed).clamp(0.2, 2.0) + speed_boost_power;
        body.cause_extra_knockback(
            target.as_ref(),
            f64::from(speed_factor * self.knockback_force),
            body.velocity(),
        );
    }
}

impl<E: Mob> Behavior<E> for ChargeAttack {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &self.entry_condition
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        Self::attack_target(body).is_some()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        let Some(target) = Self::attack_target(body) else {
            return false;
        };
        if body.as_tamable_animal().is_some_and(TamableAnimal::is_tame) {
            return false;
        }
        let position = body.position();
        if position.distance_squared(self.start_position)
            >= self.max_charge_distance * self.max_charge_distance
        {
            return false;
        }
        if target.position().distance_squared(position)
            >= self.max_target_detection_distance * self.max_target_detection_distance
        {
            return false;
        }
        body.has_line_of_sight(target.as_ref())
            && !body
                .brain_memories()
                .is_some_and(|memories| memories.lock().has_value(&CHARGE_COOLDOWN_TICKS))
    }

    fn start(&mut self, world: &Arc<World>, body: &E, time: i64) {
        self.start_position = body.position();
        let Some(target) = Self::attack_target(body) else {
            return;
        };
        let direction = (target.position() - body.position()).normalize_or_zero();
        self.charge_velocity = direction * f64::from(self.speed);
        if self.can_still_use(world, body, time) {
            body.play_sound(self.charge_sound, 1.0, 1.0);
        }
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        let Some(target) = Self::attack_target(body) else {
            return;
        };
        body.mob_look_at(target.as_ref(), 360.0, 360.0);
        body.set_velocity(self.charge_velocity);
        let body_id = body.id();
        let hit = world
            .get_entities_in_aabb_matching(&body.bounding_box(), |entity| {
                entity.id() != body_id
                    && entity
                        .as_living_entity()
                        .is_some_and(|living| self.charge_targeting.test(world, Some(body), living))
            })
            .into_iter()
            .next();
        let Some(hit) = hit else {
            return;
        };
        if body.has_passenger(hit.as_ref()) {
            return;
        }
        self.hit(world, body, &hit);
        self.stop(world, body, time);
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.set(&CHARGE_COOLDOWN_TICKS, Some(self.time_between_attacks));
            memories.erase(&ATTACK_TARGET);
        }
    }
}
