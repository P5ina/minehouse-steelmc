//! Smithing table result slot.
use std::sync::Arc;

use steel_registry::{
    REGISTRY, RegistryHolder,
    data_components::{
        components::ArmorTrim,
        vanilla_components::{PROVIDES_TRIM_MATERIAL, TRIM},
    },
    level_events,
    recipe::{SmithingRecipe, SmithingRecipeInput, vanilla_recipe_types},
    trim_pattern::TrimPatternRef,
};
use steel_utils::{BlockPos, locks::Shared};

use crate::{
    inventory::{
        container::{ResultContainer, SimpleContainer},
        prelude::*,
    },
    world::World,
};

/// Vanilla `SmithingMenu.TEMPLATE_SLOT`.
pub const TEMPLATE_SLOT: usize = 0;
/// Vanilla `SmithingMenu.BASE_SLOT`.
pub const BASE_SLOT: usize = 1;
/// Vanilla `SmithingMenu.ADDITIONAL_SLOT`.
pub const ADDITIONAL_SLOT: usize = 2;

/// Vanilla `SmithingMenu.createResult`: the output of the first matching smithing
/// recipe for the three inputs, or empty.
#[must_use]
pub fn assemble_smithing(
    template: &ItemStack,
    base: &ItemStack,
    addition: &ItemStack,
) -> ItemStack {
    let input = SmithingRecipeInput::new(template.clone(), base.clone(), addition.clone());
    let Some(recipe) = REGISTRY
        .recipes
        .find_match(&vanilla_recipe_types::SMITHING, &input)
    else {
        return ItemStack::empty();
    };
    match recipe.data() {
        // Vanilla `TransmuteRecipe.createWithOriginalComponents`.
        SmithingRecipe::Transform(recipe) => recipe
            .result
            .apply(recipe.result.count(), input.base.components_patch()),
        SmithingRecipe::Trim(recipe) => apply_trim(&input.base, &input.addition, recipe.pattern),
    }
}

/// Vanilla `SmithingTrimRecipe.applyTrim`.
#[must_use]
pub fn apply_trim(base: &ItemStack, material: &ItemStack, pattern: TrimPatternRef) -> ItemStack {
    let Some(material) = material.get(PROVIDES_TRIM_MATERIAL) else {
        return ItemStack::empty();
    };
    let new_trim = ArmorTrim::new(
        material.material().clone(),
        RegistryHolder::Reference(pattern),
    );
    if base.get(TRIM) == Some(&new_trim) {
        return ItemStack::empty();
    }
    let mut trimmed = base.copy_with_count(1);
    trimmed.set(TRIM, new_trim);
    trimmed
}

/// Result slot handler for a smithing table.
#[derive(Clone)]
pub struct SmithingResultHandler {
    input_container: Shared<SimpleContainer>,
    result_container: Shared<ResultContainer>,
    block_pos: BlockPos,
    world: Arc<World>,
}

impl SmithingResultHandler {
    /// Creates a new handler.
    pub const fn new(
        input_container: Shared<SimpleContainer>,
        result_container: Shared<ResultContainer>,
        block_pos: BlockPos,
        world: Arc<World>,
    ) -> Self {
        Self {
            input_container,
            result_container,
            block_pos,
            world,
        }
    }
}

impl ResultHandler for SmithingResultHandler {
    fn result_container(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.result_container))
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        vec![ContainerRef::from(Arc::clone(&self.input_container))]
    }

    /// Vanilla `SmithingMenu.createResult`.
    fn update_result(&self, guard: &mut ContainerLockGuard) {
        let Some(input) = guard.get(ContainerId::from_arc(&self.input_container)) else {
            log::warn!("input container not locked while creating the smithing result");
            return;
        };
        let result = assemble_smithing(
            input.get_item(TEMPLATE_SLOT),
            input.get_item(BASE_SLOT),
            input.get_item(ADDITIONAL_SLOT),
        );
        let Some(result_container) = guard.get_mut(ContainerId::from_arc(&self.result_container))
        else {
            log::warn!("result container not locked while creating the smithing result");
            return;
        };
        result_container.set_item(0, result);
    }

    /// Vanilla `SmithingMenu.onTake`.
    // TODO: Award the used recipe once player recipe books exist.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) -> Option<ItemStack> {
        {
            let Some(input) = guard.get_mut(ContainerId::from_arc(&self.input_container)) else {
                log::warn!("input container not locked while taking the smithing result");
                return None;
            };
            for slot in [TEMPLATE_SLOT, BASE_SLOT, ADDITIONAL_SLOT] {
                let stack = input.get_item_mut(slot);
                if !stack.is_empty() {
                    stack.shrink(1);
                }
            }
            input.set_changed();
        }
        self.world.level_event(
            level_events::SOUND_SMITHING_TABLE_USED,
            self.block_pos,
            0,
            None,
        );
        // Vanilla's input container recomputes the result as soon as it changes,
        // which lets a shift-click keep upgrading while inputs remain.
        self.update_result(guard);
        None
    }

    /// Vanilla `ItemCombinerMenu.mayPickup` always allows taking the result.
    fn is_result_valid(&self, _guard: &ContainerLockGuard, _player: &Player) -> bool {
        true
    }
}
