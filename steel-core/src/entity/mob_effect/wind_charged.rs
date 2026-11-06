//! `WindChargedMobEffect` behavior.

use super::MobEffectBehavior;

// TODO: Vanilla `WindChargedMobEffect.onMobRemoved` triggers a small,
// blockless wind-burst explosion (`AbstractWindCharge.EXPLOSION_DAMAGE_CALCULATOR`)
// at the mob's death position. Blocked on `ExplosionInteraction::Trigger`
// (`Explosion.BlockInteraction.TRIGGER_BLOCK`), which wind charges need too.
/// Wind charged mob effect behavior.
pub struct WindChargedBehavior;

impl MobEffectBehavior for WindChargedBehavior {}
