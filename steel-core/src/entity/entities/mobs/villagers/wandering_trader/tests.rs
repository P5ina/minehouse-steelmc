use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, sound_events, vanilla_entities, vanilla_items};
use steel_utils::BlockPos;
use steel_utils::types::InteractionHand;

use super::WanderingTraderEntity;
use crate::behavior::{InteractionResult, init_behaviors};
use crate::entity::entities::mobs::villagers::Merchant;
use crate::entity::{AgeableMob, Entity, LivingEntity, Mob, init_entities, next_entity_id};
use crate::test_support::{TestPlayerBuilder, fresh_test_world};

fn trader(id: i32) -> WanderingTraderEntity {
    WanderingTraderEntity::new(
        &vanilla_entities::WANDERING_TRADER,
        id,
        DVec3::ZERO,
        Weak::new(),
    )
}

#[test]
fn traders_roll_buying_uncommon_and_common_trades() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let mob = trader(1);
    // Vanilla trade sets: 2 buying, 2 uncommon and 5 common trades.
    assert_eq!(mob.with_offers(|offers| offers.len()), 9);
    let first =
        mob.with_offers(|offers| offers.get(0).map(|offer| offer.result().item().key.clone()));
    assert_eq!(first, Some(vanilla_items::EMERALD.key.clone()));
}

#[test]
fn traders_save_and_load_offers_despawn_delay_and_wander_target() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let mob = trader(1);
    mob.set_despawn_delay(48_000);
    mob.set_wander_target(Some(BlockPos::new(10, 64, -3)));
    let offers = mob.with_offers(|offers| offers.len());
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::BREAD, 2));
    mob.set_age(-100);

    let mut nbt = NbtCompound::new();
    mob.save_additional(&mut nbt);
    assert_eq!(nbt.int("DespawnDelay"), Some(48_000));
    assert_eq!(nbt.int_array("wander_target"), Some(&[10, 64, -3][..]));
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = trader(2);
    loaded.load_additional((&borrowed).into());

    assert_eq!(loaded.despawn_delay(), 48_000);
    assert_eq!(loaded.wander_target(), Some(BlockPos::new(10, 64, -3)));
    assert_eq!(loaded.with_offers(|offers| offers.len()), offers);
    assert_eq!(loaded.inventory().lock().store_as_item_list().len(), 1);
    assert_eq!(loaded.get_age(), 0, "a loaded trader is never a baby");
}

#[test]
fn traders_without_a_wander_target_save_none() {
    init_vanilla_registry();
    let mob = trader(1);
    let mut nbt = NbtCompound::new();
    mob.save_additional(&mut nbt);
    assert!(nbt.int_array("wander_target").is_none());
    assert_eq!(nbt.int("DespawnDelay"), Some(0));
    assert!(
        nbt.compound("Offers").is_none(),
        "offers are rolled on first use"
    );
}

#[test]
fn traders_despawn_when_the_delay_runs_out() {
    init_vanilla_registry();
    let mob = trader(1);
    mob.maybe_despawn();
    assert!(!mob.is_removed(), "a zero delay never despawns");
    mob.set_despawn_delay(2);
    mob.maybe_despawn();
    assert!(!mob.is_removed());
    assert_eq!(mob.despawn_delay(), 1);
    mob.maybe_despawn();
    assert!(mob.is_removed());
}

#[test]
fn traders_drink_with_their_own_sounds() {
    init_vanilla_registry();
    let mob = trader(1);
    assert_eq!(
        mob.consume_sound_override(&ItemStack::new(&vanilla_items::MILK_BUCKET))
            .map(|sound| sound.key.clone()),
        Some(sound_events::ENTITY_WANDERING_TRADER_DRINK_MILK.key.clone())
    );
    assert_eq!(
        mob.consume_sound_override(&ItemStack::new(&vanilla_items::POTION))
            .map(|sound| sound.key.clone()),
        Some(
            sound_events::ENTITY_WANDERING_TRADER_DRINK_POTION
                .key
                .clone()
        )
    );
    assert!(!mob.mob_can_be_leashed());
    assert!(!mob.remove_when_far_away(1.0e6));
}

#[test]
fn talking_to_a_trader_counts_and_opens_its_offers() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("wandering_trader_talk");
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Talker", 21).build();
    let mob = WanderingTraderEntity::new(
        &vanilla_entities::WANDERING_TRADER,
        next_entity_id(),
        DVec3::new(1.0, 64.0, 1.0),
        Arc::downgrade(world),
    );
    assert_eq!(
        mob.mob_interact(&player, InteractionHand::MainHand),
        InteractionResult::Success
    );
    assert_eq!(mob.with_offers(|offers| offers.len()), 9);
    assert_eq!(Merchant::trading_player(&mob), Some(player.uuid()));
    // A trader that is already trading does not open a second screen.
    assert_ne!(
        mob.mob_interact(&player, InteractionHand::MainHand),
        InteractionResult::Success
    );
}

#[test]
fn a_trading_wandering_trader_does_not_despawn() {
    init_vanilla_registry();
    init_behaviors();
    let trader = trader(3);
    trader.set_despawn_delay(1);
    Merchant::set_trading_player(&trader, Some(uuid::Uuid::new_v4()));
    trader.maybe_despawn();
    assert_eq!(trader.despawn_delay(), 1);
    Merchant::set_trading_player(&trader, None);
    trader.maybe_despawn();
    assert_eq!(trader.despawn_delay(), 0);
}
