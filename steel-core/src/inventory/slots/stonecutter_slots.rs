//! Stonecutter result slot.
use std::sync::Arc;

use steel_protocol::packets::game::SoundSource;
use steel_registry::sound_events;
use steel_utils::{
    BlockPos,
    locks::{Shared, SyncMutex},
};

use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        prelude::*,
        recipe_access::StonecutterRecipeSet,
    },
    world::World,
};

/// Stonecutter state shared between the menu and its result slot.
#[derive(Debug)]
pub struct StonecutterState {
    /// Vanilla `StonecutterMenu.recipesForInput`.
    pub recipes_for_input: StonecutterRecipeSet,
    /// Vanilla `StonecutterMenu.input`: the input the recipe list was built for.
    pub input: ItemStack,
    /// Server copy of the `selectedRecipeIndex` data slot.
    pub selected_recipe_index: i32,
    /// Vanilla `StonecutterMenu.lastSoundTime`.
    pub last_sound_time: i64,
}

impl Default for StonecutterState {
    fn default() -> Self {
        Self {
            recipes_for_input: StonecutterRecipeSet::empty(),
            input: ItemStack::empty(),
            // Vanilla `DataSlot.standalone()` starts at zero.
            selected_recipe_index: 0,
            last_sound_time: 0,
        }
    }
}

impl StonecutterState {
    /// Vanilla `StonecutterMenu.isValidRecipeIndex`.
    #[must_use]
    pub fn is_valid_recipe_index(&self, index: i32) -> bool {
        usize::try_from(index).is_ok_and(|index| index < self.recipes_for_input.len())
    }

    /// The result Vanilla `StonecutterMenu.setupResultSlot` shows for `index`.
    #[must_use]
    pub fn result_for(&self, index: i32) -> ItemStack {
        if self.recipes_for_input.is_empty() || !self.is_valid_recipe_index(index) {
            return ItemStack::empty();
        }
        usize::try_from(index)
            .ok()
            .and_then(|index| self.recipes_for_input.get(index))
            .map_or_else(ItemStack::empty, |recipe| recipe.data().result.create())
    }
}

/// Result slot handler for a stonecutter.
#[derive(Clone)]
pub struct StonecutterResultHandler {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    state: Arc<SyncMutex<StonecutterState>>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl StonecutterResultHandler {
    /// Creates a new handler.
    pub const fn new(
        input_container: Shared<SimpleContainer>,
        result_container: Shared<ResultContainer>,
        state: Arc<SyncMutex<StonecutterState>>,
        block_pos: BlockPos,
        world: Arc<World>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            state,
            block_pos,
            world,
        }
    }
}

impl ResultHandler for StonecutterResultHandler {
    fn result_container(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.result_container))
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        vec![ContainerRef::from(Arc::clone(&self.input_container))]
    }

    /// The menu sets the result when a recipe is selected or the input changes.
    fn update_result(&self, _guard: &mut ContainerLockGuard) {}

    /// Vanilla `StonecutterMenu`'s result slot `onTake`.
    // TODO: Award the used recipe once player recipe books exist.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) -> Option<ItemStack> {
        let input_left = {
            let Some(input) = guard.get_mut(ContainerId::from_arc(&self.input_container)) else {
                log::warn!("input container not locked while taking the stonecutter result");
                return None;
            };
            let removed = input.remove_item(0, 1);
            input.set_changed();
            (!removed.is_empty()).then(|| !input.get_item(0).is_empty())
        };

        let mut state = self.state.lock();
        if let Some(input_left) = input_left {
            // Vanilla re-runs `setupResultSlot` with the selected recipe. An input
            // that ran out has already reset the recipe list through
            // `slotsChanged`, which leaves the result empty; the menu resets the
            // selection itself when it sees the changed input.
            let result = if input_left {
                state.result_for(state.selected_recipe_index)
            } else {
                ItemStack::empty()
            };
            if let Some(result_container) =
                guard.get_mut(ContainerId::from_arc(&self.result_container))
            {
                result_container.set_item(0, result);
            }
        }

        let game_time = self.world.game_time();
        if state.last_sound_time != game_time {
            self.world.play_sound(
                &sound_events::UI_STONECUTTER_TAKE_RESULT,
                SoundSource::Blocks,
                self.block_pos,
                1.0,
                1.0,
                None,
            );
            state.last_sound_time = game_time;
        }
        None
    }

    /// Vanilla's stonecutter result slot keeps the default `mayPickup`.
    fn is_result_valid(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }
}
