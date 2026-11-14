//! Passive brewing recipe data and input snapshots.

use steel_utils::{DowncastType, DowncastTypeKey};

use crate::RegistryHolderSet;
use crate::data_components::vanilla_components::{POTION_CONTENTS, PotionContents};
use crate::item_stack::ItemStack;
use crate::item_stack_template::ItemStackTemplate;
use crate::potion::Potion;

use super::{Ingredient, RecipeData, RecipeInput, RecipeMatches, RecipeProperties};

/// Vanilla `PotionIngredient`: an item ingredient, optionally narrowed to some
/// potions through a `PotionsPredicate`.
#[derive(Debug, Clone)]
pub struct PotionIngredient {
    pub ingredient: Ingredient,
    /// Vanilla `PotionsPredicate.potions`. No vanilla brewing recipe narrows by
    /// effects, so the predicate's optional `effects` list is not modeled.
    pub potions: Option<RegistryHolderSet<Potion>>,
}

impl PotionIngredient {
    /// Vanilla `PotionIngredient.test`.
    #[must_use]
    pub fn test(&self, stack: &ItemStack) -> bool {
        if !self.ingredient.test(stack) {
            return false;
        }
        let Some(potions) = &self.potions else {
            return true;
        };
        // Vanilla `PotionsPredicate.matches` on the stack's `potion_contents`.
        stack
            .get(POTION_CONTENTS)
            .and_then(PotionContents::potion)
            .is_some_and(|potion| potions.contains(potion.value()))
    }
}

/// Vanilla `BrewingRecipe`.
#[derive(Debug)]
pub struct BrewingRecipe {
    pub properties: RecipeProperties,
    pub input: PotionIngredient,
    pub reagent: PotionIngredient,
    pub output: ItemStackTemplate,
}

// SAFETY: This Steel-owned key uniquely identifies vanilla brewing recipe data.
unsafe impl DowncastType for BrewingRecipe {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:recipe_data/brewing");
}

impl BrewingRecipe {
    /// Vanilla `BrewingRecipe.assemble`.
    #[must_use]
    pub fn assemble(&self) -> ItemStack {
        self.output.create()
    }
}

impl RecipeData for BrewingRecipe {
    fn properties(&self) -> Option<&RecipeProperties> {
        Some(&self.properties)
    }
}

/// Vanilla `BrewingInput`: one bottle and the reagent.
#[derive(Debug, Clone)]
pub struct BrewingInput {
    pub input: ItemStack,
    pub reagent: ItemStack,
}

impl BrewingInput {
    #[must_use]
    pub const fn new(input: ItemStack, reagent: ItemStack) -> Self {
        Self { input, reagent }
    }
}

// SAFETY: This Steel-owned key uniquely identifies a brewing matching snapshot.
unsafe impl DowncastType for BrewingInput {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:recipe_input/brewing");
}

impl RecipeInput for BrewingInput {
    fn is_empty(&self) -> bool {
        self.input.is_empty() && self.reagent.is_empty()
    }
}

impl RecipeMatches<BrewingInput> for BrewingRecipe {
    /// Vanilla `BrewingRecipe.matches`.
    fn matches(&self, input: &BrewingInput) -> bool {
        self.input.test(&input.input) && self.reagent.test(&input.reagent)
    }
}
