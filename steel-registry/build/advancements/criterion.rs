use crate::generator_functions::generate_identifier;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use steel_utils::Identifier;

#[derive(Deserialize)]
pub(crate) struct CriterionJson {
    trigger: Identifier,
    #[serde(default)]
    conditions: Value,
}

impl ToTokens for CriterionJson {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let trigger = generate_identifier(&self.trigger);
        let conditions = if self.conditions.is_null() {
            "{}".to_owned()
        } else {
            self.conditions.to_string()
        };
        tokens.extend(quote! {
            Criterion::new(#trigger, #conditions)
        });
    }
}

pub(crate) fn parse_criteria(list: &BTreeMap<String, CriterionJson>) -> TokenStream {
    let criteria = list.iter().map(|(k, v)| {
        quote! { (#k.to_owned(), #v) }
    });
    quote! {
        BTreeMap::from([#(#criteria),*])
    }
}
