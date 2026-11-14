//! Item frame items: hang a frame on the clicked face.

use std::sync::Arc;

use steel_macros::item_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_game_events;

use crate::behavior::{InteractionResult, ItemBehavior, UseOnContext};
use crate::entity::block_attached_entity::BlockAttachedEntity as _;
use crate::entity::entities::ItemFrameEntity;
use crate::entity::{
    Entity, SharedEntity, add_spawned_entity, apply_item_stack_components, next_entity_id,
};
use crate::world::LevelReader as _;
use crate::world::game_event::GameEventContext;

/// Vanilla `ItemFrameItem`, the `HangingEntityItem` of item frames and glow item frames.
#[item_behavior]
pub struct ItemFrameItem {
    #[json_arg(vanilla_entities, json = "type")]
    entity_type: EntityTypeRef,
}

impl ItemFrameItem {
    /// Creates the behavior for the item that hangs `entity_type`.
    #[must_use]
    pub const fn new(entity_type: EntityTypeRef) -> Self {
        Self { entity_type }
    }
}

impl ItemBehavior for ItemFrameItem {
    /// Vanilla `HangingEntityItem.useOn` with `ItemFrameItem.mayPlace`.
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        let world = context.world;
        let player = context.player;
        let clicked_face = context.hit_result.direction;
        let pos = context.hit_result.block_pos.relative(clicked_face);
        let stack = context.inv.with_item(|item| item.clone());
        if world.is_outside_build_height(pos.y())
            || !player.may_use_item_at(pos, clicked_face, &stack)
        {
            return InteractionResult::Fail;
        }

        let frame = Arc::new(ItemFrameEntity::new_attached(
            self.entity_type,
            next_entity_id(),
            pos,
            clicked_face,
            Arc::downgrade(world),
        ));
        let entity: SharedEntity = Arc::clone(&frame) as SharedEntity;
        if apply_item_stack_components(&entity, &stack, player.is_operator()).is_err() {
            return InteractionResult::Fail;
        }
        if !frame.survives() {
            return InteractionResult::Consume;
        }

        frame.play_sound(frame.place_sound(), 1.0, 1.0);
        world.game_event_at(
            &vanilla_game_events::ENTITY_PLACE,
            frame.position(),
            &GameEventContext::new(Some(player.as_ref() as &dyn Entity), None),
        );
        if add_spawned_entity(world, entity).is_err() {
            return InteractionResult::Fail;
        }
        context.inv.with_item(|item| item.shrink(1));
        InteractionResult::Success
    }
}
