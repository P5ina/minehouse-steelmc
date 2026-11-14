//! Vanilla `ArmorStandItem`: stand an armor stand on the clicked block.

use std::sync::Arc;

use glam::DVec3;
use steel_macros::item_behavior;
use steel_math::wrap_degrees;
use steel_protocol::packets::game::SoundSource;
use steel_registry::{sound_events, vanilla_entities, vanilla_game_events};
use steel_utils::{Direction, WorldAabb};

use crate::behavior::BlockCollisionContext;
use crate::behavior::{InteractionResult, ItemBehavior, UseOnContext};
use crate::entity::{
    Entity, EntitySpawnPlacement, EntitySpawnReason, EntitySpawnRequest, add_spawned_entity,
    create_spawned_entity,
};
use crate::physics::CollisionWorld as _;
use crate::physics::WorldCollisionProvider;

/// Vanilla `ArmorStandItem`.
#[item_behavior]
pub struct ArmorStandItem;

impl ItemBehavior for ArmorStandItem {
    /// Vanilla `ArmorStandItem.useOn`.
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        if context.hit_result.direction == Direction::Down {
            return InteractionResult::Fail;
        }
        let world = context.world;
        let player = context.player;
        let block_pos = context.build_place_context().place_pos();
        let bottom_center = DVec3::new(
            f64::from(block_pos.x()) + 0.5,
            f64::from(block_pos.y()),
            f64::from(block_pos.z()) + 0.5,
        );
        let dimensions = vanilla_entities::ARMOR_STAND.dimensions;
        let half_width = f64::from(dimensions.width) / 2.0;
        let space = WorldAabb::new(
            bottom_center.x - half_width,
            bottom_center.y,
            bottom_center.z - half_width,
            bottom_center.x + half_width,
            bottom_center.y + f64::from(dimensions.height),
            bottom_center.z + half_width,
        );
        // Vanilla `noCollision(null, box)` and `getEntities(null, box).isEmpty()`.
        let collisions = WorldCollisionProvider::new(world);
        if collisions.has_entity_collision(&space)
            || collisions.has_block_collision_with_context(&space, BlockCollisionContext::empty())
            || world.has_entity_in_aabb_matching(&space, |entity| !entity.is_spectator())
        {
            return InteractionResult::Fail;
        }

        let stack = context.inv.with_item(|item| item.clone());
        let request = EntitySpawnRequest {
            entity_type: &vanilla_entities::ARMOR_STAND,
            placement: EntitySpawnPlacement::Block {
                pos: block_pos,
                try_move_down: true,
                moved_up: true,
            },
            reason: EntitySpawnReason::SpawnItemUse,
            finalize_spawn: true,
            play_ambient_sound: false,
            item_stack: Some(&stack),
            user_is_operator: player.is_operator(),
        };
        let Ok(stand) = create_spawned_entity(world, request) else {
            return InteractionResult::Fail;
        };
        let y_rot = ((wrap_degrees(player.rotation().0 - 180.0) + 22.5) / 45.0).floor() * 45.0;
        stand.set_rotation((y_rot, 0.0));
        stand.base().set_old_rotation_to_current();
        if add_spawned_entity(world, Arc::clone(&stand)).is_err() {
            return InteractionResult::Fail;
        }
        world.play_sound_at(
            &sound_events::ENTITY_ARMOR_STAND_PLACE,
            SoundSource::Blocks,
            stand.position(),
            0.75,
            0.8,
            None,
        );
        stand.game_event_with_source_entity(
            &vanilla_game_events::ENTITY_PLACE,
            Some(player.as_ref()),
        );

        context.inv.with_item(|item| item.shrink(1));
        InteractionResult::Success
    }
}
