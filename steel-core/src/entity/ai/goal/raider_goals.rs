//! Goals shared by raiders (Vanilla `Raider.HoldGroundAttackGoal` and
//! `AbstractIllager.RaiderOpenDoorGoal`).

use glam::DVec3;
use steel_registry::vanilla_entities;

use super::open_door::OpenDoorGoal;
use super::selector::{Goal, GoalControls};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{PathfinderMob, Raider, SharedEntity};

/// Vanilla `HoldGroundAttackGoal.shoutTargeting` range.
const SHOUT_RANGE: f64 = 8.0;
/// Vanilla `HoldGroundAttackGoal.tick`: one ambient sound every 50 ticks on average.
const AMBIENT_SOUND_CHANCE: i32 = 50;

/// Vanilla `Raider.HoldGroundAttackGoal`: a patrolling raider that spots a
/// target stands still, rallies the raiders around it, and only charges once
/// the target comes within `hostile_radius`.
pub(crate) struct HoldGroundAttackGoal {
    hostile_radius_sqr: f32,
    shout_targeting: TargetingConditions,
}

impl HoldGroundAttackGoal {
    #[must_use]
    pub(crate) const fn new(hostile_radius: f32) -> Self {
        Self {
            hostile_radius_sqr: hostile_radius * hostile_radius,
            shout_targeting: TargetingConditions::for_non_combat()
                .range(SHOUT_RANGE)
                .ignore_line_of_sight()
                .ignore_invisibility_testing(),
        }
    }

    /// Vanilla `getNearbyEntities(Raider.class, shoutTargeting, mob, box.inflate(8))`.
    fn nearby_raiders(&self, mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
        let Some(world) = mob.level() else {
            return Vec::new();
        };
        let search_box = mob
            .bounding_box()
            .inflate_xyz(SHOUT_RANGE, SHOUT_RANGE, SHOUT_RANGE);
        world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.as_raider().is_some()
                && entity.as_living_entity().is_some_and(|raider| {
                    self.shout_targeting.test(world.as_ref(), Some(mob), raider)
                })
        })
    }
}

impl Goal for HoldGroundAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        // TODO: Vanilla also requires `getCurrentRaid() == null`; until raids exist
        // no raider is ever in one.
        let Some(raider) = mob.as_raider() else {
            return false;
        };
        raider.is_patrolling()
            && mob.target().is_some()
            && !mob.is_aggressive()
            && mob
                .last_hurt_by_mob()
                .is_none_or(|attacker| attacker.entity_type() != &vanilla_entities::PLAYER)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
        let target = mob.target();
        for entity in self.nearby_raiders(mob) {
            if let Some(other) = entity.as_mob() {
                let _ = other.set_target(target.as_ref());
            }
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(target) = mob.target() else {
            return;
        };
        for entity in self.nearby_raiders(mob) {
            if let Some(other) = entity.as_mob() {
                let _ = other.set_target(Some(&target));
                other.set_aggressive(true);
            }
        }
        mob.set_aggressive(true);
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        if mob.position().distance_squared(target.position()) > f64::from(self.hostile_radius_sqr) {
            let target_position = target.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
                30.0,
                30.0,
            );
            if rand::random_range(0..AMBIENT_SOUND_CHANCE) == 0 {
                mob.play_ambient_sound();
            }
        } else {
            mob.set_aggressive(true);
        }
    }
}

/// Vanilla `AbstractIllager.RaiderOpenDoorGoal`: illagers only open doors
/// during a raid.
pub(crate) struct RaiderOpenDoorGoal {
    inner: OpenDoorGoal,
}

impl RaiderOpenDoorGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            inner: OpenDoorGoal::new(false),
        }
    }
}

impl Goal for RaiderOpenDoorGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_use(mob) && mob.as_raider().is_some_and(Raider::has_active_raid)
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

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.inner.tick(mob, entity);
    }
}
