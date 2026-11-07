//! Vanilla `AbstractHurtingProjectile`: the self-propelled projectiles without
//! gravity (fireballs, wither skulls, dragon fireballs, wind charges).

use std::f32::consts::PI;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_utils::locks::SyncMutex;

use crate::entity::projectile::{Projectile, ProjectileHit};
use crate::entity::{Entity, RemovalReason, SharedEntity};

/// Vanilla initial `AbstractHurtingProjectile.accelerationPower`.
pub const INITIAL_ACCELERATION_POWER: f64 = 0.1;
const ACCELERATION_POWER_TAG: &str = "acceleration_power";
const ROTATION_SPEED: f32 = 0.2;
const BURN_SECONDS_TICKS: i32 = 20;
/// Vanilla `Vec3.normalize` treats shorter vectors as zero.
const NORMALIZE_EPSILON: f64 = 1.0e-5_f32 as f64;

/// Runtime fields of vanilla `AbstractHurtingProjectile`.
#[derive(Debug)]
pub struct HurtingProjectileBase {
    acceleration_power: SyncMutex<f64>,
}

impl HurtingProjectileBase {
    /// Creates the state with the default acceleration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            acceleration_power: SyncMutex::new(INITIAL_ACCELERATION_POWER),
        }
    }
}

impl Default for HurtingProjectileBase {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `Vec3.normalize`.
#[must_use]
pub fn normalize_or_zero(vector: DVec3) -> DVec3 {
    let length = vector.length();
    if length < NORMALIZE_EPSILON {
        DVec3::ZERO
    } else {
        vector / length
    }
}

/// Vanilla `(float)(angle * 180.0F / (float)Math.PI)`.
fn vanilla_degrees(radians: f64) -> f32 {
    (radians * 180.0 / f64::from(PI)) as f32
}

/// Behavior shared by projectiles extending vanilla `AbstractHurtingProjectile`.
pub trait AbstractHurtingProjectile: Projectile {
    /// Returns the shared hurting-projectile state.
    fn hurting_projectile_base(&self) -> &HurtingProjectileBase;

    /// Speed added along the flight direction every tick.
    fn acceleration_power(&self) -> f64 {
        *self.hurting_projectile_base().acceleration_power.lock()
    }

    /// Sets the speed added along the flight direction every tick.
    fn set_acceleration_power(&self, acceleration_power: f64) {
        *self.hurting_projectile_base().acceleration_power.lock() = acceleration_power;
    }

    /// Velocity multiplier per tick in air.
    fn get_inertia(&self) -> f32 {
        0.95
    }

    /// Velocity multiplier per tick in water.
    fn get_liquid_inertia(&self) -> f32 {
        0.8
    }

    /// Whether the projectile sets itself on fire every tick.
    fn should_burn(&self) -> bool {
        true
    }

    /// Vanilla constructor with a direction: start flying along it at the
    /// acceleration power.
    fn assign_directional_movement(&self, direction: DVec3) {
        self.set_velocity(normalize_or_zero(direction) * self.acceleration_power());
        self.mark_velocity_sync();
    }

    /// Vanilla constructor from a shooter: placed at its feet, flying along
    /// `direction`, owned by it and facing its rotation.
    fn shoot_from_mob(&self, owner: &SharedEntity, direction: DVec3) {
        if let Err(error) = self.try_set_position(owner.position()) {
            log::debug!("hurting projectile could not move to its shooter: {error}");
        }
        self.assign_directional_movement(direction);
        self.set_owner_entity(Some(owner));
        self.set_rotation(owner.rotation());
    }

    /// Vanilla `applyInertia`.
    fn apply_hurting_inertia(&self) {
        let movement = self.velocity();
        // VANILLA CLIENT-LOCAL: the bubbles in water are client particles.
        let inertia = if self.is_in_water() {
            self.get_liquid_inertia()
        } else {
            self.get_inertia()
        };
        self.set_velocity(
            (movement + normalize_or_zero(movement) * self.acceleration_power())
                * f64::from(inertia),
        );
    }

    /// Vanilla `ProjectileUtil.rotateTowardsMovement`.
    fn rotate_towards_movement(&self, rotation_speed: f32) {
        rotate_towards_movement(self, rotation_speed);
    }

    /// Vanilla `AbstractHurtingProjectile.tick`.
    fn hurting_projectile_tick(self: Arc<Self>) {
        self.set_old_position_to_current();
        self.base().set_old_rotation_to_current();

        let owner = self.get_owner();
        self.apply_hurting_inertia();
        let Some(world) = self.level() else {
            return;
        };
        let owner_present = owner.is_none_or(|owner| !owner.is_removed());
        if !owner_present || !world.is_full_chunk_loaded_at(self.block_position()) {
            self.set_removed(RemovalReason::Discarded);
            return;
        }

        let hit = self.get_hit_result_on_move_vector();
        let new_position = hit.as_ref().map_or_else(
            || self.position() + self.velocity(),
            ProjectileHit::location,
        );
        self.rotate_towards_movement(ROTATION_SPEED);
        if let Err(error) = self.try_set_position(new_position) {
            log::debug!(
                "failed to advance hurting projectile {}: {error}",
                self.id()
            );
            self.set_removed(RemovalReason::Discarded);
            return;
        }
        self.apply_effects_from_blocks();
        self.projectile_base_tick();
        if self.should_burn() {
            self.ignite_for_ticks(BURN_SECONDS_TICKS);
        }
        if let Some(hit) = hit
            && self.is_alive()
            && !self.is_world_change_pending()
        {
            self.hit_target_or_deflect_self(&hit);
        }
        // VANILLA CLIENT-LOCAL: `createParticleTrail` only adds client particles.
    }

    /// Vanilla `onDeflection`: a hit resets the acceleration, other deflections halve it.
    fn on_hurting_deflection(&self, by_attack: bool) {
        if by_attack {
            self.set_acceleration_power(INITIAL_ACCELERATION_POWER);
        } else {
            self.set_acceleration_power(self.acceleration_power() * 0.5);
        }
    }

    /// Vanilla `canHitEntity`: also skips entities without physics.
    fn hurting_can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.base_can_hit_entity(entity) && !entity.no_physics()
    }

    /// Saves `acceleration_power`.
    fn save_hurting_projectile(&self, nbt: &mut NbtCompound) {
        nbt.insert(ACCELERATION_POWER_TAG, self.acceleration_power());
    }

    /// Loads `acceleration_power`.
    fn load_hurting_projectile(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_acceleration_power(
            nbt.double(ACCELERATION_POWER_TAG)
                .unwrap_or(INITIAL_ACCELERATION_POWER),
        );
    }
}

/// Vanilla `ProjectileUtil.rotateTowardsMovement` for any entity.
pub(crate) fn rotate_towards_movement(entity: &(impl Entity + ?Sized), rotation_speed: f32) {
    let movement = entity.velocity();
    if movement.length_squared() == 0.0 {
        return;
    }
    let horizontal = movement.x.hypot(movement.z);
    let y_rot = vanilla_degrees(movement.z.atan2(movement.x)) + 90.0;
    let x_rot = vanilla_degrees(horizontal.atan2(movement.y)) - 90.0;
    let (mut y_rot_old, mut x_rot_old) = entity.base().old_rotation();
    while x_rot - x_rot_old < -180.0 {
        x_rot_old -= 360.0;
    }
    while x_rot - x_rot_old >= 180.0 {
        x_rot_old += 360.0;
    }
    while y_rot - y_rot_old < -180.0 {
        y_rot_old -= 360.0;
    }
    while y_rot - y_rot_old >= 180.0 {
        y_rot_old += 360.0;
    }
    entity.base().set_old_rotation((y_rot_old, x_rot_old));
    entity.set_rotation((
        y_rot_old + rotation_speed * (y_rot - y_rot_old),
        x_rot_old + rotation_speed * (x_rot - x_rot_old),
    ));
}
