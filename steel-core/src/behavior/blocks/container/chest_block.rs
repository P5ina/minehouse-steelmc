//! Chest, trapped chest and copper chest behavior.
//!
//! Two chests facing the same way side by side join into a double chest. Each half
//! keeps its own block entity; menus, comparators and hoppers see the halves as one
//! container, `RIGHT` half first. Copper chests of any oxidation join each other and
//! share the least oxidized look, Vanilla `CopperChestBlock`.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{BlockStateProperties, ChestType};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::stat::custom::CustomStatRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{
    REGISTRY, sound_events, vanilla_block_entity_types, vanilla_blocks, vanilla_custom_stats,
};
use steel_utils::{BlockPos, BlockStateId, Direction, Downcast as _, translations};
use text_components::TextComponent;

use super::anger_nearby_piglins;
use crate::behavior::InventoryAccess;
use crate::behavior::block::{
    BlockBehavior, BlockEntityCreation, schedule_water_tick_if_waterlogged,
};
use crate::behavior::blocks::{WeatherState, WeatheringCopper};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::behavior::waxables::get_normal_from_waxed_variant;
use crate::behavior::weathering::get_weather_state;
use crate::block_entity::entities::ChestBlockEntity;
use crate::block_entity::{BLOCK_ENTITIES, SharedBlockEntity};
use crate::inventory::container::calculate_redstone_signal_from_containers;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::kinds::chest_halves;
use crate::player::Player;
use crate::world::{
    LevelReader, ScheduledTickAccess, SignalQueryContext, World, is_redstone_conductor,
};

/// The block entities a chest resolves to.
pub enum ChestCombineResult {
    /// The chest is blocked or has no block entity.
    None,
    /// A chest without a usable partner.
    Single(SharedBlockEntity),
    /// Both halves of a double chest, `RIGHT` half first.
    Double(SharedBlockEntity, SharedBlockEntity),
}

impl ChestCombineResult {
    /// The containers in the order Vanilla's `CompoundContainer` addresses them.
    #[must_use]
    pub fn containers(&self) -> Vec<ContainerRef> {
        match self {
            Self::None => Vec::new(),
            Self::Single(chest) => chest.container_ref().into_iter().collect(),
            Self::Double(first, second) => match (first.container_ref(), second.container_ref()) {
                (Some(first), Some(second)) => vec![first, second],
                _ => Vec::new(),
            },
        }
    }
}

/// Shared chest logic for [`ChestBlock`], [`TrappedChestBlock`],
/// [`CopperChestBlock`] and [`WeatheringCopperChestBlock`].
struct ChestBehavior {
    block: BlockRef,
    block_entity_type: BlockEntityTypeRef,
    open_stat: CustomStatRef,
    /// Joins any copper chest instead of only its own block.
    copper: bool,
}

impl ChestBehavior {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> BlockStateId {
        let mut chest_type = ChestType::Single;
        let mut facing = context.horizontal_direction().opposite();
        let secondary_use = context.is_secondary_use_active();
        let clicked_face = context.clicked_face();
        let world = context.world;
        let pos = context.place_pos();

        if clicked_face.is_horizontal()
            && secondary_use
            && let Some(partner_facing) =
                self.candidate_partner_facing(world.as_ref(), pos, clicked_face.opposite())
            && partner_facing.axis() != clicked_face.axis()
        {
            facing = partner_facing;
            chest_type = if facing.rotate_y_counter_clockwise() == clicked_face.opposite() {
                ChestType::Right
            } else {
                ChestType::Left
            };
        }

        if chest_type == ChestType::Single && !secondary_use {
            chest_type = self.chest_type(world.as_ref(), pos, facing);
        }

        let state = self
            .block
            .default_state()
            .set_value(&BlockStateProperties::FACING, facing)
            .set_value(&BlockStateProperties::CHEST_TYPE, chest_type)
            .set_value(
                &BlockStateProperties::WATERLOGGED,
                context.is_water_source(),
            );
        if self.copper {
            least_oxidized_chest_of_connected_blocks(state, world.as_ref(), pos)
        } else {
            state
        }
    }

    fn chest_type(&self, world: &dyn LevelReader, pos: BlockPos, facing: Direction) -> ChestType {
        if self.candidate_partner_facing(world, pos, facing.rotate_y_clockwise()) == Some(facing) {
            ChestType::Left
        } else if self.candidate_partner_facing(world, pos, facing.rotate_y_counter_clockwise())
            == Some(facing)
        {
            ChestType::Right
        } else {
            ChestType::Single
        }
    }

    /// The facing of a single chest of this kind next to `pos`, if there is one.
    fn candidate_partner_facing(
        &self,
        world: &dyn LevelReader,
        pos: BlockPos,
        direction: Direction,
    ) -> Option<Direction> {
        let state = world.get_block_state(pos.relative(direction));
        (self.chest_can_connect_to(state)
            && state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Single)
            .then(|| state.get_value(&BlockStateProperties::FACING))
    }

    /// Vanilla `chestCanConnectTo`; `CopperChestBlock` accepts any block in
    /// `#copper_chests` that has a chest type.
    fn chest_can_connect_to(&self, state: BlockStateId) -> bool {
        if self.copper {
            state.get_block().has_tag(&BlockTag::COPPER_CHESTS)
                && state
                    .try_get_value(&BlockStateProperties::CHEST_TYPE)
                    .is_some()
        } else {
            state.get_block() == self.block
        }
    }

    /// Vanilla `ChestBlock.updateShape`, then for copper chests
    /// `CopperChestBlock.updateShape`: a joined half takes its partner's
    /// oxidation, so both halves weather and get waxed together.
    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        neighbor_state: BlockStateId,
    ) -> BlockStateId {
        let state = self.chest_update_shape(state, world, pos, direction, neighbor_state);
        if self.copper
            && self.chest_can_connect_to(neighbor_state)
            && state.get_value(&BlockStateProperties::CHEST_TYPE) != ChestType::Single
            && connected_direction(state) == direction
        {
            return REGISTRY
                .blocks
                .copy_matching_properties(state, neighbor_state.get_block());
        }
        state
    }

    fn chest_update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        direction: Direction,
        neighbor_state: BlockStateId,
    ) -> BlockStateId {
        schedule_water_tick_if_waterlogged(state, world, pos);
        if self.chest_can_connect_to(neighbor_state) && direction.is_horizontal() {
            let neighbor_type = neighbor_state.get_value(&BlockStateProperties::CHEST_TYPE);
            if state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Single
                && neighbor_type != ChestType::Single
                && state.get_value(&BlockStateProperties::FACING)
                    == neighbor_state.get_value(&BlockStateProperties::FACING)
                && connected_direction(neighbor_state) == direction.opposite()
            {
                let chest_type = match neighbor_type {
                    ChestType::Left => ChestType::Right,
                    ChestType::Right | ChestType::Single => ChestType::Left,
                };
                return state.set_value(&BlockStateProperties::CHEST_TYPE, chest_type);
            }
        } else if connected_direction(state) == direction {
            return state.set_value(&BlockStateProperties::CHEST_TYPE, ChestType::Single);
        }
        state
    }

    /// Resolves this chest and its partner. Vanilla `ChestBlock.combine`.
    fn combine(
        &self,
        state: BlockStateId,
        world: &World,
        pos: BlockPos,
        ignore_blocked: bool,
    ) -> ChestCombineResult {
        let is_blocked = |pos| !ignore_blocked && is_chest_blocked_at(world, pos);
        let Some(chest) = self.chest_block_entity(world, pos) else {
            return ChestCombineResult::None;
        };
        if is_blocked(pos) {
            return ChestCombineResult::None;
        }
        let chest_type = state.get_value(&BlockStateProperties::CHEST_TYPE);
        if chest_type == ChestType::Single {
            return ChestCombineResult::Single(chest);
        }

        let neighbor_pos = pos.relative(connected_direction(state));
        let neighbor_state = world.get_block_state(neighbor_pos);
        if neighbor_state.get_block() == state.get_block() {
            let neighbor_type = neighbor_state.get_value(&BlockStateProperties::CHEST_TYPE);
            if neighbor_type != ChestType::Single
                && neighbor_type != chest_type
                && neighbor_state.get_value(&BlockStateProperties::FACING)
                    == state.get_value(&BlockStateProperties::FACING)
            {
                if is_blocked(neighbor_pos) {
                    return ChestCombineResult::None;
                }
                if let Some(neighbor) = self.chest_block_entity(world, neighbor_pos) {
                    return if chest_type == ChestType::Right {
                        ChestCombineResult::Double(chest, neighbor)
                    } else {
                        ChestCombineResult::Double(neighbor, chest)
                    };
                }
            }
        }
        ChestCombineResult::Single(chest)
    }

    fn chest_block_entity(&self, world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|entity| entity.get_type() == self.block_entity_type)
    }

    /// Opens the chest menu (vanilla `getMenuProvider` + `openMenu`), returning
    /// whether the chest provided one. A blocked chest provides none.
    fn open_menu(
        &self,
        state: BlockStateId,
        world: &World,
        pos: BlockPos,
        player: &Player,
    ) -> bool {
        let combined = self.combine(state, world, pos, false);
        let (title, rows) = match &combined {
            ChestCombineResult::None => return false,
            ChestCombineResult::Single(chest) => (
                custom_name(chest).unwrap_or_else(|| {
                    TextComponent::translated(translations::CONTAINER_CHEST.msg())
                }),
                3,
            ),
            ChestCombineResult::Double(first, second) => (
                custom_name(first)
                    .or_else(|| custom_name(second))
                    .unwrap_or_else(|| {
                        TextComponent::translated(translations::CONTAINER_CHEST_DOUBLE.msg())
                    }),
                6,
            ),
        };
        let containers = combined.containers();
        if containers.is_empty() {
            return false;
        }

        let inventory = Arc::clone(&player.inventory);
        player.open_menu(title, move |context| {
            chest_halves(inventory, context.container_id, &containers, rows)
        });
        true
    }

    fn use_without_item(&self, state: BlockStateId, world: &World, pos: BlockPos, player: &Player) {
        if !self.open_menu(state, world, pos, player) {
            return;
        }
        player.award_custom_stat(self.open_stat);
        anger_nearby_piglins(world, player, true);
    }

    fn get_analog_output_signal(&self, state: BlockStateId, world: &World, pos: BlockPos) -> i32 {
        let containers = self.combine(state, world, pos, false).containers();
        if containers.is_empty() {
            return 0;
        }
        let guard = ContainerLockGuard::lock_all(&containers);
        let locked: Vec<_> = containers
            .iter()
            .filter_map(|container| guard.get(container.container_id()))
            .collect();
        calculate_redstone_signal_from_containers(&locked)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            self.block_entity_type,
            level,
            pos,
            state,
        ))
    }
}

/// The direction from a chest half to its partner.
#[must_use]
pub fn connected_direction(state: BlockStateId) -> Direction {
    let facing: Direction = state.get_value(&BlockStateProperties::FACING);
    if state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Left {
        facing.rotate_y_clockwise()
    } else {
        facing.rotate_y_counter_clockwise()
    }
}

/// Whether a chest at `pos` cannot be opened.
#[must_use]
pub fn is_chest_blocked_at(world: &World, pos: BlockPos) -> bool {
    let above = pos.above();
    // TODO: Cats sitting on the chest block it too (isCatSittingOnChest); Steel has no cats yet.
    is_redstone_conductor(world, world.get_block_state(above), above)
}

/// A copper chest block's oxidation and whether it is waxed, from Vanilla
/// `CopperChestBlock.getState` and `isWaxed`.
fn copper_chest_weather(block: BlockRef) -> Option<(WeatherState, bool)> {
    if !block.has_tag(&BlockTag::COPPER_CHESTS) {
        return None;
    }
    let unwaxed = get_normal_from_waxed_variant(block);
    let weather = get_weather_state(unwaxed.unwrap_or(block))?;
    Some((weather, unwaxed.is_some()))
}

/// The unwaxed copy of a copper chest state, Vanilla `CopperChestBlock.unwaxBlock`.
fn unwax(state: BlockStateId) -> BlockStateId {
    get_normal_from_waxed_variant(state.get_block()).map_or(state, |unwaxed| {
        REGISTRY.blocks.copy_matching_properties(state, unwaxed)
    })
}

/// Vanilla `CopperChestBlock.getLeastOxidizedChestOfConnectedBlocks`: a
/// copper chest placed against another takes the less oxidized of the two
/// looks, and loses its wax when only one of them is waxed.
fn least_oxidized_chest_of_connected_blocks(
    state: BlockStateId,
    world: &dyn LevelReader,
    pos: BlockPos,
) -> BlockStateId {
    if state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Single {
        return state;
    }
    let neighbor = world.get_block_state(pos.relative(connected_direction(state)));
    let (Some((own_weather, own_waxed)), Some((neighbor_weather, neighbor_waxed))) = (
        copper_chest_weather(state.get_block()),
        copper_chest_weather(neighbor.get_block()),
    ) else {
        return state;
    };
    let (own, partner) = if own_waxed == neighbor_waxed {
        (state, neighbor)
    } else {
        (unwax(state), unwax(neighbor))
    };
    let block = if own_weather <= neighbor_weather {
        own.get_block()
    } else {
        partner.get_block()
    };
    REGISTRY.blocks.copy_matching_properties(own, block)
}

/// The sounds a chest block opens and closes with, Vanilla
/// `ChestBlock.getOpenChestSound` / `getCloseChestSound`, or `None` for a
/// block that is not a chest. Copper chests sound by oxidation, Vanilla
/// `CopperChestBlock.getHingeSound`.
#[must_use]
pub fn chest_lid_sounds(block: BlockRef) -> Option<(SoundEventRef, SoundEventRef)> {
    if block == &vanilla_blocks::CHEST || block == &vanilla_blocks::TRAPPED_CHEST {
        return Some((
            &sound_events::BLOCK_CHEST_OPEN,
            &sound_events::BLOCK_CHEST_CLOSE,
        ));
    }
    let (weather, _) = copper_chest_weather(block)?;
    Some(match weather {
        WeatherState::Weathered => (
            &sound_events::BLOCK_COPPER_CHEST_WEATHERED_OPEN,
            &sound_events::BLOCK_COPPER_CHEST_WEATHERED_CLOSE,
        ),
        WeatherState::Oxidized => (
            &sound_events::BLOCK_COPPER_CHEST_OXIDIZED_OPEN,
            &sound_events::BLOCK_COPPER_CHEST_OXIDIZED_CLOSE,
        ),
        WeatherState::Unaffected | WeatherState::Exposed => (
            &sound_events::BLOCK_COPPER_CHEST_OPEN,
            &sound_events::BLOCK_COPPER_CHEST_CLOSE,
        ),
    })
}

fn custom_name(chest: &SharedBlockEntity) -> Option<TextComponent> {
    chest
        .downcast_ref::<ChestBlockEntity>()
        .and_then(ChestBlockEntity::custom_name)
}

#[block_behavior]
/// Chest block behavior.
pub struct ChestBlock {
    block: BlockRef,
}

#[block_behavior]
/// Trapped chest block behavior: a chest whose viewers power it.
pub struct TrappedChestBlock {
    block: BlockRef,
}

#[block_behavior]
/// Waxed copper chest behavior, Vanilla `CopperChestBlock`.
pub struct CopperChestBlock {
    block: BlockRef,
}

#[block_behavior]
/// Unwaxed copper chest behavior that oxidizes over time, Vanilla
/// `WeatheringCopperChestBlock`.
pub struct WeatheringCopperChestBlock {
    block: BlockRef,
}

macro_rules! impl_chest_block {
    ($name:ident, $entity_type:ident, $stat:ident, $trapped:expr, $copper:expr, $weathering:expr) => {
        impl $name {
            /// Creates the chest behavior for `block`.
            #[must_use]
            pub const fn new(block: BlockRef) -> Self {
                Self { block }
            }

            const fn chest(&self) -> ChestBehavior {
                ChestBehavior {
                    block: self.block,
                    block_entity_type: &vanilla_block_entity_types::$entity_type,
                    open_stat: &vanilla_custom_stats::$stat,
                    copper: $copper,
                }
            }

            /// Resolves this chest and its partner, Vanilla `ChestBlock.combine`.
            #[must_use]
            pub fn combine(
                &self,
                state: BlockStateId,
                world: &World,
                pos: BlockPos,
                ignore_blocked: bool,
            ) -> ChestCombineResult {
                self.chest().combine(state, world, pos, ignore_blocked)
            }
        }

        impl BlockBehavior for $name {
            fn get_state_for_placement(
                &self,
                context: &BlockPlaceContext<'_>,
            ) -> Option<BlockStateId> {
                Some(self.chest().get_state_for_placement(context))
            }

            fn update_shape(
                &self,
                state: BlockStateId,
                world: &dyn ScheduledTickAccess,
                pos: BlockPos,
                direction: Direction,
                _neighbor_pos: BlockPos,
                neighbor_state: BlockStateId,
            ) -> BlockStateId {
                self.chest()
                    .update_shape(state, world, pos, direction, neighbor_state)
            }

            fn open_menu_provider(
                &self,
                state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                player: &Player,
            ) -> bool {
                self.chest().open_menu(state, world, pos, player)
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
                self.chest().use_without_item(state, world, pos, player);
                InteractionResult::Success
            }

            fn affect_neighbors_after_removal(
                &self,
                _state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                _moved_by_piston: bool,
            ) {
                world.update_neighbor_for_output_signal(pos, self.chest().block);
            }

            fn new_block_entity(
                &self,
                level: Weak<World>,
                pos: BlockPos,
                state: BlockStateId,
            ) -> BlockEntityCreation {
                self.chest().new_block_entity(level, pos, state)
            }

            fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
                true
            }

            fn get_analog_output_signal(
                &self,
                state: BlockStateId,
                world: &World,
                pos: BlockPos,
                _direction: Direction,
            ) -> i32 {
                self.chest().get_analog_output_signal(state, world, pos)
            }

            /// Vanilla `ChestBlock.tick`: the viewer recheck scheduled while
            /// the chest is open.
            fn tick(&self, _state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
                if let Some(chest) = world.get_block_entity(pos)
                    && chest.downcast_ref::<ChestBlockEntity>().is_some()
                {
                    chest.recheck_open();
                }
            }

            /// Vanilla `WeatheringCopperChestBlock.randomTick`: the `LEFT` or
            /// single half oxidizes while nobody has it open; the `RIGHT` half
            /// follows through `update_shape`.
            fn random_tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
                if !$weathering
                    || state.get_value(&BlockStateProperties::CHEST_TYPE) == ChestType::Right
                {
                    return;
                }
                let Some(chest) = world.get_block_entity(pos) else {
                    return;
                };
                let Some(chest_entity) = chest.downcast_ref::<ChestBlockEntity>() else {
                    return;
                };
                if chest_entity.has_viewers(world) {
                    return;
                }
                if let Some(weather) = get_weather_state(self.block) {
                    WeatheringCopper::new(weather).change_over_time(state, world, pos);
                }
            }

            /// Vanilla `BaseEntityBlock.triggerEvent`: the block entity decides.
            fn trigger_event(
                &self,
                _state: BlockStateId,
                world: &Arc<World>,
                pos: BlockPos,
                param_a: i32,
                param_b: i32,
            ) -> bool {
                world
                    .get_block_entity(pos)
                    .is_some_and(|chest| chest.trigger_event(param_a, param_b))
            }

            fn is_signal_source(&self, _state: BlockStateId, _context: SignalQueryContext) -> bool {
                $trapped
            }

            /// Vanilla `TrappedChestBlock.ownSignal`: one per viewer of this
            /// half, at most 15.
            fn get_own_signal(
                &self,
                _state: BlockStateId,
                world: &dyn LevelReader,
                pos: BlockPos,
                _context: SignalQueryContext,
            ) -> i32 {
                if !$trapped {
                    return 0;
                }
                trapped_chest_signal(world, pos)
            }

            /// Vanilla `TrappedChestBlock.getDirectSignal`: strong power only
            /// into the block below.
            fn get_direct_signal(
                &self,
                state: BlockStateId,
                world: &dyn LevelReader,
                pos: BlockPos,
                direction: Direction,
                context: SignalQueryContext,
            ) -> i32 {
                if $trapped && direction == Direction::Up {
                    self.get_signal(state, world, pos, direction, context)
                } else {
                    0
                }
            }
        }
    };
}

/// A trapped chest half's power, Vanilla `ChestBlockEntity.getOpenCount`
/// clamped to 0..=15.
fn trapped_chest_signal(world: &dyn LevelReader, pos: BlockPos) -> i32 {
    world
        .get_block_entity(pos)
        .and_then(|chest| {
            chest
                .downcast_ref::<ChestBlockEntity>()
                .map(ChestBlockEntity::opener_count)
        })
        .unwrap_or(0)
        .clamp(0, 15)
}

impl_chest_block!(ChestBlock, CHEST, OPEN_CHEST, false, false, false);
impl_chest_block!(
    TrappedChestBlock,
    TRAPPED_CHEST,
    TRIGGER_TRAPPED_CHEST,
    true,
    false,
    false
);
impl_chest_block!(CopperChestBlock, CHEST, OPEN_CHEST, false, true, false);
impl_chest_block!(
    WeatheringCopperChestBlock,
    CHEST,
    OPEN_CHEST,
    false,
    true,
    true
);

/// The containers of the chest at `pos`, `RIGHT` half first, or `None` if the
/// block is not a chest. Vanilla `ChestBlock.getContainer`.
#[must_use]
pub fn chest_container(
    state: BlockStateId,
    world: &World,
    pos: BlockPos,
    ignore_blocked: bool,
) -> Option<Vec<ContainerRef>> {
    let block = state.get_block();
    let chest = if block == &vanilla_blocks::CHEST {
        ChestBlock::new(block).chest()
    } else if block == &vanilla_blocks::TRAPPED_CHEST {
        TrappedChestBlock::new(block).chest()
    } else if block.has_tag(&BlockTag::COPPER_CHESTS) {
        CopperChestBlock::new(block).chest()
    } else {
        return None;
    };
    Some(
        chest
            .combine(state, world, pos, ignore_blocked)
            .containers(),
    )
}

#[cfg(test)]
mod tests {
    use steel_registry::init_vanilla_registry;
    use steel_utils::ChunkPos;
    use steel_utils::types::UpdateFlags;

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::block_entity::init_block_entities;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    const POS: BlockPos = BlockPos::new(8, 64, 8);

    fn half(block: BlockRef, chest_type: ChestType) -> BlockStateId {
        block
            .default_state()
            .set_value(&BlockStateProperties::FACING, Direction::South)
            .set_value(&BlockStateProperties::CHEST_TYPE, chest_type)
    }

    /// The block a `LEFT` copper half of `placed` becomes next to a single
    /// `partner`, Vanilla `getLeastOxidizedChestOfConnectedBlocks`.
    fn placed_next_to(placed: BlockRef, partner: BlockRef) -> BlockRef {
        init_vanilla_registry();
        init_behaviors();
        init_block_entities();
        let fixture = fresh_test_world("copper_chest_placement");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::from_block_pos(POS));
        let state = half(placed, ChestType::Left);
        assert!(world.set_block(
            POS.relative(connected_direction(state)),
            half(partner, ChestType::Single),
            UpdateFlags::UPDATE_NONE,
        ));
        let result = least_oxidized_chest_of_connected_blocks(state, world.as_ref(), POS);
        assert_eq!(
            result.get_value(&BlockStateProperties::CHEST_TYPE),
            ChestType::Left
        );
        result.get_block()
    }

    #[test]
    fn placed_copper_chest_takes_the_least_oxidized_look() {
        assert_eq!(
            placed_next_to(
                &vanilla_blocks::OXIDIZED_COPPER_CHEST,
                &vanilla_blocks::EXPOSED_COPPER_CHEST
            ),
            &vanilla_blocks::EXPOSED_COPPER_CHEST
        );
        assert_eq!(
            placed_next_to(
                &vanilla_blocks::COPPER_CHEST,
                &vanilla_blocks::WEATHERED_COPPER_CHEST
            ),
            &vanilla_blocks::COPPER_CHEST
        );
    }

    #[test]
    fn placed_copper_chest_loses_wax_unless_both_halves_are_waxed() {
        assert_eq!(
            placed_next_to(
                &vanilla_blocks::WAXED_WEATHERED_COPPER_CHEST,
                &vanilla_blocks::OXIDIZED_COPPER_CHEST
            ),
            &vanilla_blocks::WEATHERED_COPPER_CHEST
        );
        assert_eq!(
            placed_next_to(
                &vanilla_blocks::WAXED_OXIDIZED_COPPER_CHEST,
                &vanilla_blocks::WAXED_EXPOSED_COPPER_CHEST
            ),
            &vanilla_blocks::WAXED_EXPOSED_COPPER_CHEST
        );
    }

    #[test]
    fn plain_chests_only_join_their_own_block() {
        init_vanilla_registry();
        let chest = ChestBlock::new(&vanilla_blocks::CHEST).chest();
        assert!(chest.chest_can_connect_to(vanilla_blocks::CHEST.default_state()));
        assert!(!chest.chest_can_connect_to(vanilla_blocks::COPPER_CHEST.default_state()));
        let copper = CopperChestBlock::new(&vanilla_blocks::WAXED_COPPER_CHEST).chest();
        assert!(copper.chest_can_connect_to(vanilla_blocks::OXIDIZED_COPPER_CHEST.default_state()));
        assert!(!copper.chest_can_connect_to(vanilla_blocks::CHEST.default_state()));
    }
}
