//! Generator for the vanilla brewing serializer.

use proc_macro2::TokenStream;
use quote::quote;
use serde_json::Value;

use super::shared::{field, identifier_tokens, ingredient_tokens, result_tokens, vanilla_ident};

/// Vanilla `PotionIngredient.MAP_CODEC`: `item` plus optional `potion_contents`.
fn potion_ingredient_tokens(value: &Value) -> TokenStream {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("Potion ingredient is not an object: {value}"));
    for key in object.keys() {
        assert!(
            key == "item" || key == "potion_contents",
            "Unsupported potion ingredient field {key} in {value}"
        );
    }
    let ingredient = ingredient_tokens(field(value, "item"));
    let potions = value.get("potion_contents").map_or_else(
        || quote! { None },
        |predicate| {
            let predicate = predicate
                .as_object()
                .unwrap_or_else(|| panic!("Potions predicate is not an object: {predicate}"));
            for key in predicate.keys() {
                assert!(
                    key == "potions",
                    "Unsupported potions predicate field {key}"
                );
            }
            predicate.get("potions").map_or_else(
                || quote! { None },
                |potions| {
                    let set = potion_holder_set_tokens(potions);
                    quote! { Some(#set) }
                },
            )
        },
    );
    quote! { PotionIngredient { ingredient: #ingredient, potions: #potions } }
}

/// `RegistryCodecs.holderSet(Registries.POTION)`: a tag, one id or a list.
fn potion_holder_set_tokens(value: &Value) -> TokenStream {
    let potion = |id: &str| {
        let ident = vanilla_ident(id);
        quote! { &vanilla_potions::#ident }
    };
    match value {
        Value::String(id) => {
            if let Some(tag) = id.strip_prefix('#') {
                let identifier = identifier_tokens(tag);
                quote! { RegistryHolderSet::Tag(#identifier) }
            } else {
                let potion = potion(id);
                quote! { RegistryHolderSet::Direct(vec![#potion]) }
            }
        }
        Value::Array(ids) => {
            let potions = ids.iter().map(|id| {
                potion(
                    id.as_str()
                        .unwrap_or_else(|| panic!("Potion id is not a string: {id}")),
                )
            });
            quote! { RegistryHolderSet::Direct(vec![#(#potions),*]) }
        }
        other => panic!("Unsupported potion holder set {other}"),
    }
}

pub(super) fn generate(value: &Value) -> TokenStream {
    let input = potion_ingredient_tokens(field(value, "input"));
    let reagent = potion_ingredient_tokens(field(value, "reagent"));
    let output = result_tokens(field(value, "output"));
    // Vanilla `BrewingRecipe` is special and has `PlacementInfo.NOT_PLACEABLE`.
    quote! {
        BrewingRecipe {
            properties: RecipeProperties {
                special: true,
                placement: Some(PlacementInfo::not_placeable()),
                recipe_book: None,
            },
            input: #input,
            reagent: #reagent,
            output: #output,
        }
    }
}
