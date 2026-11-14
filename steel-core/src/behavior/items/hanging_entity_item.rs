//! Vanilla `HangingEntityItem` for paintings: hang a painting on the clicked wall.

use std::sync::Arc;

use steel_macros::item_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::{sound_events, vanilla_entities, vanilla_game_events};

use crate::behavior::{InteractionResult, ItemBehavior, UseOnContext};
use crate::entity::block_attached_entity::BlockAttachedEntity as _;
use crate::entity::entities::PaintingEntity;
use crate::entity::{
    Entity, SharedEntity, add_spawned_entity, apply_item_stack_components, next_entity_id,
};
use crate::world::game_event::GameEventContext;

/// Vanilla `HangingEntityItem`. Item frames use [`super::ItemFrameItem`].
#[item_behavior]
pub struct HangingEntityItem {
    #[json_arg(vanilla_entities, json = "type")]
    entity_type: EntityTypeRef,
}

impl HangingEntityItem {
    /// Creates the behavior for the item that hangs `entity_type`.
    #[must_use]
    pub const fn new(entity_type: EntityTypeRef) -> Self {
        Self { entity_type }
    }
}

impl ItemBehavior for HangingEntityItem {
    /// Vanilla `HangingEntityItem.useOn`.
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        let world = context.world;
        let player = context.player;
        let clicked_face = context.hit_result.direction;
        let pos = context.hit_result.block_pos.relative(clicked_face);
        let stack = context.inv.with_item(|item| item.clone());
        // Vanilla `HangingEntityItem.mayPlace`.
        if !clicked_face.is_horizontal() || !player.may_use_item_at(pos, clicked_face, &stack) {
            return InteractionResult::Fail;
        }
        if self.entity_type != &vanilla_entities::PAINTING {
            return InteractionResult::Success;
        }

        let Some(painting) = PaintingEntity::create(world, next_entity_id(), pos, clicked_face)
        else {
            return InteractionResult::Consume;
        };
        let painting = Arc::new(painting);
        let entity: SharedEntity = Arc::clone(&painting) as SharedEntity;
        if apply_item_stack_components(&entity, &stack, player.is_operator()).is_err() {
            return InteractionResult::Fail;
        }
        if !painting.survives() {
            return InteractionResult::Consume;
        }

        // Vanilla `Painting.playPlacementSound`.
        painting.play_sound(&sound_events::ENTITY_PAINTING_PLACE, 1.0, 1.0);
        world.game_event_at(
            &vanilla_game_events::ENTITY_PLACE,
            painting.position(),
            &GameEventContext::new(Some(player.as_ref() as &dyn Entity), None),
        );
        if add_spawned_entity(world, entity).is_err() {
            return InteractionResult::Fail;
        }
        context.inv.with_item(|item| item.shrink(1));
        InteractionResult::Success
    }
}
