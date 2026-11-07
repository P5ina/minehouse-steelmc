//! Ender chest block entity implementation.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::BaseNbtCompound;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::SoundSource;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{sound_events, vanilla_block_entity_types, vanilla_blocks};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::block_entity::container_openers::{
    ContainerOpenersCounter, ContainerOpenersHooks, container_sound_pitch,
};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::Entity as _;
use crate::player::Player;
use crate::world::World;

/// Block event that sets an ender chest's open count on clients.
const EVENT_SET_OPEN_COUNT: i32 = 1;

/// Ender chest block entity.
///
/// The items live in each player's own ender chest container; the block only
/// counts its viewers for the lid and the sounds.
pub struct EnderChestBlockEntity {
    base: Arc<BlockEntityBase>,
    openers: ContainerOpenersCounter,
}

// SAFETY: This key is owned by Steel and uniquely identifies `EnderChestBlockEntity`.
unsafe impl DowncastType for EnderChestBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/ender_chest");
}

impl EnderChestBlockEntity {
    /// Creates a new ender chest block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: Arc::new(BlockEntityBase::new(
                &vanilla_block_entity_types::ENDER_CHEST,
                level,
                pos,
                state,
            )),
            openers: ContainerOpenersCounter::new(),
        }
    }

    /// The number of players viewing this ender chest.
    #[must_use]
    pub fn opener_count(&self) -> i32 {
        self.openers.opener_count()
    }

    fn play_sound(world: &World, pos: BlockPos, sound: SoundEventRef) {
        world.play_sound_at(
            sound,
            SoundSource::Blocks,
            DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()) + 0.5,
                f64::from(pos.z()) + 0.5,
            ),
            0.5,
            container_sound_pitch(),
            None,
        );
    }
}

impl ContainerOpenersHooks for EnderChestBlockEntity {
    fn on_open(&self, world: &Arc<World>, pos: BlockPos, _state: BlockStateId) {
        Self::play_sound(world, pos, &sound_events::BLOCK_ENDER_CHEST_OPEN);
    }

    fn on_close(&self, world: &Arc<World>, pos: BlockPos, _state: BlockStateId) {
        Self::play_sound(world, pos, &sound_events::BLOCK_ENDER_CHEST_CLOSE);
    }

    /// Vanilla sends the event for `Blocks.ENDER_CHEST` at the entity's own
    /// position, whatever state it was given.
    fn opener_count_changed(
        &self,
        world: &Arc<World>,
        _pos: BlockPos,
        _state: BlockStateId,
        _previous: i32,
        current: i32,
    ) {
        world.block_event(
            self.base.pos(),
            &vanilla_blocks::ENDER_CHEST,
            EVENT_SET_OPEN_COUNT,
            current,
        );
    }

    /// The player's ender chest container was last opened from this chest,
    /// Vanilla `PlayerEnderChestContainer.isActiveChest`.
    fn is_own_container(&self, player: &Player) -> bool {
        player
            .ender_chest_inventory
            .lock()
            .is_active_chest(&self.base)
    }
}

impl BlockEntity for EnderChestBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    /// Clients animate the lid from this event; the server only accepts it.
    fn trigger_event(&self, param_a: i32, _param_b: i32) -> bool {
        param_a == EVENT_SET_OPEN_COUNT
    }

    fn start_open(&self, player: &Player) {
        if self.base.is_removed() || player.is_spectator() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers.increment_openers(
            self,
            player,
            &world,
            self.base.pos(),
            self.base.block_state(),
            player.block_interaction_range(),
        );
    }

    fn stop_open(&self, player: &Player) {
        if self.base.is_removed() || player.is_spectator() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers.decrement_openers(
            self,
            player,
            &world,
            self.base.pos(),
            self.base.block_state(),
        );
    }

    fn recheck_open(&self) {
        if self.base.is_removed() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers
            .recheck_openers(self, &world, self.base.pos(), self.base.block_state());
    }

    fn load_additional(&self, _nbt: &BaseNbtCompound<'_>) {}

    fn save_additional(&self, _nbt: &mut NbtCompound) {}
}
