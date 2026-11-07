//! Build script for vanilla villager trades (`villager_trade`), trade sets (`trade_set`)
//! and the `villager_trade` tags that trade sets reference.
//!
//! Only the shapes that occur in the vanilla datapack are modeled; anything else panics
//! so that a data update fails loudly instead of silently dropping trades.

use std::{fs, path::Path};

use heck::ToShoutySnakeCase;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use rustc_hash::FxHashMap;
use serde::Deserialize;

const DATAPACK: &str = "../steel-utils/build_assets/builtin_datapacks/minecraft";

/// `ContextIntProviders.CODEC`: an inline constant or a typed provider object.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum IntProviderJson {
    Constant(f64),
    Object {
        #[serde(rename = "type")]
        provider_type: String,
        #[serde(default)]
        min: Option<Box<IntProviderJson>>,
        #[serde(default)]
        max: Option<Box<IntProviderJson>>,
        #[serde(default)]
        n: Option<Box<IntProviderJson>>,
        #[serde(default)]
        p: Option<f32>,
        #[serde(default)]
        inputs: Option<Vec<IntProviderJson>>,
    },
}

/// `ItemStackTemplate` as used by `gives`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct GivesJson {
    id: String,
    #[serde(default)]
    count: Option<i32>,
}

/// Vanilla `TradeCost`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct TradeCostJson {
    id: String,
    #[serde(default)]
    count: Option<IntProviderJson>,
    #[serde(default)]
    components: Option<CostComponentsJson>,
}

/// `DataComponentExactPredicate` on a cost. Vanilla only ever requires an exact potion.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct CostComponentsJson {
    #[serde(rename = "minecraft:potion_contents")]
    potion_contents: PotionContentsJson,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct PotionContentsJson {
    potion: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum ModifierJson {
    Single(Box<FunctionJson>),
    Sequence(Vec<FunctionJson>),
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct FunctionJson {
    #[serde(rename = "type")]
    function: String,
    // enchant_with_levels
    #[serde(default)]
    levels: Option<IntProviderJson>,
    // enchant_with_levels / enchant_randomly / set_random_potion
    #[serde(default)]
    options: Option<String>,
    #[serde(default)]
    include_additional_cost_component: Option<bool>,
    // enchant_randomly
    #[serde(default)]
    only_compatible: Option<bool>,
    // exploration_map
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    decoration: Option<String>,
    #[serde(default)]
    zoom: Option<i32>,
    #[serde(default)]
    search_radius: Option<i32>,
    #[serde(default)]
    skip_existing_chunks: Option<bool>,
    // set_potion
    #[serde(default)]
    id: Option<String>,
    // set_stew_effect
    #[serde(default)]
    effects: Option<Vec<StewEffectJson>>,
    // set_random_dyes
    #[serde(default)]
    number_of_dyes: Option<IntProviderJson>,
    // filtered
    #[serde(default)]
    item_filter: Option<ItemFilterJson>,
    #[serde(default)]
    on_pass: Option<Box<FunctionJson>>,
    #[serde(default)]
    on_fail: Option<Box<FunctionJson>>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct StewEffectJson {
    #[serde(rename = "type")]
    effect: String,
    duration: IntProviderJson,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct ItemFilterJson {
    #[serde(default)]
    items: Option<String>,
    #[serde(default)]
    predicates: FxHashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct MerchantPredicateJson {
    #[serde(rename = "type")]
    condition: String,
    entity: String,
    predicate: MerchantEntityPredicateJson,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct MerchantEntityPredicateJson {
    #[serde(rename = "minecraft:predicates")]
    predicates: MerchantSubPredicatesJson,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct MerchantSubPredicatesJson {
    #[serde(rename = "minecraft:villager/variant")]
    villager_variant: HolderSetJson,
}

/// A registry holder set: a single id or a list of ids.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum HolderSetJson {
    Single(String),
    List(Vec<String>),
}

impl HolderSetJson {
    fn ids(&self) -> Vec<&str> {
        match self {
            Self::Single(id) => {
                assert!(
                    !id.starts_with('#'),
                    "villager variant tags are not modeled: {id}"
                );
                vec![id.as_str()]
            }
            Self::List(ids) => ids.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct VillagerTradeJson {
    wants: TradeCostJson,
    #[serde(default)]
    additional_wants: Option<TradeCostJson>,
    gives: GivesJson,
    #[serde(default)]
    max_uses: Option<IntProviderJson>,
    #[serde(default)]
    xp: Option<IntProviderJson>,
    #[serde(default)]
    reputation_discount: Option<f32>,
    #[serde(default)]
    merchant_predicate: Option<MerchantPredicateJson>,
    #[serde(default)]
    given_item_modifier: Option<ModifierJson>,
    #[serde(default)]
    double_trade_price_enchantments: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct TradeSetJson {
    trades: String,
    amount: IntProviderJson,
    #[serde(default)]
    allow_duplicates: bool,
    #[serde(default)]
    random_sequence: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct TagJson {
    values: Vec<String>,
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:")
        .unwrap_or_else(|| panic!("expected a minecraft: identifier, got {id:?}"))
}

fn strip_tag(id: &str) -> &str {
    strip(
        id.strip_prefix('#')
            .unwrap_or_else(|| panic!("expected a #tag reference, got {id:?}")),
    )
}

fn vanilla_id(id: &str) -> TokenStream {
    let path = strip(id);
    quote! { Identifier::vanilla_static(#path) }
}

fn tag_id(id: &str) -> TokenStream {
    let path = strip_tag(id);
    quote! { Identifier::vanilla_static(#path) }
}

fn const_ident(prefix: &str, key: &str) -> Ident {
    Ident::new(
        &format!("{prefix}_{}", key.replace('/', "_")).to_shouty_snake_case(),
        Span::call_site(),
    )
}

fn constant_int(provider: &IntProviderJson, what: &str) -> i32 {
    match provider {
        IntProviderJson::Constant(value) => *value as i32,
        IntProviderJson::Object { .. } => panic!("{what} must be a constant, got {provider:?}"),
    }
}

fn int_provider(provider: &IntProviderJson) -> TokenStream {
    match provider {
        IntProviderJson::Constant(value) => {
            let value = *value as i32;
            quote! { TradeIntProvider::Constant(#value) }
        }
        IntProviderJson::Object {
            provider_type,
            min,
            max,
            n,
            p,
            inputs,
        } => match provider_type.as_str() {
            "minecraft:uniform" => {
                let min = int_provider(min.as_deref().expect("uniform needs min"));
                let max = int_provider(max.as_deref().expect("uniform needs max"));
                quote! { TradeIntProvider::Uniform { min: &#min, max: &#max } }
            }
            "minecraft:binomial" => {
                let n = int_provider(n.as_deref().expect("binomial needs n"));
                let p = p.expect("binomial needs p");
                quote! { TradeIntProvider::Binomial { n: &#n, p: #p } }
            }
            "minecraft:add" => {
                let inputs: Vec<TokenStream> = inputs
                    .as_ref()
                    .expect("add needs inputs")
                    .iter()
                    .map(int_provider)
                    .collect();
                quote! { TradeIntProvider::Sum(&[#(#inputs),*]) }
            }
            other => panic!("unsupported villager trade int provider {other}"),
        },
    }
}

fn trade_cost(cost: &TradeCostJson) -> TokenStream {
    let item = vanilla_id(&cost.id);
    let count = cost
        .count
        .as_ref()
        .map_or_else(|| quote! { TradeIntProvider::Constant(1) }, int_provider);
    let potion = cost.components.as_ref().map_or_else(
        || quote! { None },
        |components| {
            let potion = vanilla_id(&components.potion_contents.potion);
            quote! { Some(#potion) }
        },
    );
    quote! {
        TradeCost { item: #item, count: #count, potion: #potion }
    }
}

fn filter_predicate(key: &str, value: &serde_json::Value) -> TokenStream {
    let is_any_entry_list = value.as_array().is_some_and(|list| {
        list.len() == 1 && list[0].as_object().is_some_and(serde_json::Map::is_empty)
    });
    let is_presence = value.as_object().is_some_and(serde_json::Map::is_empty);
    match key {
        "minecraft:enchantments" if is_any_entry_list => quote! { ItemFilterPredicate::Enchanted },
        "minecraft:stored_enchantments" if is_any_entry_list => {
            quote! { ItemFilterPredicate::StoredEnchanted }
        }
        "minecraft:dyed_color" if is_presence => quote! { ItemFilterPredicate::DyedColor },
        "minecraft:map_id" if is_presence => quote! { ItemFilterPredicate::MapId },
        _ => panic!("unsupported villager trade item_filter predicate {key}: {value}"),
    }
}

fn function(function: &FunctionJson) -> TokenStream {
    match function.function.as_str() {
        "minecraft:enchant_with_levels" => {
            let levels = int_provider(function.levels.as_ref().expect("levels"));
            let options = optional_tag(function.options.as_deref());
            let cost = function.include_additional_cost_component.unwrap_or(false);
            quote! {
                TradeItemModifier::EnchantWithLevels {
                    levels: #levels,
                    options: #options,
                    include_additional_cost_component: #cost,
                }
            }
        }
        "minecraft:enchant_randomly" => {
            let options = optional_tag(function.options.as_deref());
            let only_compatible = function.only_compatible.unwrap_or(true);
            let cost = function.include_additional_cost_component.unwrap_or(false);
            quote! {
                TradeItemModifier::EnchantRandomly {
                    options: #options,
                    only_compatible: #only_compatible,
                    include_additional_cost_component: #cost,
                }
            }
        }
        "minecraft:exploration_map" => {
            let destination = tag_id(function.destination.as_deref().expect("destination"));
            let decoration = vanilla_id(
                function
                    .decoration
                    .as_deref()
                    .unwrap_or("minecraft:mansion"),
            );
            let zoom = function.zoom.unwrap_or(2);
            let search_radius = function.search_radius.unwrap_or(50);
            let skip = function.skip_existing_chunks.unwrap_or(true);
            quote! {
                TradeItemModifier::ExplorationMap {
                    destination: #destination,
                    decoration: #decoration,
                    zoom: #zoom,
                    search_radius: #search_radius,
                    skip_existing_chunks: #skip,
                }
            }
        }
        "minecraft:set_potion" => {
            let id = vanilla_id(function.id.as_deref().expect("set_potion id"));
            quote! { TradeItemModifier::SetPotion { id: #id } }
        }
        "minecraft:set_random_potion" => {
            let options = optional_tag(function.options.as_deref());
            quote! { TradeItemModifier::SetRandomPotion { options: #options } }
        }
        "minecraft:set_stew_effect" => {
            let effects: Vec<TokenStream> = function
                .effects
                .as_ref()
                .expect("set_stew_effect effects")
                .iter()
                .map(|effect| {
                    let id = vanilla_id(&effect.effect);
                    let duration = int_provider(&effect.duration);
                    quote! { (#id, #duration) }
                })
                .collect();
            quote! { TradeItemModifier::SetStewEffect { effects: &[#(#effects),*] } }
        }
        "minecraft:set_random_dyes" => {
            let dyes = int_provider(function.number_of_dyes.as_ref().expect("number_of_dyes"));
            quote! { TradeItemModifier::SetRandomDyes { number_of_dyes: #dyes } }
        }
        "minecraft:discard" => quote! { TradeItemModifier::Discard },
        "minecraft:filtered" => {
            let filter = function.item_filter.as_ref().expect("item_filter");
            let items = filter.items.as_deref().map_or_else(
                || quote! { None },
                |id| {
                    let id = vanilla_id(id);
                    quote! { Some(#id) }
                },
            );
            let mut predicates: Vec<(&String, &serde_json::Value)> =
                filter.predicates.iter().collect();
            predicates.sort_by(|a, b| a.0.cmp(b.0));
            let predicates: Vec<TokenStream> = predicates
                .into_iter()
                .map(|(key, value)| filter_predicate(key, value))
                .collect();
            let on_pass = nested(function.on_pass.as_deref());
            let on_fail = nested(function.on_fail.as_deref());
            quote! {
                TradeItemModifier::Filtered {
                    items: #items,
                    predicates: &[#(#predicates),*],
                    on_pass: #on_pass,
                    on_fail: #on_fail,
                }
            }
        }
        other => panic!("unsupported villager trade item modifier {other}"),
    }
}

fn nested(function_json: Option<&FunctionJson>) -> TokenStream {
    function_json.map_or_else(
        || quote! { None },
        |f| {
            let inner = function(f);
            quote! { Some(&#inner) }
        },
    )
}

fn optional_tag(options: Option<&str>) -> TokenStream {
    options.map_or_else(
        || quote! { None },
        |tag| {
            let tag = tag_id(tag);
            quote! { Some(#tag) }
        },
    )
}

fn read_json_tree(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            read_json_tree(&path, base, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let key = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            out.push((key, content));
        }
    }
}

/// Resolves a `villager_trade` tag, expanding nested tags in place and keeping the
/// first occurrence of each trade (vanilla builds tags into a `LinkedHashSet`).
fn resolve_tag(
    tag: &str,
    tags: &FxHashMap<String, TagJson>,
    out: &mut Vec<String>,
    stack: &mut Vec<String>,
) {
    assert!(
        !stack.iter().any(|t| t == tag),
        "cyclic villager_trade tag {tag}"
    );
    let json = tags
        .get(tag)
        .unwrap_or_else(|| panic!("missing villager_trade tag {tag}"));
    stack.push(tag.to_string());
    for value in &json.values {
        if value.starts_with('#') {
            resolve_tag(strip_tag(value), tags, out, stack);
        } else {
            let key = strip(value).to_string();
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    stack.pop();
}

pub(crate) fn build() -> TokenStream {
    let trade_dir = format!("{DATAPACK}/villager_trade");
    let set_dir = format!("{DATAPACK}/trade_set");
    let tag_dir = format!("{DATAPACK}/tags/villager_trade");
    println!("cargo:rerun-if-changed={trade_dir}");
    println!("cargo:rerun-if-changed={set_dir}");
    println!("cargo:rerun-if-changed={tag_dir}");

    let mut trade_files = Vec::new();
    read_json_tree(
        Path::new(&trade_dir),
        Path::new(&trade_dir),
        &mut trade_files,
    );
    let mut set_files = Vec::new();
    read_json_tree(Path::new(&set_dir), Path::new(&set_dir), &mut set_files);
    let mut tag_files = Vec::new();
    read_json_tree(Path::new(&tag_dir), Path::new(&tag_dir), &mut tag_files);

    let tags: FxHashMap<String, TagJson> = tag_files
        .iter()
        .map(|(key, content)| {
            let json: TagJson = serde_json::from_str(content).unwrap_or_else(|error| {
                panic!("failed to parse villager_trade tag {key}: {error}")
            });
            (key.clone(), json)
        })
        .collect();

    let mut stream = TokenStream::new();
    stream.extend(quote! {
        use crate::villager_trade::{
            ItemFilterPredicate, TradeCost, TradeIntProvider, TradeItemModifier, TradeSet,
            VillagerTrade,
        };
        use steel_utils::Identifier;
    });

    let mut trade_keys = Vec::new();
    for (key, content) in &trade_files {
        let trade: VillagerTradeJson = serde_json::from_str(content)
            .unwrap_or_else(|error| panic!("failed to parse villager_trade {key}: {error}"));
        let ident = const_ident("trade", key);
        let wants = trade_cost(&trade.wants);
        let additional_wants = trade.additional_wants.as_ref().map_or_else(
            || quote! { None },
            |cost| {
                let cost = trade_cost(cost);
                quote! { Some(#cost) }
            },
        );
        let gives_item = vanilla_id(&trade.gives.id);
        let gives_count = trade.gives.count.unwrap_or(1);
        let max_uses = trade
            .max_uses
            .as_ref()
            .map_or(4, |p| constant_int(p, "max_uses"));
        let xp = trade.xp.as_ref().map_or(1, |p| constant_int(p, "xp"));
        let reputation_discount = trade.reputation_discount.unwrap_or(0.0);
        let merchant_predicate = trade.merchant_predicate.as_ref().map_or_else(
            || quote! { None },
            |predicate| {
                assert_eq!(predicate.condition, "minecraft:entity_properties");
                assert_eq!(predicate.entity, "this");
                let variants: Vec<TokenStream> = predicate
                    .predicate
                    .predicates
                    .villager_variant
                    .ids()
                    .into_iter()
                    .map(vanilla_id)
                    .collect();
                quote! { Some(&[#(#variants),*]) }
            },
        );
        let modifiers: Vec<TokenStream> = match &trade.given_item_modifier {
            None => Vec::new(),
            Some(ModifierJson::Single(f)) => vec![function(f)],
            Some(ModifierJson::Sequence(fs)) => fs.iter().map(function).collect(),
        };
        let double_price = trade
            .double_trade_price_enchantments
            .as_deref()
            .map_or_else(
                || quote! { None },
                |tag| {
                    let tag = tag_id(tag);
                    quote! { Some(#tag) }
                },
            );

        stream.extend(quote! {
            pub static #ident: VillagerTrade = VillagerTrade {
                key: Identifier::vanilla_static(#key),
                wants: #wants,
                additional_wants: #additional_wants,
                gives_item: #gives_item,
                gives_count: #gives_count,
                max_uses: #max_uses,
                xp: #xp,
                reputation_discount: #reputation_discount,
                merchant_predicate: #merchant_predicate,
                given_item_modifier: &[#(#modifiers),*],
                double_trade_price_enchantments: #double_price,
            };
        });
        trade_keys.push(key.clone());
    }

    let mut set_idents = Vec::new();
    for (key, content) in &set_files {
        let set: TradeSetJson = serde_json::from_str(content)
            .unwrap_or_else(|error| panic!("failed to parse trade_set {key}: {error}"));
        let mut resolved = Vec::new();
        resolve_tag(
            strip_tag(&set.trades),
            &tags,
            &mut resolved,
            &mut Vec::new(),
        );
        let trades: Vec<TokenStream> = resolved
            .iter()
            .map(|trade| {
                assert!(
                    trade_keys.contains(trade),
                    "trade_set {key} references missing trade {trade}"
                );
                let ident = const_ident("trade", trade);
                quote! { &#ident }
            })
            .collect();
        let amount = int_provider(&set.amount);
        let allow_duplicates = set.allow_duplicates;
        let random_sequence = set.random_sequence.as_deref().map_or_else(
            || quote! { None },
            |id| {
                let id = vanilla_id(id);
                quote! { Some(#id) }
            },
        );
        let ident = const_ident("trade_set", key);
        stream.extend(quote! {
            pub static #ident: TradeSet = TradeSet {
                key: Identifier::vanilla_static(#key),
                trades: &[#(#trades),*],
                amount: #amount,
                allow_duplicates: #allow_duplicates,
                random_sequence: #random_sequence,
            };
        });
        set_idents.push(ident);
    }

    stream.extend(quote! {
        /// Every vanilla trade set, in key order.
        pub static TRADE_SETS: &[&TradeSet] = &[#(&#set_idents),*];
    });

    stream
}
