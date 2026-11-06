//! Vanilla `minecraft:tick` enchantment effects (Soul Speed particles and
//! sounds) and the `play_sound` / `spawn_particles` entity effects they use.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::enchantment_effect::EnchantmentEntityEffect;
use steel_registry::particle_type::ParticleData;
use steel_utils::random::legacy_random::LegacyRandom;

use super::entity_requirements_state;
use super::location::{apply_equipped_entity_effect, slot_enchantments};
use crate::entity::{Entity, LivingEntity};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;

/// Vanilla `EnchantmentHelper.tickEffects`: runs every equipped
/// enchantment's `minecraft:tick` effects whose requirements pass.
pub(crate) fn tick_effects(world: &Arc<World>, entity: &dyn LivingEntity) {
    for slot in EquipmentSlot::ALL {
        for (enchantment, level) in slot_enchantments(entity, slot) {
            for conditional in enchantment.effects.tick {
                let matched = conditional.requirements.is_none_or(|requirements| {
                    entity_requirements_state(requirements, entity, level, None) == Some(true)
                });
                if matched {
                    apply_equipped_entity_effect(world, &conditional.effect, entity, slot, level);
                }
            }
        }
    }
}

/// Vanilla `PlaySoundEffect.apply`: the sound for the enchantment level,
/// broadcast at `position` in the entity's sound source unless it is silent.
pub(super) fn play_sound(
    world: &World,
    effect: &EnchantmentEntityEffect,
    level: i32,
    entity: &dyn Entity,
    position: DVec3,
) {
    let EnchantmentEntityEffect::PlaySound {
        sounds,
        volume,
        pitch,
    } = effect
    else {
        return;
    };
    if entity.is_silent() || sounds.is_empty() {
        return;
    }
    let index = (level - 1).clamp(0, sounds.len() as i32 - 1) as usize;
    let mut random = LegacyRandom::from_seed(rand::random());
    let volume = volume.sample(&mut random);
    let pitch = pitch.sample(&mut random);
    world.play_sound_at(
        sounds[index],
        entity.sound_source(),
        position,
        volume,
        pitch,
        None,
    );
}

/// Vanilla `SpawnParticlesEffect.apply`: one directed particle (`count` 0, so
/// the distribution is its velocity) around `position`.
pub(super) fn spawn_particles(
    world: &World,
    effect: &EnchantmentEntityEffect,
    entity: &dyn Entity,
    position: DVec3,
) {
    let EnchantmentEntityEffect::SpawnParticles {
        particle,
        horizontal_position,
        vertical_position,
        horizontal_velocity,
        vertical_velocity,
        speed,
    } = effect
    else {
        return;
    };
    let mut random = LegacyRandom::from_seed(rand::random());
    let movement = entity.known_movement();
    let dimensions = entity.base().dimensions();
    let (width, height) = (dimensions.width, dimensions.height);
    // Same evaluation order as vanilla's argument list.
    let x = horizontal_position.coordinate(position.x, position.x, width, &mut random);
    let y = vertical_position.coordinate(
        position.y,
        position.y + f64::from(height / 2.0),
        height,
        &mut random,
    );
    let z = horizontal_position.coordinate(position.z, position.z, width, &mut random);
    let velocity = DVec3::new(
        horizontal_velocity.velocity(movement.x, &mut random),
        vertical_velocity.velocity(movement.y, &mut random),
        horizontal_velocity.velocity(movement.z, &mut random),
    );
    let speed = speed.sample(&mut random);
    // The build script only emits `SpawnParticles` for option-less particle
    // types, so the simple payload always matches.
    world.send_particles(
        ParticleData::simple(particle),
        DVec3::new(x, y, z),
        0,
        velocity,
        f64::from(speed),
    );
}

#[cfg(test)]
mod tests;
