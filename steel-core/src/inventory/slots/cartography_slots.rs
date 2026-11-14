//! Cartography table result slot and result rules.
use std::sync::Arc;

use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::components::MapPostProcessing;
use steel_registry::data_components::vanilla_components::{MAP_ID, MAP_POST_PROCESSING};
use steel_registry::sound_events;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::vanilla_items;
use steel_utils::{
    BlockPos,
    locks::{Shared, SyncMutex},
};

use crate::behavior::on_crafted_by;
use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        prelude::*,
    },
    world::{World, maps::MAX_SCALE},
};

/// Vanilla `CartographyTableMenu.MAP_SLOT`'s container slot.
pub const CARTOGRAPHY_MAP_SLOT: usize = 0;
/// Vanilla `CartographyTableMenu.ADDITIONAL_SLOT`'s container slot.
pub const CARTOGRAPHY_ADDITIONAL_SLOT: usize = 1;

/// Vanilla's map slot `mayPlace`.
#[must_use]
pub fn is_cartography_map(stack: &ItemStack) -> bool {
    stack.has(MAP_ID)
}

/// Vanilla's additional slot `mayPlace`.
#[must_use]
pub fn is_cartography_additional(stack: &ItemStack) -> bool {
    stack.is(&vanilla_items::PAPER)
        || stack.is(&vanilla_items::MAP)
        || stack.is(&vanilla_items::GLASS_PANE)
}

/// Vanilla `CartographyTableMenu.slotsChanged` with `setupResultSlot`.
///
/// Returns the new result, or `None` when the result stays as it is.
#[must_use]
pub fn cartography_result(
    world: &World,
    map: &ItemStack,
    additional: &ItemStack,
    current: &ItemStack,
) -> Option<ItemStack> {
    let both_filled = !map.is_empty() && !additional.is_empty();
    if !current.is_empty() && !both_filled {
        return Some(ItemStack::empty());
    }
    if !both_filled {
        return None;
    }

    let data = world.map_data(*map.get(MAP_ID)?)?;
    let (locked, scale) = {
        let data = data.lock();
        (data.locked, data.scale)
    };
    let result = if additional.is(&vanilla_items::PAPER)
        && map.item().has_tag(&ItemTag::EXTENDABLE_MAPS)
        && !locked
        && scale < MAX_SCALE
    {
        let mut result = map.copy_with_count(1);
        result.set(MAP_POST_PROCESSING, MapPostProcessing::Scale);
        result
    } else if additional.is(&vanilla_items::GLASS_PANE) && !locked {
        let mut result = map.copy_with_count(1);
        result.set(MAP_POST_PROCESSING, MapPostProcessing::Lock);
        result
    } else if additional.is(&vanilla_items::MAP) {
        map.copy_with_count(2)
    } else {
        return Some(ItemStack::empty());
    };
    (!ItemStack::matches(&result, current)).then_some(result)
}

/// Result slot handler for a cartography table.
#[derive(Clone)]
pub struct CartographyResultHandler {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    /// Vanilla `CartographyTableMenu.lastSoundTime`.
    last_sound_time: Arc<SyncMutex<i64>>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl CartographyResultHandler {
    /// Creates a new handler.
    pub fn new(
        input_container: Shared<SimpleContainer>,
        result_container: Shared<ResultContainer>,
        block_pos: BlockPos,
        world: Arc<World>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            last_sound_time: Arc::new(SyncMutex::new(0)),
            block_pos,
            world,
        }
    }
}

impl ResultHandler for CartographyResultHandler {
    fn result_container(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.result_container))
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        vec![ContainerRef::from(Arc::clone(&self.input_container))]
    }

    /// The menu sets the result when the inputs change.
    fn update_result(&self, _guard: &mut ContainerLockGuard, _player: &Player) {}

    /// Vanilla's result slot `onTake` and `quickMoveStack` finish the map with
    /// `Item.onCraftedBy`.
    fn on_crafted_by(&self, stack: &mut ItemStack, player: &Player) {
        on_crafted_by(stack, player);
    }

    /// Vanilla `CartographyTableMenu`'s result slot `onTake`.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) -> Option<ItemStack> {
        let (map, additional) = {
            let Some(input) = guard.get_mut(ContainerId::from_arc(&self.input_container)) else {
                log::warn!("input container not locked while taking the cartography result");
                return None;
            };
            input.remove_item(CARTOGRAPHY_MAP_SLOT, 1);
            input.remove_item(CARTOGRAPHY_ADDITIONAL_SLOT, 1);
            input.set_changed();
            (
                input.get_item(CARTOGRAPHY_MAP_SLOT).clone(),
                input.get_item(CARTOGRAPHY_ADDITIONAL_SLOT).clone(),
            )
        };

        // Vanilla's input container runs `slotsChanged` as soon as it changes, while
        // the taken result has already left its slot.
        if let Some(result_container) = guard.get_mut(ContainerId::from_arc(&self.result_container))
            && let Some(result) =
                cartography_result(&self.world, &map, &additional, &ItemStack::empty())
        {
            result_container.set_item(0, result);
        }

        let game_time = self.world.game_time();
        let mut last_sound_time = self.last_sound_time.lock();
        if *last_sound_time != game_time {
            self.world.play_sound(
                &sound_events::UI_CARTOGRAPHY_TABLE_TAKE_RESULT,
                SoundSource::Blocks,
                self.block_pos,
                1.0,
                1.0,
                None,
            );
            *last_sound_time = game_time;
        }
        None
    }

    /// Vanilla's cartography result slot keeps the default `mayPickup`.
    fn is_result_valid(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }
}
