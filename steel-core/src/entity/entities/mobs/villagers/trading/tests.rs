use std::io::Cursor;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::{NbtCompound, NbtList};
use steel_registry::data_components::DataComponentMap;
use steel_registry::data_components::components::PotionContents;
use steel_registry::data_components::vanilla_components::{
    ADDITIONAL_TRADE_COST, DYED_COLOR, POTION_CONTENTS, STORED_ENCHANTMENTS,
    SUSPICIOUS_STEW_EFFECTS,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_villager_trades::{
    TRADE_ARMORER_1_EMERALD_IRON_HELMET,
    TRADE_CARTOGRAPHER_2_EMERALD_AND_COMPASS_EXPLORER_JUNGLE_MAP,
    TRADE_FARMER_4_EMERALD_SUSPICIOUS_STEW, TRADE_FLETCHER_5_ARROW_AND_EMERALD_TIPPED_ARROW,
    TRADE_LEATHERWORKER_1_EMERALD_DYED_LEATHER_CHESTPLATE,
    TRADE_LIBRARIAN_1_EMERALD_AND_BOOK_ENCHANTED_BOOK, TRADE_SETS,
    TRADE_WANDERING_TRADER_WATER_BOTTLE_EMERALD,
};
use steel_registry::villager_trade::trade_set;
use steel_registry::{
    init_vanilla_registry, vanilla_entities, vanilla_items, vanilla_potions,
    vanilla_villager_professions, vanilla_villager_types,
};
use steel_utils::Identifier;
use steel_utils::random::legacy_random::LegacyRandom;

use super::{
    ItemCost, MerchantOffer, MerchantOffers, TradeContext, add_offers_from_trade_set, get_offer,
    profession_trade_set,
};
use crate::behavior::init_behaviors;
use crate::entity::entities::mobs::villagers::VillagerEntity;
use crate::entity::{Entity as _, init_entities};

const SEEDS: u64 = 64;

fn plains_ctx(random: &mut LegacyRandom) -> TradeContext<'_, LegacyRandom> {
    TradeContext {
        random,
        merchant_variant: Some(&vanilla_villager_types::PLAINS),
    }
}

fn emeralds(count: i32) -> ItemCost {
    ItemCost::new(&vanilla_items::EMERALD, count, DataComponentMap::new())
}

#[test]
fn trade_sets_resolve_their_tags_in_vanilla_order() {
    init_vanilla_registry();
    assert_eq!(TRADE_SETS.len(), 68);
    let armorer = trade_set(&Identifier::vanilla_static("armorer/level_1"))
        .expect("armorer level 1 trade set");
    let keys: Vec<String> = armorer
        .trades
        .iter()
        .map(|t| t.key.path.to_string())
        .collect();
    assert_eq!(
        keys,
        [
            "smith/1/coal_emerald",
            "armorer/1/emerald_iron_leggings",
            "armorer/1/emerald_iron_boots",
            "armorer/1/emerald_iron_helmet",
            "armorer/1/emerald_iron_chestplate",
        ]
    );
    assert!(TRADE_SETS.iter().all(|set| !set.trades.is_empty()));
}

#[test]
fn professions_map_levels_one_to_five_to_trade_sets() {
    init_vanilla_registry();
    let set = profession_trade_set(&vanilla_villager_professions::LIBRARIAN, 3)
        .expect("librarian level 3");
    assert_eq!(set.key.path.as_ref(), "librarian/level_3");
    assert!(profession_trade_set(&vanilla_villager_professions::LIBRARIAN, 6).is_none());
    assert!(profession_trade_set(&vanilla_villager_professions::NITWIT, 1).is_none());
    assert!(profession_trade_set(&vanilla_villager_professions::NONE, 1).is_none());
}

#[test]
fn plain_trades_copy_their_data_into_the_offer() {
    init_vanilla_registry();
    let mut random = LegacyRandom::from_seed(0);
    let offer = get_offer(
        &TRADE_ARMORER_1_EMERALD_IRON_HELMET,
        &mut plains_ctx(&mut random),
    )
    .expect("helmet offer");
    assert!(offer.result().is(&vanilla_items::IRON_HELMET));
    assert!(offer.cost_a().is(&vanilla_items::EMERALD));
    assert_eq!(offer.cost_a().count(), 5);
    assert!(offer.cost_b().is_empty());
    assert_eq!(offer.max_uses(), 12);
    assert_eq!(offer.xp(), 1);
    assert!((offer.price_multiplier() - 0.2).abs() < f32::EPSILON);
}

#[test]
fn enchanted_book_trades_charge_the_additional_cost() {
    init_vanilla_registry();
    init_behaviors();
    for seed in 0..SEEDS {
        let mut random = LegacyRandom::from_seed(seed);
        let offer = get_offer(
            &TRADE_LIBRARIAN_1_EMERALD_AND_BOOK_ENCHANTED_BOOK,
            &mut plains_ctx(&mut random),
        )
        .expect("enchanted book offer");
        let result = offer.result();
        assert!(result.is(&vanilla_items::ENCHANTED_BOOK));
        assert!(!result.has(ADDITIONAL_TRADE_COST));
        let stored = result
            .get(STORED_ENCHANTMENTS)
            .expect("stored enchantments");
        assert_eq!(stored.iter().count(), 1);
        // Base cost 0 plus at least `2 + 3 * level`.
        assert!((5..=64).contains(&offer.cost_a().count()));
        assert!(offer.cost_b().is(&vanilla_items::BOOK));
    }
}

#[test]
fn merchant_predicates_check_the_villager_type() {
    init_vanilla_registry();
    let trade = &TRADE_CARTOGRAPHER_2_EMERALD_AND_COMPASS_EXPLORER_JUNGLE_MAP;
    let mut random = LegacyRandom::from_seed(0);
    // Plains villagers never sell jungle explorer maps.
    assert!(get_offer(trade, &mut plains_ctx(&mut random)).is_none());
    let mut ctx = TradeContext {
        random: &mut random,
        merchant_variant: None,
    };
    assert!(get_offer(trade, &mut ctx).is_none());
}

#[test]
fn modifiers_dye_brew_and_tip_their_items() {
    init_vanilla_registry();
    init_behaviors();
    for seed in 0..SEEDS {
        let mut random = LegacyRandom::from_seed(seed);
        let mut ctx = plains_ctx(&mut random);
        let chestplate = get_offer(
            &TRADE_LEATHERWORKER_1_EMERALD_DYED_LEATHER_CHESTPLATE,
            &mut ctx,
        )
        .expect("dyed chestplate");
        assert!(chestplate.result().has(DYED_COLOR));

        let stew =
            get_offer(&TRADE_FARMER_4_EMERALD_SUSPICIOUS_STEW, &mut ctx).expect("stew offer");
        let effects = stew
            .result()
            .get(SUSPICIOUS_STEW_EFFECTS)
            .expect("stew effects");
        assert_eq!(effects.effects().len(), 1);
        // Durations are seconds, except for instantaneous saturation.
        let effect = &effects.effects()[0];
        if effect.effect().key.path.as_ref() == "saturation" {
            assert_eq!(effect.duration(), 7);
        } else {
            assert_eq!(effect.duration() % 20, 0);
        }

        let arrow = get_offer(&TRADE_FLETCHER_5_ARROW_AND_EMERALD_TIPPED_ARROW, &mut ctx)
            .expect("tipped arrow");
        let potion = arrow
            .result()
            .get(POTION_CONTENTS)
            .and_then(PotionContents::potion)
            .expect("arrow potion");
        assert!(potion.value().key.path.as_ref() != "water");
    }
}

#[test]
fn water_bottle_costs_require_a_water_potion() {
    init_vanilla_registry();
    let mut random = LegacyRandom::from_seed(0);
    let mut ctx = TradeContext {
        random: &mut random,
        merchant_variant: None,
    };
    let offer =
        get_offer(&TRADE_WANDERING_TRADER_WATER_BOTTLE_EMERALD, &mut ctx).expect("water offer");
    let water = PotionContents::create_item_stack(&vanilla_items::POTION, &vanilla_potions::WATER);
    let awkward =
        PotionContents::create_item_stack(&vanilla_items::POTION, &vanilla_potions::AWKWARD);
    assert!(offer.item_cost_a().test(&water));
    assert!(!offer.item_cost_a().test(&awkward));
    assert!(
        !offer
            .item_cost_a()
            .test(&ItemStack::new(&vanilla_items::POTION))
    );
}

#[test]
fn demand_and_special_prices_change_the_first_cost() {
    init_vanilla_registry();
    let mut offer = MerchantOffer::new(
        emeralds(10),
        None,
        ItemStack::new(&vanilla_items::BREAD),
        4,
        2,
        0.05,
    );
    for _ in 0..4 {
        offer.increase_uses();
    }
    offer.update_demand();
    assert_eq!(offer.demand(), 4);
    // floor(10 * 4 * 0.05) = 2.
    assert_eq!(offer.cost_a().count(), 12);
    offer.add_to_special_price_diff(-20);
    assert_eq!(offer.cost_a().count(), 1);
    assert!(offer.is_out_of_stock());

    let mut payment = ItemStack::with_count(&vanilla_items::EMERALD, 3);
    let mut nothing = ItemStack::empty();
    assert!(offer.take(&mut payment, &mut nothing));
    assert_eq!(payment.count(), 2);
}

fn round_trip(offers: &MerchantOffers) -> Option<MerchantOffers> {
    let nbt = offers.to_nbt();
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes)).ok()?;
    MerchantOffers::from_nbt((&borrowed).into())
}

#[test]
fn offers_round_trip_through_nbt_and_omit_defaults() {
    init_vanilla_registry();
    let mut offer = MerchantOffer::new(
        emeralds(3),
        Some(ItemCost::new(
            &vanilla_items::BOOK,
            1,
            DataComponentMap::new(),
        )),
        ItemStack::with_count(&vanilla_items::BREAD, 6),
        16,
        5,
        0.05,
    );
    offer.increase_uses();
    offer.set_special_price_diff(-1);
    let mut offers = MerchantOffers::new();
    offers.push(offer);
    offers.push(MerchantOffer::new(
        emeralds(1),
        None,
        ItemStack::new(&vanilla_items::APPLE),
        4,
        1,
        0.0,
    ));

    let nbt = offers.to_nbt();
    let recipes = nbt
        .list("Recipes")
        .and_then(|l| l.compounds())
        .expect("recipes");
    let defaults = &recipes[1];
    for key in [
        "uses",
        "maxUses",
        "rewardExp",
        "specialPrice",
        "demand",
        "priceMultiplier",
        "xp",
        "buyB",
    ] {
        assert!(defaults.get(key).is_none(), "{key} should be omitted");
    }
    assert_eq!(
        defaults.compound("buy").and_then(|buy| buy.int("count")),
        Some(1)
    );

    let loaded = round_trip(&offers).expect("offers load");
    assert_eq!(loaded.len(), 2);
    let first = loaded.get(0).expect("first offer");
    assert_eq!(first.uses(), 1);
    assert_eq!(first.max_uses(), 16);
    assert_eq!(first.xp(), 5);
    assert_eq!(first.special_price_diff(), -1);
    assert!(first.cost_b().is(&vanilla_items::BOOK));
    assert_eq!(first.result().count(), 6);
    assert!(MerchantOffers::new().to_nbt().is_empty());
}

#[test]
fn one_bad_offer_drops_the_whole_list() {
    init_vanilla_registry();
    let mut nbt = NbtCompound::new();
    let mut good = MerchantOffers::new();
    good.push(MerchantOffer::new(
        emeralds(1),
        None,
        ItemStack::new(&vanilla_items::APPLE),
        4,
        1,
        0.0,
    ));
    let mut recipes = good
        .to_nbt()
        .list("Recipes")
        .and_then(|l| l.compounds())
        .expect("recipes")
        .to_vec();
    let mut bad = NbtCompound::new();
    bad.insert("buy", NbtCompound::new());
    recipes.push(bad);
    nbt.insert("Recipes", NbtList::Compound(recipes));
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes)).expect("nbt");
    assert!(MerchantOffers::from_nbt((&borrowed).into()).is_none());
}

#[test]
fn trade_sets_without_duplicates_never_repeat_a_trade() {
    init_vanilla_registry();
    init_behaviors();
    let set = profession_trade_set(&vanilla_villager_professions::ARMORER, 1).expect("set");
    for seed in 0..SEEDS {
        let mut random = LegacyRandom::from_seed(seed);
        let mut offers = MerchantOffers::new();
        add_offers_from_trade_set(&mut plains_ctx(&mut random), &mut offers, set);
        assert_eq!(offers.len(), 2);
        let first = offers.get(0).expect("first").result().item().key.clone();
        let second = offers.get(1).expect("second").result().item().key.clone();
        assert_ne!(first, second);
    }
}

#[test]
fn villagers_roll_offers_on_first_use_and_on_level_up() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let villager = VillagerEntity::new(&vanilla_entities::VILLAGER, 1, DVec3::ZERO, Weak::new());
    assert_eq!(villager.with_offers(|offers| offers.len()), 0);

    let armorer = VillagerEntity::new(&vanilla_entities::VILLAGER, 2, DVec3::ZERO, Weak::new());
    armorer.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::ARMORER,
        1,
    );
    assert_eq!(armorer.with_offers(|offers| offers.len()), 2);
    armorer.increase_merchant_career();
    assert_eq!(armorer.villager_level(), 2);
    assert_eq!(armorer.with_offers(|offers| offers.len()), 4);

    let mut nbt = NbtCompound::new();
    armorer.save_additional(&mut nbt);
    assert!(nbt.compound("Offers").is_some());
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes)).expect("nbt");
    let loaded = VillagerEntity::new(&vanilla_entities::VILLAGER, 3, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.with_offers(|offers| offers.len()), 4);

    // A new profession drops the old offers and rolls the new ones.
    loaded.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::NITWIT,
        2,
    );
    assert_eq!(loaded.with_offers(|offers| offers.len()), 0);
}
