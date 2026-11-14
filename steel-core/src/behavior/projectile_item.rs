//! Items that become projectiles when fired without a thrower (Vanilla
//! `ProjectileItem`): dispensers and ominous item spawners create the
//! projectile at a position and shoot it along a direction.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::properties::Direction;
use steel_registry::item_stack::ItemStack;

use crate::behavior::items::projectile_weapon::{as_abstract_arrow, spawn_arrow};
use crate::enchantment_helper;
use crate::entity::projectile::triangle_random;
use crate::entity::{Entity, SharedEntity, ThrowableItemProjectile};
use crate::world::World;

/// Where a dispenser puts the projectile (Vanilla `ProjectileItem.PositionFunction`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DispensePosition {
    /// Vanilla `DispenserBlock.getDispensePosition(source, distance, offset)`:
    /// `distance` blocks from the dispenser's center towards its facing,
    /// plus a fixed world-space `offset`.
    FromCenter {
        /// Distance along the facing.
        distance: f64,
        /// Offset added in world space.
        offset: DVec3,
    },
    /// Vanilla `FireworkRocketItem.getEntityJustOutsideOfBlockPos`.
    JustOutsideBlock,
}

/// Distance `FireworkRocketItem.getEntityJustOutsideOfBlockPos` moves out of
/// the block (the float `0.50001` widened to double).
const JUST_OUTSIDE_BLOCK_DISTANCE: f64 = 0.500_009_999_999_747_4;

impl DispensePosition {
    /// Resolves the spawn point for a dispenser at `pos` facing `direction`.
    #[must_use]
    pub fn resolve(self, center: DVec3, direction: Direction) -> DVec3 {
        let step = direction.offset_vec().as_dvec3();
        match self {
            Self::FromCenter { distance, offset } => center + step * distance + offset,
            Self::JustOutsideBlock => center + step * JUST_OUTSIDE_BLOCK_DISTANCE,
        }
    }
}

/// How a dispenser fires a projectile item (Vanilla `ProjectileItem.DispenseConfig`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DispenseConfig {
    /// Where the projectile starts.
    pub position: DispensePosition,
    /// `shoot` uncertainty.
    pub uncertainty: f32,
    /// `shoot` power.
    pub power: f32,
    /// Level event played instead of `SOUND_DISPENSER_PROJECTILE_LAUNCH`.
    pub override_dispense_event: Option<i32>,
}

impl DispenseConfig {
    /// Vanilla `DispenseConfig.DEFAULT`.
    pub const DEFAULT: Self = Self {
        position: DispensePosition::FromCenter {
            distance: 0.7,
            offset: DVec3::new(0.0, 0.1, 0.0),
        },
        uncertainty: 6.0,
        power: 1.1,
        override_dispense_event: None,
    };

    /// The config of thrown potions and experience bottles: half the spread
    /// and a quarter more power than [`Self::DEFAULT`].
    pub const LOBBED: Self = Self {
        uncertainty: Self::DEFAULT.uncertainty * 0.5,
        power: Self::DEFAULT.power * 1.25,
        ..Self::DEFAULT
    };

    /// The config of fire and wind charges: one block out of the center,
    /// with the given launch level event.
    #[must_use]
    pub const fn charge(dispense_event: i32) -> Self {
        Self {
            position: DispensePosition::FromCenter {
                distance: 1.0,
                offset: DVec3::ZERO,
            },
            uncertainty: 6.666_666_5,
            power: 1.0,
            override_dispense_event: Some(dispense_event),
        }
    }
}

/// An item that dispensers and ominous item spawners turn into a projectile
/// (Vanilla `ProjectileItem`).
pub trait ProjectileItem: Send + Sync {
    /// Creates the projectile at `position`, not yet shot or added to the
    /// world (Vanilla `asProjectile`).
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        direction: Direction,
    ) -> SharedEntity;

    /// Vanilla `createDispenseConfig`.
    fn create_dispense_config(&self) -> DispenseConfig {
        DispenseConfig::DEFAULT
    }
}

/// Vanilla `Projectile.spawnProjectileUsingShoot(projectile, ...)`: shoots
/// `projectile` along `direction`, adds it to the world and runs the
/// `projectile_spawned` enchantment effects of `stack`. Returns `false` when
/// the world rejected the projectile.
pub fn spawn_projectile_using_shoot(
    world: &Arc<World>,
    projectile: &SharedEntity,
    stack: &ItemStack,
    direction: DVec3,
    power: f32,
    uncertainty: f32,
) -> bool {
    if let Some(projectile) = projectile.as_projectile() {
        projectile.shoot(direction, power, uncertainty);
    }
    let owner = projectile.projectile_owner();
    if as_abstract_arrow(projectile).is_some() {
        return spawn_arrow(world, projectile, stack, owner.as_ref());
    }
    if let Err(error) = world.try_add_entity(Arc::clone(projectile)) {
        log::debug!("failed to spawn shot projectile: {error}");
        return false;
    }
    let mut stack = stack.clone();
    enchantment_helper::on_projectile_spawned(
        world,
        &mut stack,
        projectile.as_ref(),
        owner.as_ref(),
    );
    true
}

/// Spread of a dispensed fire or wind charge around the facing.
const CHARGE_SPREAD: f64 = 0.114_850_000_000_000_01;

/// Vanilla `FireChargeItem`/`WindChargeItem.asProjectile` direction: each
/// axis of the facing jittered by a triangle distribution.
#[must_use]
pub fn charge_direction(direction: Direction) -> DVec3 {
    let step = direction.offset_vec().as_dvec3();
    DVec3::new(
        triangle_random(step.x, CHARGE_SPREAD),
        triangle_random(step.y, CHARGE_SPREAD),
        triangle_random(step.z, CHARGE_SPREAD),
    )
}

/// Vanilla `ThrowableItemProjectile(type, x, y, z, level, itemStack)`: the
/// projectile renders one of `stack`.
pub(crate) fn throwable_item_projectile<E>(projectile: E, stack: &ItemStack) -> SharedEntity
where
    E: ThrowableItemProjectile + Entity + 'static,
{
    projectile.set_item_clamped(stack.clone());
    Arc::new(projectile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_position_is_point_seven_out_and_a_tenth_up() {
        let center = DVec3::new(0.5, 64.5, 0.5);
        let position = DispenseConfig::DEFAULT
            .position
            .resolve(center, Direction::North);
        assert!(position.abs_diff_eq(DVec3::new(0.5, 64.6, -0.2), 1.0e-9));
    }

    #[test]
    fn just_outside_block_clears_the_face() {
        let center = DVec3::new(0.5, 64.5, 0.5);
        let position = DispensePosition::JustOutsideBlock.resolve(center, Direction::Up);
        assert!(position.y > 65.0);
        assert!(position.y < 65.000_1);
    }

    #[test]
    fn lobbed_and_charge_configs_match_vanilla() {
        assert_eq!(DispenseConfig::LOBBED.uncertainty, 3.0);
        assert_eq!(DispenseConfig::LOBBED.power, 1.1 * 1.25);
        let charge = DispenseConfig::charge(1018);
        assert_eq!(charge.power, 1.0);
        assert_eq!(charge.override_dispense_event, Some(1018));
    }
}
