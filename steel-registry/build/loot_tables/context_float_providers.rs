//! Build script for the vanilla `context_float_provider/brewing` datapack
//! folder. Only the provider shapes that folder uses are modeled; anything
//! else panics so that a data update fails loudly.

use std::fs;

use serde::Deserialize;

use super::{TokenStream, quote};

const FOLDER: &str =
    "../steel-utils/build_assets/builtin_datapacks/minecraft/context_float_provider/brewing";

/// `ContextFloatProviders.CODEC`, limited to inline constants.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ProviderJson {
    Constant(f32),
}

fn generate_provider(provider: &ProviderJson) -> TokenStream {
    match provider {
        ProviderJson::Constant(value) => quote! { ContextFloatProvider::Constant(#value) },
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
        let key = format!("brewing/{name}");
        let content = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
        let json: ProviderJson = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("Unsupported float provider {}: {e}", path.display()));
        let provider = generate_provider(&json);
        quote! {
            NamedContextFloatProvider {
                key: Identifier::vanilla_static(#key),
                provider: #provider,
            }
        }
    });

    quote! {
        use steel_utils::Identifier;

        use crate::context_float_provider::{ContextFloatProvider, NamedContextFloatProvider};

        /// The `context_float_provider/brewing` providers of the vanilla datapack.
        pub static BREWING: &[NamedContextFloatProvider] = &[#(#providers),*];

        /// The vanilla provider named `key`.
        #[must_use]
        pub fn get(key: &Identifier) -> Option<&'static ContextFloatProvider> {
            BREWING
                .iter()
                .find(|named| named.key == *key)
                .map(|named| &named.provider)
        }
    }
}
