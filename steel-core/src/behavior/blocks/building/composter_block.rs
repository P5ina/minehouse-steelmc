//! Vanilla `ComposterBlock`: compostable items add layers, a full composter
//! turns ready after a second and gives bone meal.

use std::sync::Arc;

use glam::DVec3;
use rand::RngExt as _;
use steel_macros::block_behavior;
use steel_registry::blocks::{
    BlockRef,
    block_state_ext::BlockStateExt as _,
    properties::{BlockStateProperties, Direction, IntProperty},
};
use steel_registry::data_components::components::Compostable;
use steel_registry::data_components::vanilla_components::COMPOSTABLE;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::item::BlockHitResult;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::{
    level_events, sound_events, vanilla_context_int_providers, vanilla_game_events, vanilla_items,
};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId};

use crate::{
    behavior::{BlockBehavior, BlockPlaceContext, InteractionResult, InventoryAccess},
    entity::Entity,
    entity::ai::path::PathComputationType,
    player::Player,
    world::World,
    world::game_event::GameEventContext,
};

/// Vanilla composter behavior.
#[block_behavior]
pub struct ComposterBlock {
    block: BlockRef,
}

const LEVEL_COMPOSTER: &IntProperty = &BlockStateProperties::LEVEL_COMPOSTER;
/// Vanilla `ComposterBlock.READY`.
const READY: u8 = 8;
/// Vanilla `ComposterBlock.MAX_LEVEL`.
const MAX_LEVEL: u8 = 7;
/// Vanilla: ticks until a full composter turns ready.
const READY_DELAY: i32 = 20;

fn composter_level(state: BlockStateId) -> u8 {
    state.get_value(LEVEL_COMPOSTER)
}

impl ComposterBlock {
    /// Vanilla `ComposterBlock.READY`: the level of a composter holding bone meal.
    pub const READY: u8 = READY;
    /// Vanilla `ComposterBlock.MAX_LEVEL`: the highest level items fill.
    pub const MAX_LEVEL: u8 = MAX_LEVEL;

    /// Creates composter behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    /// The fill level of a composter state.
    #[must_use]
    pub fn level(state: BlockStateId) -> u8 {
        composter_level(state)
    }

    /// Vanilla `ComposterBlock.insertItem`: composts one of `item_stack` into
    /// a composter that is not yet full, and returns the new state.
    pub fn insert_item(
        source: &dyn Entity,
        state: BlockStateId,
        world: &Arc<World>,
        item_stack: &mut ItemStack,
        pos: BlockPos,
    ) -> BlockStateId {
        insert_item(source, state, world, item_stack, pos)
    }

    /// Vanilla `ComposterBlock.extractProduce`: drops a bone meal above the
    /// composter and empties it.
    pub fn extract_produce(
        source: &dyn Entity,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
    ) -> BlockStateId {
        extract_produce(source, state, world, pos)
    }
}

/// Vanilla `ComposterBlock.insertItem`.
fn insert_item(
    source: &dyn Entity,
    state: BlockStateId,
    world: &Arc<World>,
    item_stack: &mut ItemStack,
    pos: BlockPos,
) -> BlockStateId {
    let Some(compostable) = item_stack.get(COMPOSTABLE).cloned() else {
        return state;
    };
    if composter_level(state) >= MAX_LEVEL {
        return state;
    }
    let new_state = add_layer(Some(source), state, world, pos, &compostable);
    item_stack.shrink(1);
    new_state
}

/// Vanilla `ComposterBlock.extractProduce`.
fn extract_produce(
    source: &dyn Entity,
    state: BlockStateId,
    world: &Arc<World>,
    pos: BlockPos,
) -> BlockStateId {
    let mut rng = rand::rng();
    // Vanilla `Vec3.atLowerCornerWithOffset(pos, 0.5, 1.01, 0.5).offsetRandomXZ(random, 0.7F)`.
    let offset_x = f64::from((rng.random::<f32>() - 0.5) * 0.7);
    let offset_z = f64::from((rng.random::<f32>() - 0.5) * 0.7);
    let item_pos = DVec3::new(
        f64::from(pos.x()) + 0.5 + offset_x,
        f64::from(pos.y()) + 1.01,
        f64::from(pos.z()) + 0.5 + offset_z,
    );
    if let Some(item) = world.spawn_item(item_pos, ItemStack::new(&vanilla_items::BONE_MEAL)) {
        item.set_default_pickup_delay();
    }
    let empty_state = empty(Some(source), state, world, pos);
    world.play_block_sound(&sound_events::BLOCK_COMPOSTER_EMPTY, pos, 1.0, 1.0, None);
    empty_state
}

/// Vanilla `ComposterBlock.empty`.
fn empty(
    source: Option<&dyn Entity>,
    state: BlockStateId,
    world: &Arc<World>,
    pos: BlockPos,
) -> BlockStateId {
    let new_state = state.set_value(LEVEL_COMPOSTER, 0);
    world.set_block(pos, new_state, UpdateFlags::UPDATE_ALL);
    world.game_event(
        &vanilla_game_events::BLOCK_CHANGE,
        pos,
        &GameEventContext::new(source, Some(new_state)),
    );
    new_state
}

/// Vanilla `ComposterBlock.addLayer`: rolls the item's compost chance and
/// raises the level by what it adds; a full composter turns ready later.
fn add_layer(
    source: Option<&dyn Entity>,
    state: BlockStateId,
    world: &Arc<World>,
    pos: BlockPos,
    compostable: &Compostable,
) -> BlockStateId {
    let level = composter_level(state);
    let layers_to_add = vanilla_context_int_providers::get(&compostable.layers)
        .map_or(0, |provider| {
            provider.get_int(Some(state), &mut rand::rng())
        });
    if layers_to_add <= 0 {
        return state;
    }
    let new_level = (i32::from(level) + layers_to_add).clamp(0, i32::from(MAX_LEVEL));
    // The clamp keeps the level in `0..=7`.
    let new_level = u8::try_from(new_level).unwrap_or(MAX_LEVEL);
    let new_state = state.set_value(LEVEL_COMPOSTER, new_level);
    world.set_block(pos, new_state, UpdateFlags::UPDATE_ALL);
    world.game_event(
        &vanilla_game_events::BLOCK_CHANGE,
        pos,
        &GameEventContext::new(source, Some(new_state)),
    );
    if new_level == MAX_LEVEL {
        world.schedule_block_tick_default(pos, state.get_block(), READY_DELAY);
    }
    new_state
}

impl BlockBehavior for ComposterBlock {
    fn get_state_for_placement(&self, _context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state())
    }

    fn on_place(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        if composter_level(state) == MAX_LEVEL {
            world.schedule_block_tick_default(pos, state.get_block(), READY_DELAY);
        }
    }

    fn use_item_on(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let level = composter_level(state);
        let Some((compostable, item)) =
            inv.with_item(|stack| stack.get(COMPOSTABLE).cloned().map(|c| (c, stack.item())))
        else {
            return InteractionResult::TryEmptyHandInteraction;
        };
        if level >= READY {
            return InteractionResult::TryEmptyHandInteraction;
        }
        if level < MAX_LEVEL {
            let new_state = add_layer(Some(player), state, world, pos, &compostable);
            world.level_event(
                level_events::COMPOSTER_FILL,
                pos,
                i32::from(new_state != state),
                None,
            );
            player.award_stat(&vanilla_stat_types::ITEM_USED, item);
            let has_infinite_materials = player.has_infinite_materials();
            inv.with_item(|stack| stack.consume(1, has_infinite_materials));
        }
        InteractionResult::Success
    }

    fn use_without_item(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if composter_level(state) == READY {
            let _empty = extract_produce(player, state, world, pos);
            InteractionResult::Success
        } else {
            InteractionResult::Pass
        }
    }

    fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        if composter_level(state) == MAX_LEVEL {
            world.set_block(
                pos,
                state.set_value(LEVEL_COMPOSTER, READY),
                UpdateFlags::UPDATE_ALL,
            );
            world.play_block_sound(&sound_events::BLOCK_COMPOSTER_READY, pos, 1.0, 1.0, None);
        }
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        state: BlockStateId,
        _world: &World,
        _pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        i32::from(composter_level(state))
    }

    fn is_pathfindable(
        &self,
        _state: BlockStateId,
        _computation_type: PathComputationType,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
