//! Mobs that attack from range (Vanilla `RangedAttackMob`).

use steel_utils::types::Difficulty;

use crate::entity::{Mob, SharedEntity};
use crate::world::World;

/// Vanilla `RangedAttackMob`.
pub trait RangedAttackMob: Mob {
    /// Fires at `target`; `power` grows with distance, from 0.1 to 1.
    ///
    /// `entity` is this mob as a shared entity, so projectiles can name it as their owner.
    fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, power: f32);

    /// Spread of the mob's shots: 14 minus 4 per difficulty level by default.
    fn ranged_attack_uncertainty(&self, world: &World) -> f32 {
        default_ranged_attack_uncertainty(world.difficulty())
    }
}

/// Vanilla `RangedAttackMob.rangedAttackUncertainty` before mobs override it.
#[must_use]
pub(crate) fn default_ranged_attack_uncertainty(difficulty: Difficulty) -> f32 {
    f32::from(14 - (difficulty as u8) * 4)
}
