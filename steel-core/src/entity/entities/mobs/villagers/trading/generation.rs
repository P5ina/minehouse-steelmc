//! Vanilla `VillagerTrade.getOffer` and `AbstractVillager.addOffersFromTradeSet`.

use glam::DVec3;
use steel_registry::data_components::DataComponentMap;
use steel_registry::data_components::components::{
    DyedItemColor, PotionContents, SuspiciousStewEffect, SuspiciousStewEffects,
};
use steel_registry::data_components::vanilla_components::{
    ADDITIONAL_TRADE_COST, DYED_COLOR, ENCHANTMENTS, MAP_ID, POTION_CONTENTS, STORED_ENCHANTMENTS,
    SUSPICIOUS_STEW_EFFECTS,
};
use steel_registry::enchantment::EnchantmentRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::{ExplorationMapRequest, ExplorationMaps};
use steel_registry::potion::PotionRef;
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::villager_trade::{
    ItemFilterPredicate, TradeCost, TradeIntProvider, TradeItemModifier, TradeSet, VillagerTrade,
    trade_set,
};
use steel_registry::villager_type::VillagerTypeRef;
use steel_registry::{
    DyeColor, REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _, vanilla_items,
};
use steel_utils::Identifier;
use steel_utils::random::Random;

use super::offer::{ItemCost, MerchantOffer, MerchantOffers};
use crate::behavior::MOB_EFFECT_BEHAVIORS;
use crate::enchantment_helper::select_enchantment;

/// Vanilla `VillagerProfession.getTrades(level)`: every working profession maps
/// levels 1 to 5 to the `<profession>/level_<n>` trade set.
#[must_use]
pub fn profession_trade_set(
    profession: VillagerProfessionRef,
    level: i32,
) -> Option<&'static TradeSet> {
    if !(1..=5).contains(&level) || profession.key.namespace != "minecraft" {
        return None;
    }
    trade_set(&Identifier::vanilla(format!(
        "{}/level_{level}",
        profession.key.path
    )))
}

/// The `VILLAGER_TRADE` loot context: the merchant (`this`) and the random source
/// of the trade set's random sequence.
pub struct TradeContext<'a, R: Random> {
    /// The random source the trade rolls draw from.
    pub random: &'a mut R,
    /// The merchant's `villager/variant` component; `None` for a wandering trader.
    pub merchant_variant: Option<VillagerTypeRef>,
    /// Vanilla `ORIGIN`: the merchant's position.
    pub origin: DVec3,
    /// The level's structure search for exploration maps; without one, map trades
    /// find no structure and are dropped.
    pub exploration_maps: Option<&'a mut dyn ExplorationMaps>,
}

impl<R: Random> TradeContext<'_, R> {
    fn int(&mut self, provider: &TradeIntProvider) -> i32 {
        match provider {
            TradeIntProvider::Constant(value) => *value,
            TradeIntProvider::Uniform { min, max } => {
                let min = self.int(min);
                let max = self.int(max);
                next_int(self.random, min, max)
            }
            TradeIntProvider::Binomial { n, p } => {
                let n = self.int(n);
                let mut result = 0;
                for _ in 0..n {
                    if self.random.next_f32() < *p {
                        result += 1;
                    }
                }
                result
            }
            TradeIntProvider::Sum(inputs) => {
                let mut value: i64 = 0;
                for input in *inputs {
                    value += i64::from(self.int(input));
                }
                // Vanilla `ContextIntProvider.longToIntSafe`.
                value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
            }
        }
    }
}

/// Vanilla `Mth.nextInt`: `min` when the range is empty.
fn next_int(random: &mut impl Random, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        random.next_i32_bounded(max - min + 1) + min
    }
}

fn trade_cost_to_item_cost<R: Random>(
    cost: &TradeCost,
    ctx: &mut TradeContext<'_, R>,
    additional_cost: i32,
) -> Option<ItemCost> {
    let item = REGISTRY.items.by_key(&cost.item)?;
    let count = ctx
        .int(&cost.count)
        .saturating_add(additional_cost)
        .clamp(0, ItemStack::new(item).max_stack_size());
    let mut components = DataComponentMap::new();
    if let Some(potion) = &cost.potion {
        components.set(
            POTION_CONTENTS,
            Some(PotionContents::of(REGISTRY.potions.by_key(potion)?)),
        );
    }
    Some(ItemCost::new(item, count, components))
}

/// Vanilla `VillagerTrade.getOffer`.
#[must_use]
pub fn get_offer<R: Random>(
    trade: &VillagerTrade,
    ctx: &mut TradeContext<'_, R>,
) -> Option<MerchantOffer> {
    if let Some(variants) = trade.merchant_predicate {
        let matches = ctx
            .merchant_variant
            .is_some_and(|variant| variants.contains(&variant.key));
        if !matches {
            return None;
        }
    }
    let item = REGISTRY.items.by_key(&trade.gives_item)?;
    let mut result = ItemStack::with_count(item, trade.gives_count);
    let mut additional_cost = 0;
    if !trade.given_item_modifier.is_empty() {
        for modifier in trade.given_item_modifier {
            result = apply_modifier(modifier, result, ctx);
        }
        if result.is_empty() {
            return None;
        }
    }

    if let Some(cost) = result.get(ADDITIONAL_TRADE_COST).copied() {
        result.remove(ADDITIONAL_TRADE_COST);
        additional_cost += cost;
    }

    if let Some(tag) = &trade.double_trade_price_enchantments
        && let Some(stored) = result.get(STORED_ENCHANTMENTS)
        && stored.iter().any(|(key, _)| {
            REGISTRY
                .enchantments
                .by_key(key)
                .is_some_and(|enchantment| REGISTRY.enchantments.is_in_tag(enchantment, tag))
        })
    {
        additional_cost *= 2;
    }

    let item_cost = trade_cost_to_item_cost(&trade.wants, ctx, additional_cost)?;
    if item_cost.count() < 1 {
        return None;
    }
    let additional_item_cost = match &trade.additional_wants {
        Some(cost) => {
            let cost = trade_cost_to_item_cost(cost, ctx, 0)?;
            if cost.count() < 1 {
                return None;
            }
            Some(cost)
        }
        None => None,
    };
    Some(MerchantOffer::new(
        item_cost,
        additional_item_cost,
        result,
        trade.max_uses.max(1),
        trade.xp.max(0),
        trade.reputation_discount.max(0.0),
    ))
}

/// Vanilla `AbstractVillager.addOffersFromTradeSet`.
pub fn add_offers_from_trade_set<R: Random>(
    ctx: &mut TradeContext<'_, R>,
    offers: &mut MerchantOffers,
    trade_set: &TradeSet,
) {
    let number_of_offers = ctx.int(&trade_set.amount);
    if trade_set.allow_duplicates {
        add_offers_from_item_listings(ctx, offers, trade_set.trades, number_of_offers);
    } else {
        add_offers_from_item_listings_without_duplicates(
            ctx,
            offers,
            trade_set.trades,
            number_of_offers,
        );
    }
}

/// Vanilla `addOffersFromItemListings`: a trade that yields no offer is dropped.
fn add_offers_from_item_listings<R: Random>(
    ctx: &mut TradeContext<'_, R>,
    offers: &mut MerchantOffers,
    potential_offers: &[&'static VillagerTrade],
    number_of_offers: i32,
) {
    let mut potential: Vec<&VillagerTrade> = potential_offers.to_vec();
    let mut offers_found = 0;
    while offers_found < number_of_offers && !potential.is_empty() {
        let roll = ctx.random.next_i32_bounded(potential.len() as i32) as usize;
        match get_offer(potential[roll], ctx) {
            None => {
                potential.remove(roll);
            }
            Some(offer) => {
                offers.push(offer);
                offers_found += 1;
            }
        }
    }
}

/// Vanilla `addOffersFromItemListingsWithoutDuplicates`.
fn add_offers_from_item_listings_without_duplicates<R: Random>(
    ctx: &mut TradeContext<'_, R>,
    offers: &mut MerchantOffers,
    potential_offers: &[&'static VillagerTrade],
    number_of_offers: i32,
) {
    let mut leftover: Vec<&VillagerTrade> = potential_offers.to_vec();
    let mut offers_found = 0;
    while offers_found < number_of_offers && !leftover.is_empty() {
        let roll = ctx.random.next_i32_bounded(leftover.len() as i32) as usize;
        let trade = leftover.remove(roll);
        if let Some(offer) = get_offer(trade, ctx) {
            offers.push(offer);
            offers_found += 1;
        }
    }
}

fn enchantments_in(options: Option<&Identifier>) -> Vec<EnchantmentRef> {
    match options {
        Some(tag) => REGISTRY.enchantments.iter_tag(tag).collect(),
        None => (0..REGISTRY.enchantments.len())
            .filter_map(|id| REGISTRY.enchantments.by_id(id))
            .collect(),
    }
}

/// Vanilla `ItemStack.enchant` on a book: the book becomes an enchanted book first.
fn book_to_enchanted_book(item: ItemStack) -> ItemStack {
    if item.is(&vanilla_items::BOOK) {
        ItemStack::new(&vanilla_items::ENCHANTED_BOOK)
    } else {
        item
    }
}

/// Applies one `given_item_modifier` function (vanilla `LootItemFunction.apply`).
#[expect(
    clippy::too_many_lines,
    reason = "one arm per vanilla loot function, kept together like LootItemFunctions"
)]
fn apply_modifier<R: Random>(
    modifier: &TradeItemModifier,
    mut item: ItemStack,
    ctx: &mut TradeContext<'_, R>,
) -> ItemStack {
    match modifier {
        TradeItemModifier::EnchantWithLevels {
            levels,
            options,
            include_additional_cost_component,
        } => {
            // Vanilla `EnchantmentHelper.enchantItem`.
            let enchantment_cost = ctx.int(levels);
            let source = enchantments_in(options.as_ref());
            let enchants =
                select_enchantment(ctx.random, &item, enchantment_cost, source.into_iter());
            let mut result = book_to_enchanted_book(item);
            for enchant in enchants {
                result.upgrade_enchantment(enchant.enchantment.key.clone(), enchant.level);
            }
            // `ADDITIONAL_COST_COMPONENT_ALLOWED` is always present for trades.
            if *include_additional_cost_component && !result.is_empty() && enchantment_cost > 0 {
                result.set(ADDITIONAL_TRADE_COST, enchantment_cost);
            }
            result
        }
        TradeItemModifier::EnchantRandomly {
            options,
            only_compatible,
            include_additional_cost_component,
        } => {
            let target_is_book = item.is(&vanilla_items::BOOK);
            let check_compatibility = !target_is_book && *only_compatible;
            let compatible: Vec<EnchantmentRef> = enchantments_in(options.as_ref())
                .into_iter()
                .filter(|candidate| !check_compatibility || candidate.can_enchant(item.item()))
                .collect();
            if compatible.is_empty() {
                log::warn!("Couldn't find a compatible enchantment for {item:?}");
                return item;
            }
            let enchantment =
                compatible[ctx.random.next_i32_bounded(compatible.len() as i32) as usize];
            // Vanilla `EnchantRandomlyFunction.enchantItem`.
            let level = next_int(ctx.random, 1, enchantment.max_level as i32);
            let mut result = book_to_enchanted_book(item);
            result.upgrade_enchantment(enchantment.key.clone(), level as u32);
            if *include_additional_cost_component {
                let cost = 2 + ctx.random.next_i32_bounded(5 + level * 10) + 3 * level;
                result.set(ADDITIONAL_TRADE_COST, cost);
            }
            result
        }
        TradeItemModifier::ExplorationMap {
            destination,
            decoration,
            zoom,
            search_radius,
            skip_existing_chunks,
        } => {
            // Vanilla `ExplorationMapFunction.run`: without a structure the plain map
            // stays, which the `map_id` filter after every map trade then discards.
            if !item.is_empty()
                && let Some(maps) = ctx.exploration_maps.as_deref_mut()
            {
                maps.create_exploration_map(
                    &mut item,
                    ExplorationMapRequest {
                        destination: destination.clone(),
                        decoration: decoration.clone(),
                        zoom: *zoom,
                        search_radius: *search_radius,
                        skip_existing_chunks: *skip_existing_chunks,
                        origin: (ctx.origin.x, ctx.origin.y, ctx.origin.z),
                    },
                );
            }
            item
        }
        TradeItemModifier::SetPotion { id } => {
            if let Some(potion) = REGISTRY.potions.by_key(id) {
                set_potion(&mut item, potion);
            }
            item
        }
        TradeItemModifier::SetRandomPotion { options } => {
            let potions: Vec<_> = match options {
                Some(tag) => REGISTRY.potions.iter_tag(tag).collect(),
                None => (0..REGISTRY.potions.len())
                    .filter_map(|id| REGISTRY.potions.by_id(id))
                    .collect(),
            };
            if !potions.is_empty() {
                let potion = potions[ctx.random.next_i32_bounded(potions.len() as i32) as usize];
                set_potion(&mut item, potion);
            }
            item
        }
        TradeItemModifier::SetStewEffect { effects } => {
            if item.is(&vanilla_items::SUSPICIOUS_STEW) && !effects.is_empty() {
                let (effect_id, duration) =
                    &effects[ctx.random.next_i32_bounded(effects.len() as i32) as usize];
                let mut duration = ctx.int(duration);
                if let Some(effect) = REGISTRY.mob_effects.by_key(effect_id) {
                    if MOB_EFFECT_BEHAVIORS
                        .get_behavior(effect)
                        .as_instantaneous()
                        .is_none()
                    {
                        duration *= 20;
                    }
                    let mut entries = item
                        .get(SUSPICIOUS_STEW_EFFECTS)
                        .map_or_else(Vec::new, |current| current.effects().to_vec());
                    entries.push(SuspiciousStewEffect::new(effect, duration));
                    item.set(SUSPICIOUS_STEW_EFFECTS, SuspiciousStewEffects::new(entries));
                }
            }
            item
        }
        TradeItemModifier::SetRandomDyes { number_of_dyes } => {
            let rolls = ctx.int(number_of_dyes);
            if rolls <= 0 {
                return item;
            }
            let dyes: Vec<DyeColor> = (0..rolls)
                .map(|_| {
                    DyeColor::VALUES
                        [ctx.random.next_i32_bounded(DyeColor::VALUES.len() as i32) as usize]
                })
                .collect();
            // Vanilla `DyedItemColor.applyDyes(ItemStack, List)`.
            let color = DyedItemColor::apply_dyes(item.get(DYED_COLOR).copied(), &dyes);
            let mut result = item.copy_with_count(1);
            result.set(DYED_COLOR, color);
            result
        }
        TradeItemModifier::Discard => ItemStack::empty(),
        TradeItemModifier::Filtered {
            items,
            predicates,
            on_pass,
            on_fail,
        } => {
            let branch = if filter_matches(&item, items.as_ref(), predicates) {
                on_pass
            } else {
                on_fail
            };
            match branch {
                Some(function) => apply_modifier(function, item, ctx),
                None => item,
            }
        }
    }
}

/// Vanilla `PotionContents.withPotion` through `ItemStack.update`.
fn set_potion(item: &mut ItemStack, potion: PotionRef) {
    let current = item
        .get(POTION_CONTENTS)
        .cloned()
        .unwrap_or_else(PotionContents::empty);
    item.set(
        POTION_CONTENTS,
        PotionContents::new(
            Some(RegistryReference::new(potion)),
            current.custom_color(),
            current.custom_effects().to_vec(),
            current.custom_name().map(str::to_owned),
        ),
    );
}

/// Vanilla `ItemPredicate.test` for the `filtered` trade modifiers.
fn filter_matches(
    item: &ItemStack,
    items: Option<&Identifier>,
    predicates: &[ItemFilterPredicate],
) -> bool {
    if let Some(expected) = items
        && item.item().key != *expected
    {
        return false;
    }
    predicates.iter().all(|predicate| match predicate {
        ItemFilterPredicate::Enchanted => item.get(ENCHANTMENTS).is_some_and(|e| !e.is_empty()),
        ItemFilterPredicate::StoredEnchanted => {
            item.get(STORED_ENCHANTMENTS).is_some_and(|e| !e.is_empty())
        }
        ItemFilterPredicate::DyedColor => item.has(DYED_COLOR),
        ItemFilterPredicate::MapId => item.has(MAP_ID),
    })
}
