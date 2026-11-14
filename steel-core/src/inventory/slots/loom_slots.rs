//! Loom result slot and pattern selection.
use std::ptr;
use std::sync::Arc;

use steel_protocol::packets::game::SoundSource;
use steel_registry::banner_pattern::BannerPattern;
use steel_registry::data_components::components::{BannerPatternLayer, BannerPatternLayers};
use steel_registry::data_components::vanilla_components::{
    BANNER_PATTERNS, DYE, PROVIDES_BANNER_PATTERNS,
};
use steel_registry::sound_events;
use steel_registry::vanilla_banner_pattern_tags::BannerPatternTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, RegistryHolder, RegistryHolderSet, TaggedRegistryExt as _};
use steel_utils::{
    BlockPos,
    locks::{Shared, SyncMutex},
};

use crate::behavior::ITEM_BEHAVIORS;
use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        prelude::*,
    },
    world::World,
};

/// Vanilla `LoomMenu.bannerSlot`'s container slot.
pub const LOOM_BANNER_SLOT: usize = 0;
/// Vanilla `LoomMenu.dyeSlot`'s container slot.
pub const LOOM_DYE_SLOT: usize = 1;
/// Vanilla `LoomMenu.patternSlot`'s container slot.
pub const LOOM_PATTERN_SLOT: usize = 2;
/// Vanilla `LoomMenu.PATTERN_NOT_SET`.
pub const PATTERN_NOT_SET: i32 = -1;
/// Banners take at most six pattern layers in the loom.
const MAX_PATTERNS: usize = 6;

/// Vanilla `instanceof BannerItem`.
#[must_use]
pub fn is_banner_item(stack: &ItemStack) -> bool {
    ITEM_BEHAVIORS
        .get_behavior(stack.item())
        .as_banner_item()
        .is_some()
}

/// Vanilla `LoomMenu.isDyeItem`.
#[must_use]
pub fn is_dye_item(stack: &ItemStack) -> bool {
    stack.item().has_tag(&ItemTag::LOOM_DYES) && stack.has(DYE)
}

/// Vanilla `LoomMenu.isPatternItem`.
#[must_use]
pub fn is_pattern_item(stack: &ItemStack) -> bool {
    stack.item().has_tag(&ItemTag::LOOM_PATTERNS) && stack.has(PROVIDES_BANNER_PATTERNS)
}

/// Vanilla `LoomMenu.getSelectablePatterns`.
#[must_use]
pub fn selectable_patterns(pattern_stack: &ItemStack) -> Vec<&'static BannerPattern> {
    let holder_set = if pattern_stack.is_empty() {
        RegistryHolderSet::Tag(BannerPatternTag::NO_ITEM_REQUIRED)
    } else {
        match pattern_stack.get(PROVIDES_BANNER_PATTERNS) {
            Some(patterns) => patterns.clone(),
            None => return Vec::new(),
        }
    };
    match holder_set {
        RegistryHolderSet::Tag(tag) => REGISTRY.banner_patterns.iter_tag(&tag).collect(),
        RegistryHolderSet::Direct(patterns) => patterns,
    }
}

/// Vanilla `LoomMenu.setupResultSlot`'s result for `pattern`.
#[must_use]
pub fn loom_result(
    banner: &ItemStack,
    dye: &ItemStack,
    pattern: &'static BannerPattern,
) -> ItemStack {
    if banner.is_empty() || dye.is_empty() {
        return ItemStack::empty();
    }
    let Some(color) = dye.get(DYE).copied() else {
        return ItemStack::empty();
    };
    let mut result = banner.copy_with_count(1);
    let mut layers = result
        .get(BANNER_PATTERNS)
        .map_or_else(Vec::new, |layers| layers.layers().to_vec());
    layers.push(BannerPatternLayer::new(
        RegistryHolder::Reference(pattern),
        color,
    ));
    result.set(BANNER_PATTERNS, BannerPatternLayers::new(layers));
    result
}

/// Loom state shared between the menu and its result slot.
#[derive(Debug, Default)]
pub struct LoomState {
    /// Vanilla `LoomMenu.selectablePatterns`.
    pub selectable_patterns: Vec<&'static BannerPattern>,
    /// Server copy of the `selectedBannerPatternIndex` data slot; Vanilla
    /// `DataSlot.standalone()` starts at zero.
    pub selected_pattern_index: i32,
    /// Vanilla `LoomMenu.lastSoundTime`.
    pub last_sound_time: i64,
}

impl LoomState {
    /// Vanilla `LoomMenu.isValidPatternIndex`.
    #[must_use]
    pub fn is_valid_pattern_index(&self, index: i32) -> bool {
        usize::try_from(index).is_ok_and(|index| index < self.selectable_patterns.len())
    }

    /// The pattern at `index`, if it is valid.
    #[must_use]
    pub fn pattern_at(&self, index: i32) -> Option<&'static BannerPattern> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.selectable_patterns.get(index).copied())
    }

    /// Vanilla `LoomMenu.slotsChanged`: updates the pattern list and the
    /// selection for the current inputs and returns the new result.
    pub fn slots_changed(
        &mut self,
        banner: &ItemStack,
        dye: &ItemStack,
        pattern: &ItemStack,
        current_result: &ItemStack,
    ) -> ItemStack {
        if banner.is_empty() || dye.is_empty() {
            self.selectable_patterns = Vec::new();
            self.selected_pattern_index = PATTERN_NOT_SET;
            return ItemStack::empty();
        }

        let selected = self.selected_pattern_index;
        let previous = self.pattern_at(selected);
        self.selectable_patterns = selectable_patterns(pattern);
        let to_display = if self.selectable_patterns.len() == 1 {
            self.selected_pattern_index = 0;
            Some(self.selectable_patterns[0])
        } else if let Some(previous) = previous {
            if let Some(index) = self
                .selectable_patterns
                .iter()
                .position(|candidate| ptr::eq(*candidate, previous))
            {
                self.selected_pattern_index = i32::try_from(index).unwrap_or(PATTERN_NOT_SET);
                Some(previous)
            } else {
                self.selected_pattern_index = PATTERN_NOT_SET;
                None
            }
        } else {
            self.selected_pattern_index = PATTERN_NOT_SET;
            None
        };

        let Some(to_display) = to_display else {
            return ItemStack::empty();
        };
        let has_max_patterns = banner
            .get(BANNER_PATTERNS)
            .is_some_and(|layers| layers.layers().len() >= MAX_PATTERNS);
        if has_max_patterns {
            self.selected_pattern_index = PATTERN_NOT_SET;
            return ItemStack::empty();
        }
        setup_result(banner, dye, to_display, current_result)
    }
}

/// Vanilla `LoomMenu.setupResultSlot`, which keeps the current result when it
/// already matches.
fn setup_result(
    banner: &ItemStack,
    dye: &ItemStack,
    pattern: &'static BannerPattern,
    current_result: &ItemStack,
) -> ItemStack {
    let result = loom_result(banner, dye, pattern);
    if ItemStack::matches(&result, current_result) {
        current_result.clone()
    } else {
        result
    }
}

/// Result slot handler for a loom.
#[derive(Clone)]
pub struct LoomResultHandler {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    state: Arc<SyncMutex<LoomState>>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl LoomResultHandler {
    /// Creates a new handler.
    pub const fn new(
        input_container: Shared<SimpleContainer>,
        result_container: Shared<ResultContainer>,
        state: Arc<SyncMutex<LoomState>>,
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

impl ResultHandler for LoomResultHandler {
    fn result_container(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.result_container))
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        vec![ContainerRef::from(Arc::clone(&self.input_container))]
    }

    /// The menu sets the result when a pattern is selected or the inputs change.
    fn update_result(&self, _guard: &mut ContainerLockGuard, _player: &Player) {}

    /// Vanilla `LoomMenu`'s result slot `onTake`.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) -> Option<ItemStack> {
        let inputs = {
            let Some(input) = guard.get_mut(ContainerId::from_arc(&self.input_container)) else {
                log::warn!("input container not locked while taking the loom result");
                return None;
            };
            input.remove_item(LOOM_BANNER_SLOT, 1);
            input.remove_item(LOOM_DYE_SLOT, 1);
            input.set_changed();
            [LOOM_BANNER_SLOT, LOOM_DYE_SLOT, LOOM_PATTERN_SLOT]
                .map(|slot| input.get_item(slot).clone())
        };

        let mut state = self.state.lock();
        if inputs[0].is_empty() || inputs[1].is_empty() {
            state.selected_pattern_index = PATTERN_NOT_SET;
        }
        // Vanilla's input container runs `slotsChanged` as soon as it changes,
        // which lets a shift-click keep weaving while inputs remain.
        if let Some(result_container) = guard.get_mut(ContainerId::from_arc(&self.result_container))
        {
            let result =
                state.slots_changed(&inputs[0], &inputs[1], &inputs[2], &ItemStack::empty());
            result_container.set_item(0, result);
        }

        let game_time = self.world.game_time();
        if state.last_sound_time != game_time {
            self.world.play_sound(
                &sound_events::UI_LOOM_TAKE_RESULT,
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

    /// Vanilla's loom result slot keeps the default `mayPickup`.
    fn is_result_valid(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }
}
