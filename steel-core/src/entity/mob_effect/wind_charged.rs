//! `WindChargedMobEffect` behavior.

use std::sync::Arc;

use steel_protocol::packets::game::ExplosionParticlePalette;
use steel_registry::particle_type::ParticleData;
use steel_registry::sound_event::SoundEventHolder;
use steel_registry::{sound_events, vanilla_particle_types};

use super::MobEffectBehavior;
use crate::entity::{LivingEntity, RemovalReason};
use crate::world::{
    ExplosionInteraction, ExplosionOptions, WindChargeExplosionDamageCalculator, World,
};

/// Wind charged mob effect behavior: a killed mob bursts into a wind gust that
/// pushes nearby entities and triggers doors, levers, buttons, bells and candles.
pub struct WindChargedBehavior;

impl MobEffectBehavior for WindChargedBehavior {
    fn on_mob_removed(
        &self,
        world: &Arc<World>,
        mob: &dyn LivingEntity,
        _amplifier: i32,
        reason: RemovalReason,
    ) {
        if reason != RemovalReason::Killed {
            return;
        }
        let position = mob.position();
        let center = position.with_y(position.y + f64::from(mob.base().dimensions().height / 2.0));
        let gust_strength = 3.0 + rand::random::<f32>() * 2.0;
        let Ok(block_particles) = ExplosionParticlePalette::try_new(Vec::new()) else {
            return;
        };
        let calculator = WindChargeExplosionDamageCalculator;
        let mut options =
            ExplosionOptions::new(center, gust_strength, ExplosionInteraction::Trigger);
        options.source = world.get_entity_by_id(mob.id());
        options.damage_calculator = Some(&calculator);
        options.small_explosion_particle =
            ParticleData::simple(&vanilla_particle_types::GUST_EMITTER_SMALL);
        options.large_explosion_particle =
            ParticleData::simple(&vanilla_particle_types::GUST_EMITTER_LARGE);
        options.block_particles = block_particles;
        options.explosion_sound =
            SoundEventHolder::registry(&sound_events::ENTITY_BREEZE_WIND_BURST);
        world.explode(options);
    }
}
