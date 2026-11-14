//! Vanilla `ShelfBlock`: three item slots on the front face; powered wooden
//! shelves chain up to three wide and swap their items with the hotbar.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, BoolProperty, Direction, EnumProperty, SideChainPart,
};
use steel_registry::data_components::vanilla_components::USE_EFFECTS;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{sound_events, vanilla_block_entity_types, vanilla_game_events};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use super::selectable_slot::hit_slot;
use crate::behavior::InventoryAccess;
use crate::behavior::block::schedule_water_tick_if_waterlogged;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::entities::{SHELF_SLOTS, ShelfBlockEntity};
use crate::block_entity::{BLOCK_ENTITIES, SharedBlockEntity};
use crate::entity::ai::path::PathComputationType;
use crate::inventory::container::Container;
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{ScheduledTickAccess, SignalGetter as _, World};

const FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;
const POWERED: &BoolProperty = &BlockStateProperties::POWERED;
const SIDE_CHAIN_PART: &EnumProperty<SideChainPart> = &BlockStateProperties::SIDE_CHAIN_PART;
const WATERLOGGED: &BoolProperty = &BlockStateProperties::WATERLOGGED;

/// Vanilla `getRows`.
const ROWS: u32 = 1;
/// Vanilla `getColumns`.
const COLUMNS: u32 = 3;
/// Vanilla `getMaxChainLength`.
const MAX_CHAIN_LENGTH: usize = 3;
/// Hotbar size, the slots a powered chain swaps with.
const HOTBAR_SLOTS: usize = 9;

/// Shelf block behavior.
#[block_behavior]
pub struct ShelfBlock {
    block: BlockRef,
}

impl ShelfBlock {
    /// Creates shelf behavior for `block`.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn shelf_at(world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|entity| entity.downcast_ref::<ShelfBlockEntity>().is_some())
    }

    fn play_sound(world: &World, pos: BlockPos, sound: SoundEventRef) {
        world.play_sound(sound, SoundSource::Blocks, pos, 1.0, 1.0, None);
    }

    /// Vanilla `swapSingleItem`: the held stack goes on the shelf and the
    /// shelf's item comes back. Returns `(held_before, removed)`.
    fn swap_single_item(
        player: &Player,
        shelf: &ShelfBlockEntity,
        slot: usize,
        inv: &InventoryAccess,
    ) -> (ItemStack, ItemStack) {
        let held = inv.with_item(|stack| stack.clone());
        let removed = shelf.swap_item_no_update(slot, held.clone());
        let new_inventory_item = if player.has_infinite_materials() && removed.is_empty() {
            held.clone()
        } else {
            removed.clone()
        };
        let event = if new_inventory_item
            .get(USE_EFFECTS)
            .is_some_and(|effects| !effects.interact_vibrations)
        {
            None
        } else {
            Some(&vanilla_game_events::ITEM_INTERACT_FINISH)
        };
        inv.with_item(|stack| *stack = new_inventory_item);
        shelf.set_changed_with(event);
        (held, removed)
    }

    /// Vanilla `swapHotbar`: each powered shelf in the chain swaps its three
    /// slots with the matching hotbar slots, the chain filling the hotbar
    /// from the right.
    fn swap_hotbar(world: &Arc<World>, pos: BlockPos, inv: &InventoryAccess) -> bool {
        let connected = all_blocks_connected_to(world, pos);
        let mut any_swapped = false;
        for (part_index, part_pos) in connected.iter().enumerate() {
            let Some(block_entity) = world.get_block_entity(*part_pos) else {
                continue;
            };
            let Some(shelf) = block_entity.downcast_ref::<ShelfBlockEntity>() else {
                continue;
            };
            for slot in 0..SHELF_SLOTS {
                // Vanilla `9 - (chain length - part index) * 3 + slot`, skipped
                // when negative.
                let Some(inventory_slot) = (HOTBAR_SLOTS + part_index * SHELF_SLOTS + slot)
                    .checked_sub(connected.len() * SHELF_SLOTS)
                else {
                    continue;
                };
                inv.with_inventory(|inventory| {
                    let placed = inventory.remove_item_no_update(inventory_slot);
                    let removed = shelf.swap_item_no_update(slot, placed.clone());
                    if !placed.is_empty() || !removed.is_empty() {
                        inventory.set_item(inventory_slot, removed);
                        any_swapped = true;
                    }
                });
            }
            inv.with_inventory(Container::set_changed);
            shelf.set_changed_with(Some(&vanilla_game_events::ENTITY_INTERACT));
        }
        any_swapped
    }
}

/// Vanilla `isConnectable`: a powered wooden shelf.
fn is_connectable(state: BlockStateId) -> bool {
    state.get_block().has_tag(&BlockTag::WOODEN_SHELVES)
        && state.try_get_value(POWERED).unwrap_or(false)
}

/// A shelf next to a chain, as Vanilla `SideChainPartBlock.Neighbor` sees it
/// when the neighbor is first looked at.
#[derive(Clone, Copy)]
struct Neighbor {
    pos: BlockPos,
    /// `None` for Vanilla's `EmptyNeighbor`.
    part: Option<SideChainPart>,
}

impl Neighbor {
    const fn is_connectable(self) -> bool {
        self.part.is_some()
    }

    fn is_unconnectable_or_chain_end(self) -> bool {
        self.part.is_none_or(|part| part != SideChainPart::Center)
    }

    fn connects_towards(self, end_part: SideChainPart) -> bool {
        self.part
            .is_some_and(|part| part == SideChainPart::Center || part == end_part)
    }

    fn update(self, world: &Arc<World>, change: fn(SideChainPart) -> SideChainPart) {
        if let Some(part) = self.part {
            set_part(world, self.pos, change(part));
        }
    }
}

/// Vanilla `SideChainPartBlock.Neighbors`, with its per-call cache.
struct Neighbors<'a> {
    world: &'a Arc<World>,
    facing: Direction,
    center: BlockPos,
    cache: Vec<Neighbor>,
}

impl<'a> Neighbors<'a> {
    const fn new(world: &'a Arc<World>, center: BlockPos, facing: Direction) -> Self {
        Self {
            world,
            facing,
            center,
            cache: Vec::new(),
        }
    }

    fn get(&mut self, direction: Direction, steps: i32) -> Neighbor {
        let pos = self.center.relative_n(direction, steps);
        if let Some(cached) = self.cache.iter().find(|neighbor| neighbor.pos == pos) {
            return *cached;
        }
        let state = self.world.get_block_state(pos);
        let part = (is_connectable(state) && state.get_value(FACING) == self.facing)
            .then(|| state.get_value(SIDE_CHAIN_PART));
        let neighbor = Neighbor { pos, part };
        self.cache.push(neighbor);
        neighbor
    }

    fn left(&mut self, steps: i32) -> Neighbor {
        self.get(self.facing.rotate_y_clockwise(), steps)
    }

    fn right(&mut self, steps: i32) -> Neighbor {
        self.get(self.facing.rotate_y_counter_clockwise(), steps)
    }
}

/// Vanilla `SideChainPart.whenConnectedToTheRight`.
const fn when_connected_to_the_right(part: SideChainPart) -> SideChainPart {
    match part {
        SideChainPart::Unconnected | SideChainPart::Left => SideChainPart::Left,
        SideChainPart::Right | SideChainPart::Center => SideChainPart::Center,
    }
}

/// Vanilla `SideChainPart.whenConnectedToTheLeft`.
const fn when_connected_to_the_left(part: SideChainPart) -> SideChainPart {
    match part {
        SideChainPart::Unconnected | SideChainPart::Right => SideChainPart::Right,
        SideChainPart::Center | SideChainPart::Left => SideChainPart::Center,
    }
}

/// Vanilla `SideChainPart.whenDisconnectedFromTheRight`.
const fn when_disconnected_from_the_right(part: SideChainPart) -> SideChainPart {
    match part {
        SideChainPart::Unconnected | SideChainPart::Left => SideChainPart::Unconnected,
        SideChainPart::Right | SideChainPart::Center => SideChainPart::Right,
    }
}

/// Vanilla `SideChainPart.whenDisconnectedFromTheLeft`.
const fn when_disconnected_from_the_left(part: SideChainPart) -> SideChainPart {
    match part {
        SideChainPart::Unconnected | SideChainPart::Right => SideChainPart::Unconnected,
        SideChainPart::Center | SideChainPart::Left => SideChainPart::Left,
    }
}

/// Vanilla `setPart`.
fn set_part(world: &Arc<World>, pos: BlockPos, part: SideChainPart) {
    let state = world.get_block_state(pos);
    if state.get_value(SIDE_CHAIN_PART) != part {
        world.set_block(
            pos,
            state.set_value(SIDE_CHAIN_PART, part),
            UpdateFlags::UPDATE_ALL,
        );
    }
}

/// Vanilla `getAllBlocksConnectedTo`: the chain through `pos`, left to right.
fn all_blocks_connected_to(world: &Arc<World>, pos: BlockPos) -> Vec<BlockPos> {
    let state = world.get_block_state(pos);
    if !is_connectable(state) {
        return Vec::new();
    }
    let mut neighbors = Neighbors::new(world, pos, state.get_value(FACING));
    let mut left = Vec::new();
    for steps in 1..MAX_CHAIN_LENGTH as i32 {
        let neighbor = neighbors.left(steps);
        if neighbor.connects_towards(SideChainPart::Left) {
            left.push(neighbor.pos);
        }
        if neighbor.is_unconnectable_or_chain_end() {
            break;
        }
    }
    let mut results: Vec<BlockPos> = left.into_iter().rev().collect();
    results.push(pos);
    for steps in 1..MAX_CHAIN_LENGTH as i32 {
        let neighbor = neighbors.right(steps);
        if neighbor.connects_towards(SideChainPart::Right) {
            results.push(neighbor.pos);
        }
        if neighbor.is_unconnectable_or_chain_end() {
            break;
        }
    }
    results
}

/// Vanilla `updateNeighborsAfterPoweringDown`.
fn update_neighbors_after_powering_down(world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
    let mut neighbors = Neighbors::new(world, pos, state.get_value(FACING));
    neighbors
        .left(1)
        .update(world, when_disconnected_from_the_right);
    neighbors
        .right(1)
        .update(world, when_disconnected_from_the_left);
}

/// Vanilla `canConnect`.
const fn can_connect(new_blocks: usize, current_chain_length: usize) -> bool {
    new_blocks > 0 && current_chain_length + new_blocks <= MAX_CHAIN_LENGTH
}

/// Vanilla `updateSelfAndNeighborsOnPoweringUp`.
fn update_self_and_neighbors_on_powering_up(
    world: &Arc<World>,
    pos: BlockPos,
    state: BlockStateId,
    old_state: BlockStateId,
) {
    if !is_connectable(state) {
        return;
    }
    // Vanilla `isBeingUpdatedByNeighbor`.
    let getting_connected = state.get_value(SIDE_CHAIN_PART) != SideChainPart::Unconnected;
    let connected_before = is_connectable(old_state)
        && old_state.get_value(SIDE_CHAIN_PART) != SideChainPart::Unconnected;
    if getting_connected || connected_before {
        return;
    }

    let mut neighbors = Neighbors::new(world, pos, state.get_value(FACING));
    let left = neighbors.left(1);
    let right = neighbors.right(1);
    let chain_on_the_left = if left.is_connectable() {
        all_blocks_connected_to(world, left.pos).len()
    } else {
        0
    };
    let chain_on_the_right = if right.is_connectable() {
        all_blocks_connected_to(world, right.pos).len()
    } else {
        0
    };
    let mut part = SideChainPart::Unconnected;
    let mut chain_length = 1;
    if can_connect(chain_on_the_left, chain_length) {
        part = when_connected_to_the_left(part);
        left.update(world, when_connected_to_the_right);
        chain_length += chain_on_the_left;
    }
    if can_connect(chain_on_the_right, chain_length) {
        part = when_connected_to_the_right(part);
        right.update(world, when_connected_to_the_left);
    }
    set_part(world, pos, part);
}

impl BlockBehavior for ShelfBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(
            self.block
                .default_state()
                .set_value(FACING, context.horizontal_direction().opposite())
                .set_value(
                    POWERED,
                    context.world.has_neighbor_signal(context.place_pos()),
                )
                .set_value(WATERLOGGED, context.is_water_source()),
        )
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        schedule_water_tick_if_waterlogged(state, world, pos);
        state
    }

    fn on_place(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        old_state: BlockStateId,
        _moved_by_piston: bool,
    ) {
        if state.get_value(POWERED) {
            update_self_and_neighbors_on_powering_up(world, pos, state, old_state);
        } else {
            update_neighbors_after_powering_down(world, pos, state);
        }
    }

    fn handle_neighbor_changed(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _source_block: BlockRef,
        _moved_by_piston: bool,
    ) {
        let signal = world.has_neighbor_signal(pos);
        if state.get_value(POWERED) == signal {
            return;
        }
        let mut new_state = state.set_value(POWERED, signal);
        if !signal {
            new_state = new_state.set_value(SIDE_CHAIN_PART, SideChainPart::Unconnected);
        }
        world.set_block(pos, new_state, UpdateFlags::UPDATE_ALL);
        Self::play_sound(
            world,
            pos,
            if signal {
                &sound_events::BLOCK_SHELF_ACTIVATE
            } else {
                &sound_events::BLOCK_SHELF_DEACTIVATE
            },
        );
        world.game_event(
            if signal {
                &vanilla_game_events::BLOCK_ACTIVATE
            } else {
                &vanilla_game_events::BLOCK_DEACTIVATE
            },
            pos,
            &GameEventContext::new(None, Some(new_state)),
        );
    }

    fn use_item_on(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        hand: InteractionHand,
        hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        if hand == InteractionHand::OffHand {
            return InteractionResult::Pass;
        }
        let Some(block_entity) = Self::shelf_at(world, pos) else {
            return InteractionResult::Pass;
        };
        let Some(shelf_entity) = block_entity.downcast_ref::<ShelfBlockEntity>() else {
            return InteractionResult::Pass;
        };
        let Some(slot) = hit_slot(hit_result, state.get_value(FACING), ROWS, COLUMNS) else {
            return InteractionResult::Pass;
        };

        if !state.get_value(POWERED) {
            let (held, removed) = Self::swap_single_item(player, shelf_entity, slot, inv);
            if removed.is_empty() {
                if held.is_empty() {
                    return InteractionResult::Pass;
                }
                Self::play_sound(world, pos, &sound_events::BLOCK_SHELF_PLACE_ITEM);
            } else {
                Self::play_sound(
                    world,
                    pos,
                    if held.is_empty() {
                        &sound_events::BLOCK_SHELF_TAKE_ITEM
                    } else {
                        &sound_events::BLOCK_SHELF_SINGLE_SWAP
                    },
                );
            }
            return InteractionResult::SuccessServer;
        }

        if !Self::swap_hotbar(world, pos, inv) {
            return InteractionResult::Consume;
        }
        Self::play_sound(world, pos, &sound_events::BLOCK_SHELF_MULTI_SWAP);
        InteractionResult::SuccessServer
    }

    /// Vanilla: only through water.
    fn is_pathfindable(&self, state: BlockStateId, path_type: PathComputationType) -> bool {
        path_type == PathComputationType::Water && state.get_value(WATERLOGGED)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::SHELF,
            level,
            pos,
            state,
        ))
    }

    /// Vanilla `Containers`: comparators reread the shelf; then the chain lets
    /// go of this shelf.
    fn affect_neighbors_after_removal(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
        update_neighbors_after_powering_down(world, pos, state);
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    /// Vanilla: read from behind, one bit per occupied slot.
    fn get_analog_output_signal(
        &self,
        state: BlockStateId,
        world: &World,
        pos: BlockPos,
        direction: Direction,
    ) -> i32 {
        if direction != state.get_value(FACING).opposite() {
            return 0;
        }
        let Some(block_entity) = world.get_block_entity(pos) else {
            return 0;
        };
        let Some(shelf_entity) = block_entity.downcast_ref::<ShelfBlockEntity>() else {
            return 0;
        };
        (0..SHELF_SLOTS)
            .filter(|slot| !shelf_entity.item(*slot).is_empty())
            .map(|slot| 1 << slot)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::ChunkPos;

    use super::*;
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::block_entity::init_block_entities;
    use crate::entity::init_entities;
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
    };

    /// The chain's right end; the shelves face north, so the left is east.
    const RIGHT: BlockPos = BlockPos::new(8, 64, 8);
    const CENTER: BlockPos = BlockPos::new(9, 64, 8);
    const LEFT: BlockPos = BlockPos::new(10, 64, 8);

    fn arena(name: &'static str) -> TestWorld {
        init_vanilla_registry();
        init_behaviors();
        init_block_entities();
        init_entities();
        let fixture = fresh_test_world(name);
        insert_ready_full_chunk(&fixture.world, ChunkPos::from_block_pos(RIGHT));
        fixture
    }

    fn shelf_state(powered: bool) -> BlockStateId {
        vanilla_blocks::OAK_SHELF
            .default_state()
            .set_value(FACING, Direction::North)
            .set_value(POWERED, powered)
    }

    /// Places a shelf, with a redstone block under it when `powered`, as a
    /// player would: powered from the start.
    fn place(world: &Arc<World>, pos: BlockPos, powered: bool) {
        let below = if powered {
            vanilla_blocks::REDSTONE_BLOCK.default_state()
        } else {
            vanilla_blocks::STONE.default_state()
        };
        assert!(world.set_block(pos.below(), below, UpdateFlags::UPDATE_ALL));
        assert!(world.set_block(pos, shelf_state(powered), UpdateFlags::UPDATE_ALL));
    }

    fn part(world: &World, pos: BlockPos) -> SideChainPart {
        world.get_block_state(pos).get_value(SIDE_CHAIN_PART)
    }

    fn shelf(world: &World, pos: BlockPos) -> SharedBlockEntity {
        world.get_block_entity(pos).expect("a shelf block entity")
    }

    /// A click on the north face of the shelf at `pos`, in `column` (0 is
    /// the viewer's left).
    fn front_hit(pos: BlockPos, column: u32) -> BlockHitResult {
        let x = f64::from(pos.x()) + 1.0 - (f64::from(column) + 0.5) / 3.0;
        BlockHitResult {
            location: DVec3::new(x, f64::from(pos.y()) + 0.5, f64::from(pos.z())),
            direction: Direction::North,
            block_pos: pos,
            miss: false,
            inside: false,
            world_border_hit: false,
        }
    }

    fn powered_chain(world: &Arc<World>) {
        place(world, RIGHT, true);
        place(world, CENTER, true);
        place(world, LEFT, true);
    }

    #[test]
    fn three_powered_shelves_chain_left_to_right() {
        let fixture = arena("shelf_chain");
        let world = &fixture.world;
        powered_chain(world);

        assert_eq!(part(world, LEFT), SideChainPart::Left);
        assert_eq!(part(world, CENTER), SideChainPart::Center);
        assert_eq!(part(world, RIGHT), SideChainPart::Right);
        assert_eq!(all_blocks_connected_to(world, RIGHT), [LEFT, CENTER, RIGHT]);
    }

    #[test]
    fn a_fourth_shelf_starts_no_longer_chain() {
        let fixture = arena("shelf_chain_limit");
        let world = &fixture.world;
        powered_chain(world);
        let fourth = BlockPos::new(7, 64, 8);
        place(world, fourth, true);

        assert_eq!(part(world, fourth), SideChainPart::Unconnected);
        assert_eq!(
            all_blocks_connected_to(world, CENTER).len(),
            MAX_CHAIN_LENGTH
        );
    }

    #[test]
    fn unpowering_the_middle_breaks_the_chain() {
        let fixture = arena("shelf_chain_break");
        let world = &fixture.world;
        powered_chain(world);
        assert!(world.set_block(
            CENTER.below(),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));

        assert!(!world.get_block_state(CENTER).get_value(POWERED));

        assert_eq!(part(world, LEFT), SideChainPart::Unconnected);
        assert_eq!(part(world, RIGHT), SideChainPart::Unconnected);
    }

    #[test]
    fn a_powered_chain_swaps_with_the_whole_hotbar() {
        let fixture = arena("shelf_hotbar_swap");
        let world = &fixture.world;
        powered_chain(world);
        let player = TestPlayerBuilder::new(Arc::clone(world), "ShelfTester", 1).build();
        let hotbar = [
            &vanilla_items::APPLE,
            &vanilla_items::STICK,
            &vanilla_items::DIAMOND,
            &vanilla_items::EMERALD,
            &vanilla_items::COAL,
            &vanilla_items::BREAD,
            &vanilla_items::BONE,
            &vanilla_items::FEATHER,
            &vanilla_items::STRING,
        ];
        {
            let mut inventory = player.inventory.lock();
            for (slot, item) in hotbar.iter().enumerate() {
                inventory.set_item(slot, ItemStack::new(item));
            }
        }

        let state = world.get_block_state(CENTER);
        let mut inv =
            InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
        let result = BLOCK_BEHAVIORS.get_behavior(state.get_block()).use_item_on(
            state,
            world,
            CENTER,
            &player,
            InteractionHand::MainHand,
            &front_hit(CENTER, 1),
            &mut inv,
        );

        assert_eq!(result, InteractionResult::SuccessServer);
        for (part_index, pos) in [LEFT, CENTER, RIGHT].into_iter().enumerate() {
            let block_entity = shelf(world, pos);
            let shelf = block_entity
                .downcast_ref::<ShelfBlockEntity>()
                .expect("a shelf");
            for slot in 0..SHELF_SLOTS {
                assert!(shelf.item(slot).is(hotbar[part_index * SHELF_SLOTS + slot]));
            }
        }
        let inventory = player.inventory.lock();
        assert!((0..HOTBAR_SLOTS).all(|slot| inventory.get_item(slot).is_empty()));
    }

    #[test]
    fn an_unpowered_shelf_swaps_the_clicked_slot_and_reads_on_a_comparator() {
        let fixture = arena("shelf_single_swap");
        let world = &fixture.world;
        place(world, RIGHT, false);
        let player = TestPlayerBuilder::new(Arc::clone(world), "ShelfTester", 1).build();
        player
            .inventory
            .lock()
            .set_item(0, ItemStack::with_count(&vanilla_items::APPLE, 5));

        let state = world.get_block_state(RIGHT);
        let behavior = BLOCK_BEHAVIORS.get_behavior(state.get_block());
        let mut inv =
            InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
        let result = behavior.use_item_on(
            state,
            world,
            RIGHT,
            &player,
            InteractionHand::MainHand,
            &front_hit(RIGHT, 2),
            &mut inv,
        );

        assert_eq!(result, InteractionResult::SuccessServer);
        let block_entity = shelf(world, RIGHT);
        let placed = block_entity
            .downcast_ref::<ShelfBlockEntity>()
            .expect("a shelf")
            .item(2);
        assert!(placed.is(&vanilla_items::APPLE));
        assert_eq!(placed.count(), 5);
        assert!(player.inventory.lock().get_item(0).is_empty());
        assert_eq!(
            behavior.get_analog_output_signal(state, world, RIGHT, Direction::South),
            0b100
        );
        assert_eq!(
            behavior.get_analog_output_signal(state, world, RIGHT, Direction::North),
            0
        );
    }
}
