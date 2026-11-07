//! Vanilla `CreakingHeartBlock`: the pale oak heart that awakens at night
//! and keeps a creaking alive.

use std::sync::{Arc, Weak};

use rand::RngExt as _;
use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, CreakingHeartState};
use steel_registry::{vanilla_block_entity_types, vanilla_damage_types};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Direction, Downcast as _};

use crate::behavior::{BlockBehavior, BlockEntityCreation, BlockPlaceContext};
use crate::block_entity::entities::{CreakingHeartBlockEntity, creaking_heart_has_required_logs};
use crate::block_entity::{BLOCK_ENTITIES, BlockEntityTicker, SharedBlockEntity};
use crate::entity::Entity as _;
use crate::entity::damage::DamageSource;
use crate::inventory::prelude::ItemStack;
use crate::player::Player;
use crate::world::explosion::Explosion;
use crate::world::{ScheduledTickAccess, World};

/// Vanilla `tryAwardExperience` range.
const MIN_EXPERIENCE: i32 = 20;
const MAX_EXPERIENCE: i32 = 24;

/// Creaking heart block behavior.
#[block_behavior]
pub struct CreakingHeartBlock {
    block: BlockRef,
}

impl CreakingHeartBlock {
    /// Creates the creaking heart behavior.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    /// Vanilla `updateState`: an uprooted heart wakes once its logs are back.
    fn update_state(state: BlockStateId, world: &World, pos: BlockPos) -> BlockStateId {
        let disabled = state.get_value(&BlockStateProperties::CREAKING_HEART_STATE)
            == CreakingHeartState::Uprooted;
        if disabled && creaking_heart_has_required_logs(state, world, pos) {
            let heart_state = if world.creaking_active() {
                CreakingHeartState::Awake
            } else {
                CreakingHeartState::Dormant
            };
            return state.set_value(&BlockStateProperties::CREAKING_HEART_STATE, heart_state);
        }
        state
    }

    fn heart_at(world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world.get_block_entity(pos).filter(|block_entity| {
            block_entity
                .downcast_ref::<CreakingHeartBlockEntity>()
                .is_some()
        })
    }

    /// Vanilla `tryAwardExperience`.
    fn try_award_experience(
        player: &Player,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
    ) {
        if !player.prevents_block_drops()
            && !player.is_spectator()
            && state.get_value(&BlockStateProperties::NATURAL)
        {
            let amount = rand::rng().random_range(MIN_EXPERIENCE..=MAX_EXPERIENCE);
            world.pop_experience(pos, amount);
        }
    }
}

impl BlockBehavior for CreakingHeartBlock {
    /// Vanilla `getStateForPlacement`.
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        let state = self.block.default_state().set_value(
            &BlockStateProperties::AXIS,
            context.clicked_face().get_axis(),
        );
        Some(Self::update_state(
            state,
            context.world,
            context.place_pos(),
        ))
    }

    /// Vanilla `updateShape`: re-checks the logs on the next tick.
    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        let _ = world.schedule_block_tick_default(pos, self.block, 1);
        state
    }

    fn tick(&self, state: BlockStateId, world: &Arc<World>, pos: BlockPos) {
        let new_state = Self::update_state(state, world, pos);
        if new_state != state {
            world.set_block(pos, new_state, UpdateFlags::UPDATE_ALL);
        }
    }

    // `animateTick` only plays the client-local idle sound.

    fn affect_neighbors_after_removal(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
    }

    /// Vanilla `onExplosionHit`: the blast kills the heart's creaking.
    fn on_explosion_hit(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        explosion: &dyn Explosion,
        on_hit: &mut dyn FnMut(ItemStack, BlockPos),
    ) {
        if explosion.should_affect_blocklike_entities()
            && let Some(block_entity) = Self::heart_at(world, pos)
            && let Some(heart) = block_entity.downcast_ref::<CreakingHeartBlockEntity>()
        {
            heart.remove_protector(world, Some(explosion.damage_source()));
            if let Some(indirect) = explosion.indirect_source_entity()
                && let Some(player) = indirect.as_player()
            {
                Self::try_award_experience(player, state, world, pos);
            }
        }
        self.default_on_explosion_hit(state, world, pos, explosion, on_hit);
    }

    /// Vanilla `playerWillDestroy`: breaking the heart kills its creaking.
    fn player_will_destroy(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> BlockStateId {
        if let Some(block_entity) = Self::heart_at(world, pos)
            && let Some(heart) = block_entity.downcast_ref::<CreakingHeartBlockEntity>()
        {
            let source = world.get_entity_by_id(player.id()).map_or_else(
                || DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK),
                |player_entity| {
                    DamageSource::direct(&vanilla_damage_types::PLAYER_ATTACK, player_entity)
                },
            );
            heart.remove_protector(world, Some(&source));
            Self::try_award_experience(player, state, world, pos);
        }
        state
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
        if state.get_value(&BlockStateProperties::CREAKING_HEART_STATE)
            == CreakingHeartState::Uprooted
        {
            return 0;
        }
        Self::heart_at(world, pos)
            .and_then(|block_entity| {
                block_entity
                    .downcast_ref::<CreakingHeartBlockEntity>()
                    .map(CreakingHeartBlockEntity::analog_output_signal)
            })
            .unwrap_or(0)
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::CREAKING_HEART,
            level,
            pos,
            state,
        ))
    }

    /// Vanilla `getTicker`: uprooted hearts do not tick.
    fn get_block_entity_ticker(
        &self,
        _world: &Arc<World>,
        state: BlockStateId,
        block_entity_type: BlockEntityTypeRef,
    ) -> Option<BlockEntityTicker> {
        if state.get_value(&BlockStateProperties::CREAKING_HEART_STATE)
            == CreakingHeartState::Uprooted
        {
            return None;
        }
        BlockEntityTicker::for_matching_entity_tick(
            block_entity_type,
            &vanilla_block_entity_types::CREAKING_HEART,
        )
    }
}
