//! Minecart item behavior: places a cart on a rail.

use std::sync::Arc;

use glam::DVec3;
use steel_macros::item_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, RailShape};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_events;

use crate::behavior::blocks::is_rail_state;
use crate::behavior::{InteractionResult, ItemBehavior, UseOnContext};
use crate::entity::{
    Entity, add_spawned_entity, apply_item_stack_components, create_entity_instance,
};
use crate::world::game_event::GameEventContext;

/// Vanilla `MinecartItem`, shared by every cart item.
#[item_behavior]
pub struct MinecartItem {
    #[json_arg(vanilla_entities, json = "type")]
    entity_type: EntityTypeRef,
}

impl MinecartItem {
    /// Creates the behavior for the item that places `entity_type`.
    #[must_use]
    pub const fn new(entity_type: EntityTypeRef) -> Self {
        Self { entity_type }
    }
}

impl ItemBehavior for MinecartItem {
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        let world = context.world;
        let pos = context.hit_result.block_pos;
        let state = world.get_block_state(pos);
        if !state.get_block().has_tag(&BlockTag::RAILS) {
            return InteractionResult::Fail;
        }

        let shape = if is_rail_state(state) {
            state.get_value(&BlockStateProperties::RAIL_SHAPE)
        } else {
            RailShape::NorthSouth
        };
        let offset = if shape.is_slope() { 0.5 } else { 0.0 };
        let spawn_pos = DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.0625 + offset,
            f64::from(pos.z()) + 0.5,
        );

        let Ok(cart) = create_entity_instance(world, self.entity_type, spawn_pos) else {
            return InteractionResult::Fail;
        };
        cart.set_old_position_to_current();
        let stack = context.inv.with_item(|item| item.clone());
        if apply_item_stack_components(&cart, &stack, context.player.is_operator()).is_err() {
            return InteractionResult::Fail;
        }
        if add_spawned_entity(world, Arc::clone(&cart)).is_err() {
            return InteractionResult::Fail;
        }

        world.game_event(
            &vanilla_game_events::ENTITY_PLACE,
            pos,
            &GameEventContext::new(
                Some(context.player.as_ref() as &dyn Entity),
                Some(world.get_block_state(pos.below())),
            ),
        );
        context.inv.with_item(|item| item.shrink(1));
        InteractionResult::Success
    }
}
