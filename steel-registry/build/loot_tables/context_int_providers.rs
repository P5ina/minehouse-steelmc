//! Build script for the vanilla `context_int_provider/compostable` datapack
//! folder. Only the provider shapes that folder uses are modeled; anything
//! else panics so that a data update fails loudly.

use std::fs;

use serde::Deserialize;

use super::{ConditionRefJson, TokenStream, generate_condition, quote};

const FOLDER: &str =
    "../steel-utils/build_assets/builtin_datapacks/minecraft/context_int_provider/compostable";

/// `ContextIntProviders.CODEC`: an inline constant or a typed provider object.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ProviderJson {
    Constant(i32),
    Object(Box<ProviderObjectJson>),
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ProviderObjectJson {
    #[serde(rename = "type")]
    provider_type: String,
    #[serde(default)]
    cases: Vec<CaseJson>,
    #[serde(default)]
    default: Option<ProviderJson>,
    #[serde(default)]
    distribution: Vec<WeightedJson>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct CaseJson {
    condition: ConditionRefJson,
    value: ProviderJson,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct WeightedJson {
    data: ProviderJson,
    weight: i32,
}

fn generate_provider(provider: &ProviderJson) -> TokenStream {
    match provider {
        ProviderJson::Constant(value) => quote! { ContextIntProvider::Constant(#value) },
        ProviderJson::Object(object) => match object.provider_type.as_str() {
            "minecraft:number_dispatcher" => {
                let cases = object.cases.iter().map(|case| {
                    let condition = generate_condition(&case.condition);
                    let value = generate_provider(&case.value);
                    quote! { DispatcherCase { condition: #condition, value: #value } }
                });
                // Vanilla `DispatcherProvider.mapCodec` defaults to `exactly(0)`.
                let default = object.default.as_ref().map_or_else(
                    || quote! { ContextIntProvider::Constant(0) },
                    generate_provider,
                );
                quote! {
                    ContextIntProvider::NumberDispatcher {
                        cases: &[#(#cases),*],
                        default: &#default,
                    }
                }
            }
            "minecraft:weighted_list" => {
                let entries = object.distribution.iter().map(|entry| {
                    let data = generate_provider(&entry.data);
                    let weight = entry.weight;
                    quote! { WeightedEntry { data: #data, weight: #weight } }
                });
                quote! { ContextIntProvider::WeightedList(&[#(#entries),*]) }
            }
            other => panic!("unsupported compostable context int provider type {other}"),
        },
    }
}

pub(crate) fn build() -> TokenStream {
    println!("cargo:rerun-if-changed={FOLDER}");
    let mut files: Vec<_> = fs::read_dir(FOLDER)
        .unwrap_or_else(|e| panic!("Failed to read {FOLDER}: {e}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("Failed to read entry: {e}"))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    let providers = files.iter().map(|path| {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("Bad file name {}", path.display()));
        let key = format!("compostable/{name}");
        let content = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
        let json: ProviderJson = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()));
        let provider = generate_provider(&json);
        quote! {
            NamedContextIntProvider {
                key: Identifier::vanilla_static(#key),
                provider: #provider,
            }
        }
    });

    quote! {
        use steel_utils::Identifier;

        use crate::context_int_provider::{
            ContextIntProvider, DispatcherCase, NamedContextIntProvider, WeightedEntry,
        };
        use crate::loot_table::{LootCondition, PropertyCheck};

        /// The `context_int_provider/compostable` providers of the vanilla datapack.
        pub static COMPOSTABLE: &[NamedContextIntProvider] = &[#(#providers),*];

        /// The compostable provider named `key`.
        #[must_use]
        pub fn get(key: &Identifier) -> Option<&'static ContextIntProvider> {
            COMPOSTABLE
                .iter()
                .find(|named| named.key == *key)
                .map(|named| &named.provider)
        }
    }
}
