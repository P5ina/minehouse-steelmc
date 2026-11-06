//! Bow item behavior (vanilla `BowItem`).

use std::sync::Arc;

use steel_macros::item_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_events;
use steel_registry::stat::vanilla_stat_types;

use super::projectile_weapon::{draw_player_ammo, find_player_arrow, shoot_from_rotation};
use crate::behavior::{InteractionResult, ItemBehavior, ItemUseAnimation, UseItemContext};
use crate::entity::{Entity, LivingEntity, SharedEntity};
use crate::world::World;

/// Vanilla `BowItem.getUseDuration`.
const USE_DURATION: i32 = 72000;
/// Vanilla `BowItem.releaseUsing` arrow speed at full draw.
const FULL_DRAW_POWER: f32 = 3.0;
/// Vanilla `BowItem.releaseUsing` minimum draw that still fires.
const MIN_DRAW_POWER: f32 = 0.1;

/// Vanilla `BowItem.getPowerForTime`: draw strength after `time_held` ticks.
#[must_use]
pub fn get_power_for_time(time_held: i32) -> f32 {
    let pow = time_held as f32 / 20.0;
    ((pow * pow + pow * 2.0) / 3.0).min(1.0)
}

/// The bow.
#[item_behavior(class = "BowItem")]
pub struct BowItem;

impl ItemBehavior for BowItem {
    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let found_projectile = find_player_arrow(context.player).is_some();
        if !context.player.has_infinite_materials() && !found_projectile {
            return InteractionResult::Fail;
        }
        context.player.start_using_item(context.hand);
        InteractionResult::Consume
    }

    fn get_use_animation(&self, _stack: &ItemStack) -> ItemUseAnimation {
        ItemUseAnimation::Bow
    }

    fn get_use_duration(&self, _stack: &ItemStack, _user: &dyn LivingEntity) -> i32 {
        USE_DURATION
    }

    fn release_using(
        &self,
        stack: &mut ItemStack,
        world: &Arc<World>,
        user: &dyn LivingEntity,
        time_left: i32,
    ) -> bool {
        let Some(player) = world.players.get_by_entity_id(user.id()) else {
            return false;
        };
        let Some((slot, projectile)) = find_player_arrow(&player) else {
            return false;
        };
        let time_held = self.get_use_duration(stack, user) - time_left;
        let pow = get_power_for_time(time_held);
        if pow < MIN_DRAW_POWER {
            return false;
        }

        let fired = draw_player_ammo(&player, stack, slot, &projectile);
        if !fired.is_empty() {
            let shooter: SharedEntity = Arc::clone(&player) as SharedEntity;
            shoot_from_rotation(
                world,
                &shooter,
                stack,
                &fired,
                pow * FULL_DRAW_POWER,
                1.0,
                pow >= 1.0,
                player.has_infinite_materials(),
            );
        }

        world.play_sound_at(
            &sound_events::ENTITY_ARROW_SHOOT,
            SoundSource::Players,
            player.position(),
            1.0,
            1.0 / (rand::random::<f32>() * 0.4 + 1.2) + pow * 0.5,
            None,
        );
        player.award_stat(&vanilla_stat_types::ITEM_USED, stack.item());
        true
    }
}
