//! Beehive block behavior implementation.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{
    BlockStateProperties, Direction, EnumProperty, IntProperty,
};
use steel_registry::data_components::DataComponentPatch;
use steel_registry::data_components::components::BlockItemStateProperties;
use steel_registry::data_components::vanilla_components::BLOCK_STATE;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::item::BlockHitResult;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::vanilla_enchantment_tags::EnchantmentTag;
use steel_registry::vanilla_game_rules::BLOCK_DROPS;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, sound_events, vanilla_block_entity_types,
    vanilla_blocks, vanilla_entities, vanilla_game_events, vanilla_items, vanilla_loot_tables,
};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, Downcast as _, WorldAabb};

use crate::behavior::block::{
    BlockBehavior, BlockEntityCreation, drop_from_block_interact_loot_table,
};
use crate::behavior::blocks::CampfireBlock;
use crate::behavior::context::BlockPlaceContext;
use crate::behavior::{BlockLootContext, InteractionResult, InventoryAccess};
use crate::block_entity::entities::{BeeReleaseStatus, BeehiveBlockEntity};
use crate::block_entity::{BLOCK_ENTITIES, BlockEntityTicker, SharedBlockEntity};
use crate::entity::entities::BeeEntity;
use crate::entity::{Entity, Mob as _};
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::explosion::Explosion;
use crate::world::game_event::GameEventContext;
use crate::world::{ScheduledTickAccess, World};

/// Behavior for beehive and bee nest blocks.
#[block_behavior]
pub struct BeehiveBlock {
    block: BlockRef,
}

const HORIZONTAL_FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;
const LEVEL_HONEY: &IntProperty = &BlockStateProperties::LEVEL_HONEY;
/// Vanilla `BeehiveBlock.MAX_HONEY_LEVELS`.
pub const MAX_HONEY_LEVELS: u8 = 5;

impl BeehiveBlock {
    /// Creates a new beehive block behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn hive_at(world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|block_entity| block_entity.downcast_ref::<BeehiveBlockEntity>().is_some())
    }

    /// Vanilla `angerNearbyBees`: every idle bee around the hive picks a random
    /// nearby player.
    fn anger_nearby_bees(world: &World, pos: BlockPos) {
        let area = WorldAabb::new(
            f64::from(pos.x()) - 8.0,
            f64::from(pos.y()) - 6.0,
            f64::from(pos.z()) - 8.0,
            f64::from(pos.x()) + 9.0,
            f64::from(pos.y()) + 7.0,
            f64::from(pos.z()) + 9.0,
        );
        let bees = world.get_entities_in_aabb_matching(&area, |entity| {
            entity.downcast_ref::<BeeEntity>().is_some()
        });
        if bees.is_empty() {
            return;
        }
        let players = world.get_entities_in_aabb_matching(&area, |entity| {
            entity.entity_type() == &vanilla_entities::PLAYER
        });
        if players.is_empty() {
            return;
        }
        for entity in bees {
            let Some(bee) = entity.downcast_ref::<BeeEntity>() else {
                continue;
            };
            if bee.target().is_none() {
                let target = &players[rand::random_range(0..players.len())];
                let _ = bee.set_target(Some(target));
            }
        }
    }

    /// Vanilla `releaseBeesAndResetHoneyLevel`.
    pub fn release_bees_and_reset_honey_level(
        world: &Arc<World>,
        state: BlockStateId,
        pos: BlockPos,
        player: Option<&Player>,
        release_status: BeeReleaseStatus,
    ) {
        Self::reset_honey_level(world, state, pos);
        if let Some(block_entity) = Self::hive_at(world, pos)
            && let Some(hive) = block_entity.downcast_ref::<BeehiveBlockEntity>()
        {
            hive.empty_all_living_from_hive(player, state, release_status);
        }
    }

    /// Vanilla `resetHoneyLevel`.
    pub fn reset_honey_level(world: &Arc<World>, state: BlockStateId, pos: BlockPos) {
        world.set_block(
            pos,
            state.set_value(LEVEL_HONEY, 0),
            UpdateFlags::UPDATE_ALL,
        );
    }

    fn honey_state_component(state: BlockStateId) -> BlockItemStateProperties {
        let mut properties = BTreeMap::new();
        properties.insert(
            LEVEL_HONEY.name.to_owned(),
            state.get_value(LEVEL_HONEY).to_string(),
        );
        BlockItemStateProperties::new(properties)
    }

    fn prevents_bee_spawns(tool: &ItemStack) -> bool {
        let Some(enchantments) = tool.get_enchantments() else {
            return false;
        };
        enchantments.iter().any(|(key, _)| {
            REGISTRY
                .enchantments
                .by_key(key)
                .is_some_and(|enchantment| {
                    REGISTRY.enchantments.is_in_tag(
                        enchantment,
                        &EnchantmentTag::PREVENTS_BEE_SPAWNS_WHEN_MINING,
                    )
                })
        })
    }

    /// Vanilla `getDrops`: blocks blown up by these entities let their bees out.
    fn is_bee_releasing_explosive(entity: &dyn Entity) -> bool {
        let entity_type = entity.entity_type();
        entity_type == &vanilla_entities::TNT
            || entity_type == &vanilla_entities::CREEPER
            || entity_type == &vanilla_entities::WITHER_SKULL
            || entity_type == &vanilla_entities::WITHER
            || entity_type == &vanilla_entities::TNT_MINECART
    }
}

impl BlockBehavior for BeehiveBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(
            self.block
                .default_state()
                .set_value(HORIZONTAL_FACING, context.horizontal_direction().opposite()),
        )
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::BEEHIVE,
            level,
            pos,
            state,
        ))
    }

    fn get_block_entity_ticker(
        &self,
        _world: &Arc<World>,
        _state: BlockStateId,
        block_entity_type: BlockEntityTypeRef,
    ) -> Option<BlockEntityTicker> {
        BlockEntityTicker::for_matching_entity_tick(
            block_entity_type,
            &vanilla_block_entity_types::BEEHIVE,
        )
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
        state.get_value(LEVEL_HONEY).into()
    }

    fn player_destroy(
        &self,
        world: &Arc<World>,
        player: &Player,
        pos: BlockPos,
        state: BlockStateId,
        block_entity: Option<&SharedBlockEntity>,
        tool: &ItemStack,
    ) {
        let Some(hive) = block_entity.and_then(|be| be.downcast_ref::<BeehiveBlockEntity>()) else {
            return;
        };
        if !Self::prevents_bee_spawns(tool) {
            hive.empty_all_living_from_hive(Some(player), state, BeeReleaseStatus::Emergency);
            world.update_neighbor_for_output_signal(pos, state.get_block());
            Self::anger_nearby_bees(world, pos);
        }
    }

    fn on_explosion_hit(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        explosion: &dyn Explosion,
        on_hit: &mut dyn FnMut(ItemStack, BlockPos),
    ) {
        self.default_on_explosion_hit(state, world, pos, explosion, on_hit);
        Self::anger_nearby_bees(world, pos);
    }

    fn use_item_on(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let honey_level = state.get_value(LEVEL_HONEY);
        let mut hive_emptied = false;
        if honey_level >= MAX_HONEY_LEVELS {
            let held = inv.with_item(|item| item.clone());
            if held.is(&vanilla_items::SHEARS) {
                for drop in drop_from_block_interact_loot_table(
                    &vanilla_loot_tables::HARVEST_BEEHIVE,
                    state,
                    world.get_block_entity(pos),
                    Some(&held),
                    Some(player),
                    &mut rand::rng(),
                ) {
                    world.pop_resource(pos, drop);
                }
                world.play_sound_at(
                    &sound_events::BLOCK_BEEHIVE_SHEAR,
                    SoundSource::Blocks,
                    player.position(),
                    1.0,
                    1.0,
                    None,
                );
                let has_infinite_materials = player.has_infinite_materials();
                inv.with_item(|item| item.hurt_and_break(1, has_infinite_materials));
                hive_emptied = true;
                world.game_event(
                    &vanilla_game_events::SHEAR,
                    pos,
                    &GameEventContext::new(Some(player), None),
                );
            } else if held.is(&vanilla_items::GLASS_BOTTLE) {
                let emptied = inv.with_item(|item| {
                    item.shrink(1);
                    item.is_empty()
                });
                world.play_sound_at(
                    &sound_events::ITEM_BOTTLE_FILL,
                    SoundSource::Blocks,
                    player.position(),
                    1.0,
                    1.0,
                    Some(player.id()),
                );
                let honey_bottle = ItemStack::new(&vanilla_items::HONEY_BOTTLE);
                if emptied {
                    inv.with_inventory(|inventory| inventory.set_item_in_hand(hand, honey_bottle));
                } else {
                    player.add_item_or_drop(honey_bottle, Prediction::Predicted);
                }
                hive_emptied = true;
                world.game_event(
                    &vanilla_game_events::FLUID_PICKUP,
                    pos,
                    &GameEventContext::new(Some(player), None),
                );
            }
            if hive_emptied {
                player.award_stat(&vanilla_stat_types::ITEM_USED, held.item());
            }
        }

        if !hive_emptied {
            return InteractionResult::TryEmptyHandInteraction;
        }
        if CampfireBlock::is_smokey_pos(world.as_ref(), pos) {
            Self::reset_honey_level(world, state, pos);
        } else {
            let contains_bees = Self::hive_at(world, pos).is_some_and(|block_entity| {
                block_entity
                    .downcast_ref::<BeehiveBlockEntity>()
                    .is_some_and(|hive| !hive.is_empty())
            });
            if contains_bees {
                Self::anger_nearby_bees(world, pos);
            }
            Self::release_bees_and_reset_honey_level(
                world,
                state,
                pos,
                Some(player),
                BeeReleaseStatus::Emergency,
            );
        }
        InteractionResult::Success
    }

    fn player_will_destroy(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> BlockStateId {
        if !player.prevents_block_drops() || !world.get_game_rule(&BLOCK_DROPS) {
            return state;
        }
        let Some(block_entity) = Self::hive_at(world, pos) else {
            return state;
        };
        let Some(hive) = block_entity.downcast_ref::<BeehiveBlockEntity>() else {
            return state;
        };
        if !hive.is_empty() || state.get_value(LEVEL_HONEY) > 0 {
            let item = hive.beehive_as_item(state);
            if let Some(item_entity) = world.spawn_item(
                glam::DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z())),
                item,
            ) {
                item_entity.set_default_pickup_delay();
            }
        }
        state
    }

    fn get_drops(
        &self,
        state: BlockStateId,
        context: &BlockLootContext<'_>,
    ) -> Option<Vec<ItemStack>> {
        if context
            .entity()
            .is_some_and(Self::is_bee_releasing_explosive)
            && let Some(hive) = context
                .block_entity()
                .and_then(|block_entity| block_entity.downcast_ref::<BeehiveBlockEntity>())
        {
            hive.empty_all_living_from_hive(None, state, BeeReleaseStatus::Emergency);
        }
        None
    }

    fn get_clone_item_stack(
        &self,
        block: BlockRef,
        state: BlockStateId,
        _block_entity: Option<SharedBlockEntity>,
        include_data: bool,
    ) -> Option<ItemStack> {
        let item = REGISTRY.items.by_block(block);
        if !include_data {
            return Some(ItemStack::new(item));
        }
        let mut patch = DataComponentPatch::new();
        patch.set(BLOCK_STATE, Self::honey_state_component(state));
        Some(ItemStack::with_count_and_patch(item, 1, patch))
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        if world.get_block_state(neighbor_pos).get_block() == &vanilla_blocks::FIRE
            && let Some(block_entity) = world.get_block_entity(pos)
            && let Some(hive) = block_entity.downcast_ref::<BeehiveBlockEntity>()
        {
            hive.empty_all_living_from_hive(None, state, BeeReleaseStatus::Emergency);
        }
        state
    }
}
