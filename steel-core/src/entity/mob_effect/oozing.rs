//! `OozingMobEffect` behavior.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::vanilla_entities;
use steel_registry::vanilla_game_rules::MAX_ENTITY_CRAMMING;

use super::MobEffectBehavior;
use crate::entity::spawn::create_entity_instance;
use crate::entity::{LivingEntity, RemovalReason};
use crate::world::World;

/// Slimes within this many blocks of the dying mob count against cramming.
const RADIUS_TO_CHECK_SLIMES: f64 = 2.0;
/// Size of every slime the effect spawns.
const SLIME_SIZE: i32 = 2;
/// Vanilla registers oozing with `random -> 2`.
const SPAWNED_COUNT: i32 = 2;

/// Oozing mob effect behavior: a killed mob splits into size-2 slimes.
pub struct OozingBehavior;

impl OozingBehavior {
    /// Vanilla `numberOfSlimesToSpawn`.
    fn number_of_slimes_to_spawn(
        max_entity_cramming: i32,
        nearby_slimes: impl FnOnce(i32) -> i32,
        number_requested: i32,
    ) -> i32 {
        if max_entity_cramming < 1 {
            return number_requested;
        }
        (max_entity_cramming - nearby_slimes(max_entity_cramming))
            .max(0)
            .min(number_requested)
    }

    fn spawn_slime_offspring(world: &Arc<World>, position: DVec3) {
        let Ok(slime) = create_entity_instance(world, &vanilla_entities::SLIME, position) else {
            return;
        };
        let Some(cube) = slime.as_cube_mob() else {
            return;
        };
        cube.set_size(SLIME_SIZE, true);
        slime.snap_to(position, rand::random::<f32>() * 360.0, 0.0);
        let _ = world.try_add_entity(slime);
    }
}

impl MobEffectBehavior for OozingBehavior {
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
        let max_entity_cramming = world.get_game_rule(&MAX_ENTITY_CRAMMING);
        let nearby_slimes = |max_results: i32| {
            let mob_id = mob.id();
            let count = world
                .get_entities_in_aabb_matching(
                    &mob.bounding_box().inflate(RADIUS_TO_CHECK_SLIMES),
                    |entity| {
                        entity.entity_type() == &vanilla_entities::SLIME && entity.id() != mob_id
                    },
                )
                .len();
            i32::try_from(count).unwrap_or(i32::MAX).min(max_results)
        };
        let count =
            Self::number_of_slimes_to_spawn(max_entity_cramming, nearby_slimes, SPAWNED_COUNT);

        let position = mob.position();
        for _ in 0..count {
            Self::spawn_slime_offspring(
                world,
                DVec3::new(position.x, position.y + 0.5, position.z),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OozingBehavior;

    #[test]
    fn slime_count_respects_entity_cramming_room() {
        let spawn =
            |cramming, nearby| OozingBehavior::number_of_slimes_to_spawn(cramming, |_| nearby, 2);
        assert_eq!(spawn(24, 0), 2);
        assert_eq!(spawn(24, 23), 1);
        assert_eq!(spawn(24, 24), 0);
        // Cramming disabled: always the requested count.
        assert_eq!(spawn(0, 100), 2);
    }
}
