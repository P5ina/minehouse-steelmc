//! `BadOmenMobEffect` behavior.

use steel_registry::vanilla_mob_effects::RAID_OMEN;
use steel_utils::types::Difficulty;

use super::MobEffectBehavior;
use crate::entity::{Entity as _, LivingEntity, MobEffectInstance};
use crate::world::World;

/// Vanilla `BadOmenMobEffect.applyEffectTick`: Raid Omen lasts this long.
const RAID_OMEN_DURATION: i32 = 600;

/// Bad omen mob effect behavior: a player standing in a village turns it into
/// Raid Omen, remembering where.
pub struct BadOmenBehavior;

impl MobEffectBehavior for BadOmenBehavior {
    fn should_apply_effect_tick_this_tick(&self, _tick_count: i32, _amplifier: i32) -> bool {
        true
    }

    fn apply_effect_tick(&self, world: &World, user: &dyn LivingEntity, amplifier: i32) -> bool {
        let Some(player) = user.as_player() else {
            return true;
        };
        let pos = player.block_position();
        if player.is_spectator()
            || world.difficulty() == Difficulty::Peaceful
            || !world.is_village(pos)
        {
            return true;
        }
        let raid = world.get_raid_at(pos);
        if raid.is_none_or(|raid| raid.raid_omen_level() < raid.max_raid_omen_level()) {
            player.add_mob_effect(MobEffectInstance::with_duration(
                RAID_OMEN,
                RAID_OMEN_DURATION,
                amplifier,
            ));
            player.set_raid_omen_position(pos);
            return false;
        }
        true
    }
}
