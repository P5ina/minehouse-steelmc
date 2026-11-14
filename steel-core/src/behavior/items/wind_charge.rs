//! Wind charge item behavior (vanilla `WindChargeItem.use`).

use crate::behavior::projectile_item::{DispenseConfig, ProjectileItem, charge_direction};
use crate::entity::AbstractHurtingProjectile as _;
use crate::world::World;
use std::sync::Arc;
use steel_registry::blocks::properties::Direction;
use steel_registry::item_stack::ItemStack;
use steel_registry::level_events;

use glam::DVec3;
use steel_macros::item_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::{sound_events, vanilla_entities};

use crate::behavior::context::{InteractionResult, UseItemContext};
use crate::behavior::item::ItemBehavior;
use crate::enchantment_helper;
use crate::entity::entities::WindChargeEntity;
use crate::entity::{Entity, Projectile, SharedEntity, next_entity_id};

/// Vanilla `PROJECTILE_SHOOT_POWER`.
const SHOOT_POWER: f32 = 1.5;
const SHOOT_UNCERTAINTY: f32 = 1.0;
const THROW_VOLUME: f32 = 0.5;
const THROW_PITCH_NUMERATOR: f32 = 0.4;
const THROW_PITCH_DIVISOR_MIN: f32 = 0.8;
const THROW_PITCH_DIVISOR_MAX: f32 = 1.2;

/// Behavior for the wind charge item.
#[item_behavior(class = "WindChargeItem")]
pub struct WindChargeItem;

impl ItemBehavior for WindChargeItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }

    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let player = context.player;
        let world = context.world;
        let player_pos = player.position();
        let charge = WindChargeEntity::new(
            &vanilla_entities::WIND_CHARGE,
            next_entity_id(),
            DVec3::new(player_pos.x, player.get_eye_y(), player_pos.z),
            Arc::downgrade(world),
        );
        let owner: SharedEntity = Arc::clone(player) as SharedEntity;
        charge.set_owner_entity(Some(&owner));
        let (yaw, pitch) = player.rotation();
        charge.shoot_from_rotation(
            player.as_ref(),
            pitch,
            yaw,
            0.0,
            SHOOT_POWER,
            SHOOT_UNCERTAINTY,
        );
        let mut thrown_item = context.inv.with_item(|item| item.clone());
        let charge: SharedEntity = Arc::new(charge);
        match world.try_add_entity(Arc::clone(&charge)) {
            Ok(()) => enchantment_helper::on_projectile_spawned(
                world,
                &mut thrown_item,
                charge.as_ref(),
                Some(&owner),
            ),
            Err(error) => log::debug!("failed to spawn wind charge: {error}"),
        }

        let pitch = THROW_PITCH_NUMERATOR
            / rand::random_range(THROW_PITCH_DIVISOR_MIN..THROW_PITCH_DIVISOR_MAX);
        world.play_sound_at(
            &sound_events::ENTITY_WIND_CHARGE_THROW,
            SoundSource::Neutral,
            player_pos,
            THROW_VOLUME,
            pitch,
            None,
        );
        player.award_stat(&vanilla_stat_types::ITEM_USED, thrown_item.item);
        let has_infinite_materials = player.has_infinite_materials();
        context
            .inv
            .with_item(|item| item.consume_one(has_infinite_materials));
        InteractionResult::Success
    }
}

impl ProjectileItem for WindChargeItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        _stack: &ItemStack,
        direction: Direction,
    ) -> SharedEntity {
        let direction = charge_direction(direction);
        let charge = WindChargeEntity::new(
            &vanilla_entities::WIND_CHARGE,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        );
        charge.assign_directional_movement(direction);
        charge.set_velocity(direction);
        Arc::new(charge)
    }

    fn create_dispense_config(&self) -> DispenseConfig {
        DispenseConfig::charge(level_events::SOUND_WIND_CHARGE_SHOOT)
    }
}
