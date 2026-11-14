//! Bottle o' enchanting item behavior (`ExperienceBottleItem`).
//!
//! Throwing the bottle spawns an [`ExperienceBottleEntity`] from the player's
//! eye, aimed 20 degrees above their look direction, and consumes one bottle
//! unless the player has infinite materials. Mirrors vanilla
//! `ExperienceBottleItem.use`.

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
use crate::behavior::projectile_item::{DispenseConfig, ProjectileItem, throwable_item_projectile};
use crate::entity::entities::ExperienceBottleEntity;
use crate::entity::next_entity_id;

/// Sound, aim offset, shoot power and spread used when throwing the bottle.
const THROW: ThrowParams = ThrowParams {
    sound: &sound_events::ENTITY_EXPERIENCE_BOTTLE_THROW,
    sound_source: SoundSource::Neutral,
    sound_volume: 0.5,
    y_offset: -20.0,
    power: 0.7,
    uncertainty: 1.0,
};

/// Behavior for the bottle o' enchanting item.
#[item_behavior(class = "ExperienceBottleItem")]
pub struct ExperienceBottleItem;

impl ItemBehavior for ExperienceBottleItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }

    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let world = context.world;
        throw_item_projectile(context, &THROW, |spawn_pos| {
            ExperienceBottleEntity::new(
                &vanilla_entities::EXPERIENCE_BOTTLE,
                next_entity_id(),
                spawn_pos,
                Arc::downgrade(world),
            )
        });
        InteractionResult::Success
    }
}

impl ProjectileItem for ExperienceBottleItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        _direction: Direction,
    ) -> SharedEntity {
        throwable_item_projectile(
            ExperienceBottleEntity::new(
                &vanilla_entities::EXPERIENCE_BOTTLE,
                next_entity_id(),
                position,
                Arc::downgrade(world),
            ),
            stack,
        )
    }

    fn create_dispense_config(&self) -> DispenseConfig {
        DispenseConfig::LOBBED
    }
}
