//! Snowball item behavior (`SnowballItem`).
//!
//! Throwing a snowball spawns a [`SnowballEntity`] from the player's eye, shot
//! along their look direction, and consumes one snowball unless the player
//! has infinite materials. Mirrors vanilla `SnowballItem.use`.

use std::sync::Arc;

use crate::entity::SharedEntity;
use crate::world::World;
use glam::DVec3;
use steel_registry::blocks::properties::Direction;
use steel_registry::item_stack::ItemStack;

use steel_macros::item_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::{sound_events, vanilla_entities};

use super::throw_projectile::{ThrowParams, throw_item_projectile};
use crate::behavior::context::{InteractionResult, UseItemContext};
use crate::behavior::item::ItemBehavior;
use crate::behavior::projectile_item::{ProjectileItem, throwable_item_projectile};
use crate::entity::entities::SnowballEntity;
use crate::entity::next_entity_id;

/// Sound, shoot power and spread used when throwing a snowball.
const THROW: ThrowParams = ThrowParams {
    sound: &sound_events::ENTITY_SNOWBALL_THROW,
    sound_source: SoundSource::Neutral,
    sound_volume: 0.5,
    y_offset: 0.0,
    power: 1.5,
    uncertainty: 1.0,
};

/// Behavior for the snowball item.
#[item_behavior(class = "SnowballItem")]
pub struct SnowballItem;

impl ItemBehavior for SnowballItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }

    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let world = context.world;
        throw_item_projectile(context, &THROW, |spawn_pos| {
            SnowballEntity::new(
                &vanilla_entities::SNOWBALL,
                next_entity_id(),
                spawn_pos,
                Arc::downgrade(world),
            )
        });
        InteractionResult::Success
    }
}

impl ProjectileItem for SnowballItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        _direction: Direction,
    ) -> SharedEntity {
        throwable_item_projectile(
            SnowballEntity::new(
                &vanilla_entities::SNOWBALL,
                next_entity_id(),
                position,
                Arc::downgrade(world),
            ),
            stack,
        )
    }
}
