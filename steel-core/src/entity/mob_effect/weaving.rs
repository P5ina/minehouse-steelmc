//! `WeavingMobEffect` behavior.

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{level_events, vanilla_blocks};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, Direction};

use super::MobEffectBehavior;
use crate::entity::{LivingEntity, RemovalReason};
use crate::world::{LevelReader, World};

/// Weaving mob effect behavior: a killed mob leaves cobwebs around it.
pub struct WeavingBehavior;

impl WeavingBehavior {
    /// Vanilla `random -> Mth.randomBetweenInclusive(random, 2, 3)`.
    fn max_cobwebs() -> usize {
        rand::random_range(2..=3)
    }

    /// Vanilla `spawnCobwebsRandomlyAround`: tries 15 random positions in the
    /// 3x3x3 cube around `pos`.
    fn spawn_cobwebs_randomly_around(world: &Arc<World>, pos: BlockPos) {
        let cobweb_count = Self::max_cobwebs();
        let mut positions = Vec::with_capacity(cobweb_count);

        for _ in 0..15 {
            let candidate = BlockPos::new(
                rand::random_range(pos.x() - 1..=pos.x() + 1),
                rand::random_range(pos.y() - 1..=pos.y() + 1),
                rand::random_range(pos.z() - 1..=pos.z() + 1),
            );
            let below = candidate.below();
            if !positions.contains(&candidate)
                && world.get_block_state(candidate).is_replaceable()
                && world.is_face_sturdy(world.get_block_state(below), below, Direction::Up)
            {
                positions.push(candidate);
                if positions.len() >= cobweb_count {
                    break;
                }
            }
        }

        for pos in positions {
            world.set_block(
                pos,
                vanilla_blocks::COBWEB.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
            world.level_event(level_events::ANIMATION_SPAWN_COBWEB, pos, 0, None);
        }
    }
}

impl MobEffectBehavior for WeavingBehavior {
    fn on_mob_removed(
        &self,
        world: &Arc<World>,
        mob: &dyn LivingEntity,
        _amplifier: i32,
        reason: RemovalReason,
    ) {
        if reason == RemovalReason::Killed
            && (mob.as_player().is_some() || world.get_game_rule(&MOB_GRIEFING))
        {
            Self::spawn_cobwebs_randomly_around(world, mob.block_position());
        }
    }
}
