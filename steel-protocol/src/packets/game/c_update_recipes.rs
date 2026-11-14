//! Clientbound packet syncing the recipe data the client predicts menus with.

use std::io::{Result, Write};

use steel_macros::ClientPacket;
use steel_registry::{
    RegistryHolderSet, item_stack_template::ItemStackTemplate, items::Item,
    packets::play::C_UPDATE_RECIPES,
};
use steel_utils::{Identifier, codec::VarInt, serial::WriteTo};

/// Registration index of `minecraft:item_stack` in Vanilla's `SlotDisplays.bootstrap`.
const ITEM_STACK_SLOT_DISPLAY: i32 = 5;

/// One Vanilla `RecipePropertySet`: the items a menu slot accepts.
#[derive(Clone, Debug)]
pub struct RecipePropertySetEntry {
    /// Vanilla `RecipePropertySet` key, such as `minecraft:smithing_base`.
    pub key: Identifier,
    /// Raw item registry ids in the set.
    pub items: Vec<VarInt>,
}

impl WriteTo for RecipePropertySetEntry {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.key.write(writer)?;
        self.items.write(writer)
    }
}

/// Vanilla `SelectableRecipe.SingleInputEntry.noRecipeCodec()` for a stonecutter recipe:
/// the input ingredient and the result shown as an `item_stack` slot display.
#[derive(Clone, Debug)]
pub struct StonecutterRecipeEntry {
    /// Items the recipe accepts, as `Ingredient.CONTENTS_STREAM_CODEC` holder set.
    pub input: RegistryHolderSet<Item>,
    /// Vanilla `StonecutterRecipe.resultDisplay()` contents.
    pub result: ItemStackTemplate,
}

impl WriteTo for StonecutterRecipeEntry {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.input.write(writer)?;
        VarInt(ITEM_STACK_SLOT_DISPLAY).write(writer)?;
        self.result.write(writer)
    }
}

/// Vanilla `ClientboundUpdateRecipesPacket`.
#[derive(ClientPacket, Clone, Debug)]
#[packet_id(Play = C_UPDATE_RECIPES)]
pub struct CUpdateRecipes {
    /// Every synchronized recipe property set.
    pub item_sets: Vec<RecipePropertySetEntry>,
    /// Every selectable stonecutter recipe, in recipe manager order.
    pub stonecutter_recipes: Vec<StonecutterRecipeEntry>,
}

impl WriteTo for CUpdateRecipes {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.item_sets.write(writer)?;
        self.stonecutter_recipes.write(writer)
    }
}
