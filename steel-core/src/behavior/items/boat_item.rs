//! Boat item behavior: puts a boat or raft down on the block or water the
//! player looks at.

use std::sync::Arc;

use steel_macros::item_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::vanilla_game_events;

use crate::behavior::item_utils::get_player_pov_hit_result;
use crate::behavior::{InteractionResult, ItemBehavior, UseItemContext};
use crate::entity::physics_state_for_move;
use crate::entity::{
    Entity, add_spawned_entity, apply_item_stack_components, create_entity_instance,
};
use crate::physics::{CollisionWorld as _, WorldCollisionProvider};
use crate::world::ClipFluid;
use crate::world::game_event::GameEventContext;

/// How far ahead of the player entities can block placing a boat
/// (vanilla `BoatItem.use`'s `range`).
const RANGE: f64 = 5.0;

/// Vanilla `BoatItem`, shared by every boat and raft item.
#[item_behavior]
pub struct BoatItem {
    #[json_arg(vanilla_entities, json = "entity_type")]
    entity_type: EntityTypeRef,
}

impl BoatItem {
    /// Creates the behavior for the item that places `entity_type`.
    #[must_use]
    pub const fn new(entity_type: EntityTypeRef) -> Self {
        Self { entity_type }
    }
}

impl ItemBehavior for BoatItem {
    /// Vanilla `BoatItem.use`.
    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let world = context.world;
        let player = context.player;
        let hit_result = get_player_pov_hit_result(world, player, ClipFluid::Any);
        if hit_result.miss {
            return InteractionResult::Pass;
        }

        // Vanilla only places a boat when no pickable entity surrounds the eyes.
        let view_vector = player.look_angle();
        let search_box = player
            .bounding_box()
            .expand_towards(view_vector * RANGE)
            .inflate(1.0);
        let player_id = player.id();
        let from = player.position().with_y(player.get_eye_y());
        let blocked = world
            .get_entities_in_aabb_matching(&search_box, |entity| {
                entity.id() != player_id && !entity.is_spectator() && entity.is_pickable()
            })
            .iter()
            .any(|entity| entity.bounding_box().contains(from));
        if blocked {
            return InteractionResult::Pass;
        }

        // Vanilla `BoatItem.getBoat`.
        let Ok(boat) = create_entity_instance(world, self.entity_type, hit_result.location) else {
            return InteractionResult::Fail;
        };
        boat.set_old_position_to_current();
        let stack = context.inv.with_item(|item| item.clone());
        if apply_item_stack_components(&boat, &stack, player.is_operator()).is_err() {
            return InteractionResult::Fail;
        }
        let (_, x_rot) = boat.rotation();
        boat.set_rotation((player.rotation().0, x_rot));

        let collision_world = WorldCollisionProvider::for_entity(world, boat.as_ref());
        if collision_world.has_collision_with_context(
            &boat.bounding_box(),
            physics_state_for_move(boat.as_ref()).block_collision_context(),
        ) {
            return InteractionResult::Fail;
        }
        if add_spawned_entity(world, Arc::clone(&boat)).is_err() {
            return InteractionResult::Fail;
        }
        world.game_event_at(
            &vanilla_game_events::ENTITY_PLACE,
            hit_result.location,
            &GameEventContext::new(Some(player.as_ref() as &dyn Entity), None),
        );
        let has_infinite_materials = player.has_infinite_materials();
        context.inv.with_item(|item| {
            player.award_stat(&vanilla_stat_types::ITEM_USED, item.item());
            item.consume(1, has_infinite_materials);
        });
        InteractionResult::Success
    }
}
