//! `RaidOmenMobEffect` behavior.

use super::MobEffectBehavior;
use crate::entity::{Entity as _, LivingEntity};
use crate::world::World;

/// Raid omen mob effect behavior: on its last tick the remembered position
/// starts or extends a raid.
pub struct RaidOmenBehavior;

impl MobEffectBehavior for RaidOmenBehavior {
    fn should_apply_effect_tick_this_tick(&self, tick_count: i32, _amplifier: i32) -> bool {
        tick_count == 1
    }

    fn apply_effect_tick(&self, world: &World, user: &dyn LivingEntity, _amplifier: i32) -> bool {
        let Some(player) = user.as_player() else {
            return true;
        };
        if player.is_spectator() {
            return true;
        }
        let Some(pos) = player.raid_omen_position() else {
            return true;
        };
        world.raids().create_or_extend_raid(world, player, pos);
        player.clear_raid_omen_position();
        false
    }
}
