//! Vanilla `RecipeAccess`: the recipe data menus test slots against and that is
//! synchronized to clients with `ClientboundUpdateRecipesPacket`.
//!
//! Mirrors `RecipeManager.finalizeRecipeLoading`, which collects every
//! [`RecipePropertySet`] and the selectable stonecutter recipe list once the
//! recipes are loaded.

use std::sync::LazyLock;

use rustc_hash::FxHashSet;
use steel_protocol::packets::game::{
    CUpdateRecipes, RecipePropertySetEntry, StonecutterRecipeEntry,
};
use steel_registry::recipe::{
    CookingRecipe, Ingredient, PlacementInfo, RecipeProperties, RecipeType, SingleItemRecipeInput,
    SmithingRecipe, StonecuttingRecipe, TypedRecipeRef, vanilla_recipe_types,
};
use steel_registry::{
    REGISTRY, RegistryEntry, RegistryHolderSet, item_stack::ItemStack, items::Item,
};
use steel_utils::{Identifier, codec::VarInt};

/// Vanilla `RecipePropertySet.SMITHING_BASE`.
pub const SMITHING_BASE: Identifier = Identifier::vanilla_static("smithing_base");
/// Vanilla `RecipePropertySet.SMITHING_TEMPLATE`.
pub const SMITHING_TEMPLATE: Identifier = Identifier::vanilla_static("smithing_template");
/// Vanilla `RecipePropertySet.SMITHING_ADDITION`.
pub const SMITHING_ADDITION: Identifier = Identifier::vanilla_static("smithing_addition");
/// Vanilla `RecipePropertySet.FURNACE_INPUT`.
pub const FURNACE_INPUT: Identifier = Identifier::vanilla_static("furnace_input");
/// Vanilla `RecipePropertySet.BLAST_FURNACE_INPUT`.
pub const BLAST_FURNACE_INPUT: Identifier = Identifier::vanilla_static("blast_furnace_input");
/// Vanilla `RecipePropertySet.SMOKER_INPUT`.
pub const SMOKER_INPUT: Identifier = Identifier::vanilla_static("smoker_input");
/// Vanilla `RecipePropertySet.CAMPFIRE_INPUT`.
pub const CAMPFIRE_INPUT: Identifier = Identifier::vanilla_static("campfire_input");
/// Vanilla `RecipePropertySet.BREWING_INPUTS`.
pub const BREWING_INPUTS: Identifier = Identifier::vanilla_static("brewing_input");
/// Vanilla `RecipePropertySet.BREWING_REAGENTS`.
pub const BREWING_REAGENTS: Identifier = Identifier::vanilla_static("brewing_reagent");

/// Typed reference to one registered stonecutter recipe.
pub type StonecutterRecipeRef = TypedRecipeRef<StonecuttingRecipe, SingleItemRecipeInput>;

/// Vanilla `RecipePropertySet`: the set of items some menu slot accepts.
#[derive(Debug, Default)]
pub struct RecipePropertySet {
    /// Raw item registry ids.
    items: FxHashSet<usize>,
}

impl RecipePropertySet {
    /// Vanilla `RecipePropertySet.create`: every item of every ingredient.
    fn create<'a>(ingredients: impl IntoIterator<Item = &'a Ingredient>) -> Self {
        Self {
            items: ingredients
                .into_iter()
                .flat_map(Ingredient::get_items)
                .map(RegistryEntry::id)
                .collect(),
        }
    }

    /// Vanilla `RecipePropertySet.test`.
    #[must_use]
    pub fn test(&self, stack: &ItemStack) -> bool {
        self.items.contains(&stack.item.id())
    }

    fn network_ids(&self) -> Vec<VarInt> {
        let mut ids: Vec<i32> = self.items.iter().map(|&id| id as i32).collect();
        // Vanilla sends an unordered set; sorting keeps the packet deterministic.
        ids.sort_unstable();
        ids.into_iter().map(VarInt).collect()
    }
}

/// Vanilla `SelectableRecipe.SingleInputSet<StonecutterRecipe>`.
#[derive(Debug, Clone, Default)]
pub struct StonecutterRecipeSet {
    entries: Vec<StonecutterRecipeRef>,
}

impl StonecutterRecipeSet {
    /// Vanilla `SingleInputSet.empty`.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Vanilla `SingleInputSet.acceptsInput`.
    #[must_use]
    pub fn accepts_input(&self, input: &ItemStack) -> bool {
        self.entries
            .iter()
            .any(|recipe| recipe.data().ingredient.test(input))
    }

    /// Vanilla `SingleInputSet.selectByInput`, keeping the full list's order.
    #[must_use]
    pub fn select_by_input(&self, input: &ItemStack) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .copied()
                .filter(|recipe| recipe.data().ingredient.test(input))
                .collect(),
        }
    }

    /// The recipe at `index`, if it is in range.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<StonecutterRecipeRef> {
        self.entries.get(index).copied()
    }

    /// Vanilla `SingleInputSet.isEmpty`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Vanilla `SingleInputSet.size`.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Everything `RecipeManager.finalizeRecipeLoading` derives from the loaded recipes.
#[derive(Debug)]
pub struct RecipeAccess {
    /// Keyed in Vanilla's `RECIPE_PROPERTY_SETS` declaration order.
    property_sets: Vec<(Identifier, RecipePropertySet)>,
    stonecutter_recipes: StonecutterRecipeSet,
}

/// The server's recipe access, built from the frozen recipe registry on first use.
pub static RECIPE_ACCESS: LazyLock<RecipeAccess> = LazyLock::new(RecipeAccess::finalize);

/// Vanilla ignores non-special recipes whose ingredients can never be placed.
fn is_ignored(properties: &RecipeProperties) -> bool {
    !properties.special
        && properties
            .placement
            .as_ref()
            .is_some_and(PlacementInfo::is_impossible_to_place)
}

impl RecipeAccess {
    fn finalize() -> Self {
        let smithing: Vec<&SmithingRecipe> = REGISTRY
            .recipes
            .by_type(&vanilla_recipe_types::SMITHING)
            .iter()
            .map(TypedRecipeRef::data)
            .filter(|recipe| !is_ignored(recipe.properties()))
            .collect();
        let smithing_set = |extract: fn(&SmithingRecipe) -> Option<&Ingredient>| {
            RecipePropertySet::create(smithing.iter().filter_map(|recipe| extract(recipe)))
        };

        let cooking_set =
            |recipe_type: &'static RecipeType<CookingRecipe, SingleItemRecipeInput>| {
                let recipes = REGISTRY.recipes.by_type(recipe_type);
                RecipePropertySet::create(
                    recipes
                        .iter()
                        .map(TypedRecipeRef::data)
                        .filter(|recipe| !is_ignored(&recipe.properties))
                        .map(|recipe| &recipe.ingredient)
                        .collect::<Vec<_>>(),
                )
            };

        let property_sets = vec![
            (
                SMITHING_ADDITION,
                smithing_set(SmithingRecipe::addition_ingredient),
            ),
            (
                SMITHING_BASE,
                smithing_set(|recipe| Some(recipe.base_ingredient())),
            ),
            (
                SMITHING_TEMPLATE,
                smithing_set(SmithingRecipe::template_ingredient),
            ),
            (FURNACE_INPUT, cooking_set(&vanilla_recipe_types::SMELTING)),
            (
                BLAST_FURNACE_INPUT,
                cooking_set(&vanilla_recipe_types::BLASTING),
            ),
            (SMOKER_INPUT, cooking_set(&vanilla_recipe_types::SMOKING)),
            (
                CAMPFIRE_INPUT,
                cooking_set(&vanilla_recipe_types::CAMPFIRE_COOKING),
            ),
            // TODO: Collect brewing recipe inputs and reagents once the brewing
            // recipe type is registered with the brewing stand.
            (BREWING_INPUTS, RecipePropertySet::default()),
            (BREWING_REAGENTS, RecipePropertySet::default()),
        ];

        let stonecutter_recipes = StonecutterRecipeSet {
            entries: REGISTRY
                .recipes
                .by_type(&vanilla_recipe_types::STONECUTTING)
                .iter()
                .filter(|recipe| !is_ignored(&recipe.data().properties))
                .collect(),
        };

        Self {
            property_sets,
            stonecutter_recipes,
        }
    }

    /// Vanilla `RecipeAccess.propertySet`; unknown keys are empty.
    #[must_use]
    pub fn property_set(&self, key: &Identifier) -> &RecipePropertySet {
        static EMPTY: LazyLock<RecipePropertySet> = LazyLock::new(RecipePropertySet::default);
        self.property_sets
            .iter()
            .find(|(set_key, _)| set_key == key)
            .map_or(&*EMPTY, |(_, set)| set)
    }

    /// Vanilla `RecipeAccess.stonecutterRecipes`.
    #[must_use]
    pub const fn stonecutter_recipes(&self) -> &StonecutterRecipeSet {
        &self.stonecutter_recipes
    }

    /// Vanilla `ClientboundUpdateRecipesPacket` built from
    /// `getSynchronizedItemProperties` and `getSynchronizedStonecutterRecipes`.
    #[must_use]
    pub fn update_recipes_packet(&self) -> CUpdateRecipes {
        CUpdateRecipes {
            item_sets: self
                .property_sets
                .iter()
                .map(|(key, set)| RecipePropertySetEntry {
                    key: key.clone(),
                    items: set.network_ids(),
                })
                .collect(),
            stonecutter_recipes: self
                .stonecutter_recipes
                .entries
                .iter()
                .map(|recipe| StonecutterRecipeEntry {
                    input: ingredient_contents(&recipe.data().ingredient),
                    result: recipe.data().result.clone(),
                })
                .collect(),
        }
    }
}

/// Vanilla `Ingredient.CONTENTS_STREAM_CODEC` holder set.
fn ingredient_contents(ingredient: &Ingredient) -> RegistryHolderSet<Item> {
    match ingredient {
        Ingredient::Tag(tag) => RegistryHolderSet::Tag(tag.clone()),
        other => RegistryHolderSet::Direct(other.get_items()),
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, item_stack::ItemStack, vanilla_items};
    use steel_utils::serial::WriteTo as _;

    use super::{BREWING_INPUTS, FURNACE_INPUT, RECIPE_ACCESS, SMITHING_BASE, SMITHING_TEMPLATE};

    #[test]
    fn property_sets_collect_recipe_ingredients() {
        init_vanilla_registry();
        let access = &*RECIPE_ACCESS;
        assert!(
            access
                .property_set(&SMITHING_BASE)
                .test(&ItemStack::new(&vanilla_items::DIAMOND_SWORD))
        );
        assert!(
            access
                .property_set(&SMITHING_TEMPLATE)
                .test(&ItemStack::new(
                    &vanilla_items::COAST_ARMOR_TRIM_SMITHING_TEMPLATE
                ))
        );
        assert!(
            !access
                .property_set(&SMITHING_TEMPLATE)
                .test(&ItemStack::new(&vanilla_items::DIAMOND_SWORD))
        );
        assert!(
            access
                .property_set(&FURNACE_INPUT)
                .test(&ItemStack::new(&vanilla_items::RAW_IRON))
        );
        assert!(
            !access
                .property_set(&BREWING_INPUTS)
                .test(&ItemStack::new(&vanilla_items::POTION))
        );
    }

    #[test]
    fn update_recipes_packet_lists_every_set_and_stonecutter_recipe() {
        init_vanilla_registry();
        let packet = RECIPE_ACCESS.update_recipes_packet();
        assert_eq!(packet.item_sets.len(), 9);
        assert_eq!(packet.stonecutter_recipes.len(), 351);
        let mut bytes = Vec::new();
        packet
            .write(&mut bytes)
            .expect("recipe packet should encode");
        // VarInt map size, then the first key.
        assert_eq!(bytes[0], 9);
    }
}
