use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_entities, vanilla_items, vanilla_villager_professions,
    vanilla_villager_types,
};
use steel_utils::types::InteractionHand;
use steel_utils::{ChunkPos, Downcast as _, Identifier, WorldAabb};
use uuid::Uuid;

use super::VillagerEntity;
use crate::behavior::{InteractionResult, init_behaviors};
use crate::entity::ai::gossip::ReputationEventType;
use crate::entity::entities::mobs::villagers::Merchant;
use crate::entity::entities::mobs::villagers::villager_data::villager_type_by_biome;
use crate::entity::entities::{LightningBoltEntity, WitchEntity};
use crate::entity::{AgeableMob, Entity, Mob, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{TestPlayerBuilder, fresh_test_world, insert_ready_full_chunk};

fn villager(id: i32) -> VillagerEntity {
    VillagerEntity::new(&vanilla_entities::VILLAGER, id, DVec3::ZERO, Weak::new())
}

#[test]
fn biomes_pick_vanilla_villager_types() {
    init_vanilla_registry();
    let cases = [
        ("eroded_badlands", &vanilla_villager_types::DESERT),
        ("sparse_jungle", &vanilla_villager_types::JUNGLE),
        ("windswept_savanna", &vanilla_villager_types::SAVANNA),
        ("grove", &vanilla_villager_types::SNOW),
        ("mangrove_swamp", &vanilla_villager_types::SWAMP),
        ("windswept_forest", &vanilla_villager_types::TAIGA),
        ("forest", &vanilla_villager_types::PLAINS),
        ("cherry_grove", &vanilla_villager_types::PLAINS),
    ];
    for (biome, expected) in cases {
        assert_eq!(
            villager_type_by_biome(&Identifier::vanilla(biome.to_owned())).key,
            expected.key,
            "{biome}"
        );
    }
}

#[test]
fn new_villagers_are_unemployed_plains_villagers_at_level_one() {
    init_vanilla_registry();
    let mob = villager(1);
    assert_eq!(mob.villager_type().key, vanilla_villager_types::PLAINS.key);
    assert_eq!(mob.profession().key, vanilla_villager_professions::NONE.key);
    assert_eq!(mob.villager_level(), 1);
    assert!(!mob.villager_data_finalized());
    assert!(!mob.mob_can_be_leashed());
    assert!(!mob.remove_when_far_away(1.0e9));
}

#[test]
fn villager_level_is_at_least_one() {
    init_vanilla_registry();
    let mob = villager(1);
    mob.set_villager_data(
        &vanilla_villager_types::SNOW,
        &vanilla_villager_professions::LIBRARIAN,
        -3,
    );
    assert_eq!(mob.villager_level(), 1);
    assert_eq!(mob.villager_type().key, vanilla_villager_types::SNOW.key);
    assert_eq!(
        mob.profession().key,
        vanilla_villager_professions::LIBRARIAN.key
    );
}

#[test]
fn villager_state_round_trips() {
    init_vanilla_registry();
    let mob = villager(1);
    mob.set_villager_data(
        &vanilla_villager_types::TAIGA,
        &vanilla_villager_professions::FARMER,
        3,
    );
    mob.set_villager_xp(77);
    let player = Uuid::from_u128(42);
    mob.on_reputation_event_from(ReputationEventType::ZombieVillagerCured, player);
    let _ = mob
        .inventory()
        .lock()
        .add_item(&ItemStack::with_count(&vanilla_items::BREAD, 5));

    let mut nbt = NbtCompound::new();
    mob.save_additional(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = villager(2);
    loaded.load_additional((&borrowed).into());

    assert!(loaded.villager_data_finalized());
    assert_eq!(
        loaded.villager_type().key,
        vanilla_villager_types::TAIGA.key
    );
    assert_eq!(
        loaded.profession().key,
        vanilla_villager_professions::FARMER.key
    );
    assert_eq!(loaded.villager_level(), 3);
    assert_eq!(loaded.villager_xp(), 77);
    assert_eq!(loaded.player_reputation(player), 20 * 5 + 25);
    assert_eq!(loaded.gossips(), mob.gossips());
    assert_eq!(loaded.inventory().lock().store_as_item_list().len(), 1);
}

#[test]
fn villagers_pick_up_food_and_their_job_items_only() {
    init_vanilla_registry();
    let mob = villager(1);
    assert!(mob.wants_to_pick_up(&ItemStack::new(&vanilla_items::BREAD)));
    assert!(mob.wants_to_pick_up(&ItemStack::new(&vanilla_items::WHEAT_SEEDS)));
    assert!(!mob.wants_to_pick_up(&ItemStack::new(&vanilla_items::BONE_MEAL)));
    assert!(!mob.wants_to_pick_up(&ItemStack::new(&vanilla_items::STONE)));

    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    assert!(mob.wants_to_pick_up(&ItemStack::new(&vanilla_items::BONE_MEAL)));
}

#[test]
fn hurting_a_villager_spreads_bad_gossip() {
    init_vanilla_registry();
    let mob = villager(1);
    let attacker = Uuid::from_u128(7);
    mob.on_reputation_event_from(ReputationEventType::VillagerHurt, attacker);
    assert_eq!(mob.player_reputation(attacker), -25);
    mob.on_reputation_event_from(ReputationEventType::VillagerKilled, attacker);
    assert_eq!(mob.player_reputation(attacker), -25 - 25 * 5);
}

#[test]
fn talking_to_a_villager_without_trades_makes_it_unhappy() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_talk");
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Talker", 21).build();
    let mob = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        DVec3::new(1.0, 64.0, 1.0),
        Arc::downgrade(world),
    );

    assert_eq!(
        mob.mob_interact(&player, InteractionHand::OffHand),
        InteractionResult::Consume
    );
    assert_eq!(mob.unhappy_counter(), 0, "only the main hand complains");
    assert_eq!(
        mob.mob_interact(&player, InteractionHand::MainHand),
        InteractionResult::Consume
    );
    assert_eq!(mob.unhappy_counter(), 40);

    mob.set_age(mob.get_baby_start_age());
    mob.set_unhappy_counter(0);
    assert_eq!(
        mob.mob_interact(&player, InteractionHand::OffHand),
        InteractionResult::Success
    );
    assert_eq!(
        mob.unhappy_counter(),
        40,
        "babies complain from either hand"
    );
}

#[test]
fn lightning_turns_a_villager_into_a_persistent_witch() {
    init_vanilla_registry();
    init_entities();
    init_behaviors();
    let fixture = fresh_test_world("villager_lightning");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let mob = Arc::new(VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    ));
    let entity: SharedEntity = Arc::<VillagerEntity>::clone(&mob);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("villager should attach to the loaded chunk");
    let bolt = LightningBoltEntity::new(
        &vanilla_entities::LIGHTNING_BOLT,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    );

    mob.thunder_hit(world, &bolt);

    assert!(mob.is_removed(), "the villager is replaced");
    let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 80.0, 16.0);
    let witches: Vec<_> = world
        .get_entities_in_aabb(&area)
        .into_iter()
        .filter(|entity| entity.downcast_ref::<WitchEntity>().is_some())
        .collect();
    assert_eq!(witches.len(), 1);
    let witch = witches[0]
        .as_mob()
        .unwrap_or_else(|| panic!("a witch is a mob"));
    assert!(witch.is_persistence_required());
}

#[test]
fn trading_starts_with_reputation_discounts() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("villager_trade_discount");
    let world = &fixture.world;
    let player = TestPlayerBuilder::new(Arc::clone(world), "Customer", 21).build();
    let mob = VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        DVec3::new(1.0, 64.0, 1.0),
        Arc::downgrade(world),
    );
    mob.set_villager_data(
        &vanilla_villager_types::PLAINS,
        &vanilla_villager_professions::FARMER,
        1,
    );
    // 20 major positive (weight 5) + 25 minor positive = 125 reputation.
    mob.on_reputation_event_from(ReputationEventType::ZombieVillagerCured, player.uuid());

    assert_eq!(
        mob.mob_interact(&player, InteractionHand::MainHand),
        InteractionResult::Success
    );
    assert_eq!(Merchant::trading_player(&mob), Some(player.uuid()));
    let discounts = mob.with_offers(|offers| {
        offers
            .iter()
            .map(|offer| {
                (
                    offer.special_price_diff(),
                    -(125.0 * offer.price_multiplier()).floor() as i32,
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(discounts.len(), 2, "a novice farmer rolls two trades");
    for (actual, expected) in discounts {
        assert_eq!(actual, expected);
    }

    // Closing the screen clears the trading player and the discounts.
    Merchant::set_trading_player(&mob, None);
    assert!(mob.with_offers(|offers| offers.iter().all(|offer| offer.special_price_diff() == 0)));
}
