use std::sync::Arc;

use glam::DVec3;
use steel_protocol::packets::game::{CMerchantOffers, MerchantItemCost, MerchantOfferEntry};
use steel_registry::data_components::DataComponentMap;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_entities, vanilla_items,
    vanilla_mob_effects, vanilla_villager_professions, vanilla_villager_types,
};
use steel_utils::Downcast as _;
use steel_utils::codec::VarInt;
use steel_utils::serial::WriteTo as _;

use super::{MerchantKind, merchant_menu};
use crate::behavior::init_behaviors;
use crate::entity::entities::VillagerEntity;
use crate::entity::entities::mobs::villagers::Merchant;
use crate::entity::entities::mobs::villagers::trading::{ItemCost, MerchantOffer, MerchantOffers};
use crate::entity::{Entity as _, LivingEntity as _, SharedEntity};
use crate::inventory::click::{Click, MouseButton};
use crate::inventory::container::{Container as _, SimpleContainer};
use crate::inventory::menu::Menu;
use crate::player::Player;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};
use steel_utils::ChunkPos;
use steel_utils::locks::Shared;

/// Two emeralds for a loaf of bread, worth `xp` villager xp.
fn bread_offer(xp: i32) -> MerchantOffer {
    MerchantOffer::new(
        ItemCost::new(&vanilla_items::EMERALD, 2, DataComponentMap::new()),
        None,
        ItemStack::new(&vanilla_items::BREAD),
        12,
        xp,
        0.05,
    )
}

struct Trade {
    _world: TestWorld,
    player: Arc<Player>,
    villager: Arc<VillagerEntity>,
    menu: Menu,
}

fn trade(key: &'static str, offer_xp: i32) -> Trade {
    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world(key);
    let world = Arc::clone(&world_fixture.world);
    insert_ready_full_chunk(&world, ChunkPos::new(0, 0));
    let player = TestPlayerBuilder::new(Arc::clone(&world), "Trader", 1).build();
    player.base().set_position_local(DVec3::new(0.5, 64.0, 0.5));

    let villager = Arc::new(VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        2,
        DVec3::new(1.5, 64.0, 0.5),
        Arc::downgrade(&world),
    ));
    villager.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    villager.with_offers(|offers| {
        *offers = MerchantOffers::new();
        offers.push(bread_offer(offer_xp));
    });
    Merchant::set_trading_player(villager.as_ref(), Some(player.uuid()));
    let shared: SharedEntity = Arc::clone(&villager) as SharedEntity;
    let menu = merchant_menu(Arc::clone(&player.inventory), 1, shared, 1);
    Trade {
        _world: world_fixture,
        player,
        villager,
        menu,
    }
}

fn payments(menu: &Menu) -> Shared<SimpleContainer> {
    let Some(kind) = menu.kind().downcast_ref::<MerchantKind>() else {
        panic!("merchant_menu should build a merchant menu");
    };
    Arc::clone(&kind.handler.payments)
}

fn result(menu: &Menu) -> ItemStack {
    let Some(kind) = menu.kind().downcast_ref::<MerchantKind>() else {
        panic!("merchant_menu should build a merchant menu");
    };
    kind.handler.result.lock().get_item(0).clone()
}

#[test]
fn paying_shows_the_result_in_either_payment_slot() {
    let mut trade = trade("merchant_menu_result", 1);
    let payments = payments(&trade.menu);

    payments
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::EMERALD, 2));
    trade.menu.update_merchant_sell_item();
    assert!(result(&trade.menu).is(&vanilla_items::BREAD));

    payments
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::EMERALD, 1));
    trade.menu.update_merchant_sell_item();
    assert!(result(&trade.menu).is_empty());

    {
        let mut payments = payments.lock();
        payments.set_item(0, ItemStack::empty());
        payments.set_item(1, ItemStack::with_count(&vanilla_items::EMERALD, 3));
    }
    trade.menu.update_merchant_sell_item();
    assert!(result(&trade.menu).is(&vanilla_items::BREAD));
}

#[test]
fn taking_the_result_pays_and_uses_the_offer() {
    let mut trade = trade("merchant_menu_take", 1);
    let payments = payments(&trade.menu);
    payments
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::EMERALD, 5));
    trade.menu.update_merchant_sell_item();

    trade.menu.clicked(
        Click::Pickup {
            slot: 2,
            button: MouseButton::Left,
        },
        &trade.player,
    );

    assert!(trade.menu.behavior().carried().is(&vanilla_items::BREAD));
    assert_eq!(payments.lock().get_item(0).count(), 3);
    assert!(result(&trade.menu).is(&vanilla_items::BREAD));
    let uses = trade
        .villager
        .with_offers(|offers| offers.get(0).map(MerchantOffer::uses));
    assert_eq!(uses, Some(1));
    assert_eq!(trade.villager.villager_xp(), 1);
}

#[test]
fn trading_enough_xp_levels_the_villager_up() {
    let mut trade = trade("merchant_menu_level_up", 10);
    payments(&trade.menu)
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::EMERALD, 2));
    trade.menu.update_merchant_sell_item();

    trade.menu.clicked(
        Click::Pickup {
            slot: 2,
            button: MouseButton::Left,
        },
        &trade.player,
    );

    assert_eq!(trade.villager.villager_level(), 2);
    assert!(
        trade
            .villager
            .has_mob_effect(vanilla_mob_effects::REGENERATION)
    );
    // The level-2 farmer trades are added to the existing offer.
    let offer_count = trade.villager.with_offers(|offers| offers.len());
    assert!(offer_count > 1);
}

#[test]
fn selecting_a_trade_fills_the_payment_from_the_inventory() {
    let mut trade = trade("merchant_menu_select", 1);
    trade
        .player
        .inventory
        .lock()
        .set_item(9, ItemStack::with_count(&vanilla_items::EMERALD, 7));

    trade.menu.select_trade(0);

    let payments = payments(&trade.menu);
    assert_eq!(payments.lock().get_item(0).count(), 7);
    assert!(trade.player.inventory.lock().get_item(9).is_empty());
    assert!(result(&trade.menu).is(&vanilla_items::BREAD));
}

#[test]
fn closing_stops_trading_and_returns_the_payment() {
    let mut trade = trade("merchant_menu_close", 1);
    payments(&trade.menu)
        .lock()
        .set_item(0, ItemStack::with_count(&vanilla_items::EMERALD, 2));
    assert!(trade.menu.still_valid(&trade.player));

    trade.menu.removed(&trade.player);

    assert_eq!(Merchant::trading_player(trade.villager.as_ref()), None);
    assert!(!trade.menu.still_valid(&trade.player));
    let inventory = trade.player.inventory.lock();
    let returned: i32 = (0..36)
        .map(|slot| inventory.get_item(slot))
        .filter(|stack| stack.is(&vanilla_items::EMERALD))
        .map(ItemStack::count)
        .sum();
    assert_eq!(returned, 2);
}

#[test]
fn the_menu_is_invalid_out_of_reach() {
    let trade = trade("merchant_menu_reach", 1);
    assert!(trade.menu.still_valid(&trade.player));
    trade
        .player
        .base()
        .set_position_local(DVec3::new(20.0, 64.0, 0.5));
    assert!(!trade.menu.still_valid(&trade.player));
}

#[test]
fn offers_packet_matches_the_vanilla_layout() {
    init_vanilla_registry();
    let packet = CMerchantOffers {
        container_id: 3,
        offers: vec![MerchantOfferEntry {
            cost_a: MerchantItemCost {
                item_id: vanilla_items::EMERALD.id() as i32,
                count: 2,
                components: DataComponentMap::new(),
            },
            result: ItemStack::new(&vanilla_items::BREAD),
            cost_b: None,
            out_of_stock: false,
            uses: 1,
            max_uses: 12,
            xp: 2,
            special_price_diff: -1,
            price_multiplier: 0.05,
            demand: 4,
        }],
        villager_level: 2,
        villager_xp: 15,
        show_progress: true,
        can_restock: true,
    };
    let mut written = Vec::new();
    packet.write(&mut written).expect("packet should encode");

    let mut expected = Vec::new();
    for value in [3, 1, vanilla_items::EMERALD.id() as i32, 2, 0] {
        VarInt(value).write(&mut expected).expect("varint");
    }
    ItemStack::new(&vanilla_items::BREAD)
        .write(&mut expected)
        .expect("result");
    expected.extend([0, 0]);
    for value in [1_i32, 12, 2, -1] {
        expected.extend(value.to_be_bytes());
    }
    expected.extend(0.05_f32.to_be_bytes());
    expected.extend(4_i32.to_be_bytes());
    VarInt(2).write(&mut expected).expect("level");
    VarInt(15).write(&mut expected).expect("xp");
    expected.extend([1, 1]);
    assert_eq!(written, expected);
}
