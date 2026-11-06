//! `InfestedMobEffect` behavior.

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::sound_events;
use steel_registry::vanilla_entities;

use super::MobEffectBehavior;
use crate::entity::LivingEntity;
use crate::entity::damage::DamageSource;
use crate::entity::spawn::create_entity_instance;
use crate::world::World;

/// Vanilla registers infested with a 10% chance per hit.
const CHANCE_TO_SPAWN: f32 = 0.1;

/// Infested mob effect behavior: a hurt mob sometimes releases silverfish.
pub struct InfestedBehavior;

impl InfestedBehavior {
    /// Vanilla `random -> Mth.randomBetweenInclusive(random, 1, 2)`.
    fn spawned_count() -> i32 {
        rand::random_range(1..=2)
    }

    fn spawn_silverfish(world: &Arc<World>, mob: &dyn LivingEntity, position: DVec3) {
        let Ok(silverfish) = create_entity_instance(world, &vanilla_entities::SILVERFISH, position)
        else {
            return;
        };
        // Vanilla `Mth.randomBetween(random, -PI/2, PI/2)`.
        let random_angle = rand::random::<f32>() * (FRAC_PI_2 - -FRAC_PI_2) + -FRAC_PI_2;
        let look = mob.look_angle();
        let (x, y, z) = (
            look.x as f32 * 0.3,
            look.y as f32 * 0.3 * 1.5,
            look.z as f32 * 0.3,
        );
        // JOML `Vector3f.rotateY`.
        let (sin, cos) = random_angle.sin_cos();
        let view_direction = DVec3::new(
            f64::from(cos * x + sin * z),
            f64::from(y),
            f64::from(-sin * x + cos * z),
        );
        silverfish.snap_to(position, rand::random::<f32>() * 360.0, 0.0);
        silverfish.set_velocity(view_direction);
        if world.try_add_entity(Arc::clone(&silverfish)).is_ok() {
            silverfish.play_sound(&sound_events::ENTITY_SILVERFISH_HURT, 1.0, 1.0);
        }
    }
}

impl MobEffectBehavior for InfestedBehavior {
    fn on_mob_hurt(
        &self,
        world: &Arc<World>,
        mob: &dyn LivingEntity,
        _amplifier: i32,
        _source: &DamageSource,
        _damage: f32,
    ) {
        if rand::random::<f32>() > CHANCE_TO_SPAWN {
            return;
        }
        let position = mob.position();
        let height = mob.bounding_box().height();
        for _ in 0..Self::spawned_count() {
            Self::spawn_silverfish(
                world,
                mob,
                DVec3::new(position.x, position.y + height / 2.0, position.z),
            );
        }
    }
}
