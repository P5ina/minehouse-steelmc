//! Vanilla villager trade data (`VillagerTrade`, `TradeCost`, `TradeSet`).
//!
//! The data is generated from the `villager_trade`, `trade_set` and
//! `tags/villager_trade` datapack folders into [`crate::vanilla_villager_trades`].
//! Evaluating a trade needs the world (enchanting, exploration maps, the merchant's
//! variant), so that lives in `steel-core`; this module only holds the data.

use steel_utils::Identifier;

/// Vanilla `ContextIntProvider`, limited to the providers vanilla trades use.
#[derive(Debug)]
pub enum TradeIntProvider {
    /// `ConstantValue`.
    Constant(i32),
    /// `UniformGenerator`: `Mth.nextInt(random, min, max)`.
    Uniform {
        min: &'static TradeIntProvider,
        max: &'static TradeIntProvider,
    },
    /// `BinomialDistributionGenerator`: `n` trials of `nextFloat() < p`.
    Binomial {
        n: &'static TradeIntProvider,
        p: f32,
    },
    /// `Sum` (`minecraft:add`).
    Sum(&'static [TradeIntProvider]),
}

/// Vanilla `TradeCost`: an item, a count and an exact component predicate.
#[derive(Debug)]
pub struct TradeCost {
    pub item: Identifier,
    pub count: TradeIntProvider,
    /// The only component vanilla costs require: an exact `potion_contents` potion.
    pub potion: Option<Identifier>,
}

/// `DataComponentMatchers` predicates used by the `filtered` trade modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemFilterPredicate {
    /// `minecraft:enchantments: [{}]`: at least one enchantment.
    Enchanted,
    /// `minecraft:stored_enchantments: [{}]`: at least one stored enchantment.
    StoredEnchanted,
    /// `minecraft:dyed_color: {}`: the component is present.
    DyedColor,
    /// `minecraft:map_id: {}`: the component is present.
    MapId,
}

/// The loot item functions vanilla uses in `given_item_modifier`.
#[derive(Debug)]
pub enum TradeItemModifier {
    /// `EnchantWithLevelsFunction`.
    EnchantWithLevels {
        levels: TradeIntProvider,
        /// Enchantment tag; `None` means every enchantment.
        options: Option<Identifier>,
        include_additional_cost_component: bool,
    },
    /// `EnchantRandomlyFunction`.
    EnchantRandomly {
        /// Enchantment tag; `None` means every enchantment.
        options: Option<Identifier>,
        only_compatible: bool,
        include_additional_cost_component: bool,
    },
    /// `ExplorationMapFunction`.
    ExplorationMap {
        /// Structure tag to look for.
        destination: Identifier,
        decoration: Identifier,
        zoom: i32,
        search_radius: i32,
        skip_existing_chunks: bool,
    },
    /// `SetPotionFunction`.
    SetPotion { id: Identifier },
    /// `SetRandomPotionFunction`. `options` is a potion tag; `None` means every potion.
    SetRandomPotion { options: Option<Identifier> },
    /// `SetStewEffectFunction`: effect id and duration in seconds.
    SetStewEffect {
        effects: &'static [(Identifier, TradeIntProvider)],
    },
    /// `SetRandomDyesFunction`.
    SetRandomDyes { number_of_dyes: TradeIntProvider },
    /// `DiscardItem`.
    Discard,
    /// `FilteredFunction`.
    Filtered {
        items: Option<Identifier>,
        predicates: &'static [ItemFilterPredicate],
        on_pass: Option<&'static TradeItemModifier>,
        on_fail: Option<&'static TradeItemModifier>,
    },
}

/// Vanilla `VillagerTrade`.
#[derive(Debug)]
pub struct VillagerTrade {
    pub key: Identifier,
    pub wants: TradeCost,
    pub additional_wants: Option<TradeCost>,
    pub gives_item: Identifier,
    pub gives_count: i32,
    pub max_uses: i32,
    pub xp: i32,
    pub reputation_discount: f32,
    /// `entity_properties` on `this` with `minecraft:villager/variant`: the merchant's
    /// villager type must be one of these.
    pub merchant_predicate: Option<&'static [Identifier]>,
    pub given_item_modifier: &'static [TradeItemModifier],
    /// Enchantment tag that doubles the additional trade cost of stored enchantments.
    pub double_trade_price_enchantments: Option<Identifier>,
}

/// Vanilla `TradeSet`, with its trade tag already resolved in vanilla order.
#[derive(Debug)]
pub struct TradeSet {
    pub key: Identifier,
    pub trades: &'static [&'static VillagerTrade],
    pub amount: TradeIntProvider,
    pub allow_duplicates: bool,
    pub random_sequence: Option<Identifier>,
}

/// Looks a vanilla trade set up by its key (for example `armorer/level_1`).
#[must_use]
pub fn trade_set(key: &Identifier) -> Option<&'static TradeSet> {
    crate::vanilla_villager_trades::TRADE_SETS
        .iter()
        .copied()
        .find(|set| &set.key == key)
}
