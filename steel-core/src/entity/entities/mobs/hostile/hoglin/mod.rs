//! Hoglin and zoglin, the Nether boars (vanilla `HoglinBase`).

mod hoglin_mob;
mod sensor;
#[cfg(test)]
mod tests;
mod zoglin;

use std::sync::Arc;

use glam::DVec3;
use steel_math::vector::y_rot;
use steel_registry::attribute::AttributeRef;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_attributes, vanilla_damage_types};

pub use hoglin_mob::HoglinEntity;
pub use zoglin::ZoglinEntity;

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::projectile::normalize_or_zero;
use crate::entity::{LivingEntity, SharedEntity};
use crate::world::World;

/// Vanilla `HoglinBase.ATTACK_ANIMATION_DURATION`.
const ATTACK_ANIMATION_DURATION: i32 = 10;
/// Vanilla `HoglinBase.PROBABILITY_OF_SPAWNING_AS_BABY`.
const PROBABILITY_OF_SPAWNING_AS_BABY: f32 = 0.2;

fn attribute(entity: &dyn LivingEntity, attribute: AttributeRef) -> f64 {
    entity
        .attributes()
        .lock()
        .get_value(attribute)
        .unwrap_or(0.0)
}

/// Vanilla `HoglinBase.hurtAndThrowTarget`: a random hit between half and
/// one and a half times the attack damage, then a toss for adults.
fn hurt_and_throw_target(world: &World, body: &SharedEntity, target: &SharedEntity) -> bool {
    let (Some(living_body), Some(living_target)) =
        (body.as_living_entity(), target.as_living_entity())
    else {
        return false;
    };
    let attack_damage = attribute(living_body, vanilla_attributes::ATTACK_DAMAGE) as f32;
    let actual_damage = if !living_body.is_baby() && attack_damage as i32 > 0 {
        attack_damage / 2.0 + rand::random_range(0..attack_damage as i32) as f32
    } else {
        attack_damage
    };
    let source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
        .with_causing_entity(Arc::clone(body))
        .with_direct_entity(Arc::clone(body));
    let hurt = living_target.hurt_server(world, &source, actual_damage);
    if hurt {
        // Vanilla `EnchantmentHelper.doPostAttackEffects`, whose weapon is the
        // attacker's main hand.
        let mut weapon = ItemStack::empty();
        living_body.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| {
            weapon = stack.copy_with_count(stack.count());
        });
        let context = EnchantmentPostAttackContext::new(target, &source);
        enchantment_helper::do_post_attack_effects_with_item_source(
            world,
            target.as_ref(),
            &weapon,
            &context,
        );
        if !living_body.is_baby() {
            throw_target(living_body, living_target);
        }
    }
    hurt
}

/// Vanilla `HoglinBase.throwTarget`: knock the target away and up.
fn throw_target(body: &dyn LivingEntity, target: &dyn LivingEntity) {
    let knockback_power = attribute(body, vanilla_attributes::ATTACK_KNOCKBACK);
    let knockback_resistance = attribute(target, vanilla_attributes::KNOCKBACK_RESISTANCE);
    let effective_power = knockback_power - knockback_resistance;
    if effective_power <= 0.0 {
        return;
    }
    let target_position = target.position();
    let body_position = body.position();
    let xd = target_position.x - body_position.x;
    let zd = target_position.z - body_position.z;
    // Vanilla passes these degrees to `Vec3.yRot` as radians.
    let horizontal_push_angle = (rand::random_range(0..21) - 10) as f32;
    let horizontal_scale = effective_power * f64::from(rand::random::<f32>() * 0.5 + 0.2);
    let horizontal = y_rot(
        normalize_or_zero(DVec3::new(xd, 0.0, zd)) * horizontal_scale,
        horizontal_push_angle,
    );
    let vertical_scale = effective_power * f64::from(rand::random::<f32>()) * 0.5;
    target.push_impulse(DVec3::new(horizontal.x, vertical_scale, horizontal.z));
    target.mark_velocity_sync();
}
