//! Villagers sharing what they carry: vanilla `TradeWithVillager` and
//! `GiveGiftToHero`.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::loot_table::{LootContext, LootTableRef};
use steel_registry::vanilla_mob_effects::HERO_OF_THE_VILLAGE;
use steel_registry::{
    vanilla_entities, vanilla_items, vanilla_loot_tables, vanilla_villager_professions,
};
use steel_utils::Downcast as _;

use super::super::{VillagerEntity, requested_items};
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::behaviors::utils::{
    entity_is_visible, lock_gaze_and_walk_to_each_other, look_at_entity,
    set_walk_and_look_target_memories, throw_item,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    INTERACTION_TARGET, LOOK_TARGET, NEAREST_VISIBLE_LIVING_ENTITIES, NEAREST_VISIBLE_PLAYER,
    PositionTracker, WALK_TARGET,
};
use crate::entity::{
    AgeableMob, Entity as _, SharedEntity, entity_loot_ref, living_loot_equipment,
};
use crate::inventory::container::Container as _;
use crate::world::World;

/// Vanilla `TradeWithVillager`: how close the other villager must be, squared.
const TRADE_DISTANCE_SQR: f64 = 5.0;
/// Vanilla `TradeWithVillager`: speed when walking up to the other villager.
const TRADE_SPEED_MODIFIER: f32 = 0.5;
/// Vanilla `throwHalfStack`: a stack of more than this always shares the excess.
const KEEP_AT_MOST: i32 = 24;
/// Vanilla `GiveGiftToHero.THROW_GIFT_AT_DISTANCE`.
const THROW_GIFT_AT_DISTANCE: i32 = 5;
/// Vanilla `GiveGiftToHero.MIN_TIME_BETWEEN_GIFTS`.
const MIN_TIME_BETWEEN_GIFTS: i32 = 600;
/// Vanilla `GiveGiftToHero.MAX_TIME_BETWEEN_GIFTS`.
const MAX_TIME_BETWEEN_GIFTS: i32 = 6600;
/// Vanilla `GiveGiftToHero.TIME_TO_DELAY_FOR_HEAD_TO_FINISH_TURNING`.
const TIME_TO_DELAY_FOR_HEAD_TO_FINISH_TURNING: i64 = 20;
/// Vanilla `GiveGiftToHero.SPEED_MODIFIER`.
const GIFT_SPEED_MODIFIER: f32 = 0.5;
/// Vanilla `new GiveGiftToHero(100)`.
const GIFT_TIMEOUT: i32 = 100;

/// Vanilla `BehaviorUtils.throwItem(thrower, item, targetPos)`.
fn throw_toward(thrower: &VillagerEntity, item: ItemStack, target_pos: DVec3) {
    let velocity = f64::from(0.3_f32);
    throw_item(thrower, item, target_pos, DVec3::splat(velocity), 0.3);
}

/// The villager in the body's `INTERACTION_TARGET`, if it is alive and seen
/// (vanilla `BehaviorUtils.targetIsValid(brain, INTERACTION_TARGET, VILLAGER)`).
fn interaction_villager(body: &VillagerEntity) -> Option<SharedEntity> {
    let target = body.brain.memories().lock().get(&INTERACTION_TARGET)?;
    (target.entity_type() == &vanilla_entities::VILLAGER
        && target.is_alive()
        && entity_is_visible(body, &target))
    .then_some(target)
}

const TRADE_WITH_VILLAGER_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
];

/// Vanilla `TradeWithVillager`: villagers that meet gossip and toss each
/// other spare food, wheat and the items the other's job asks for.
pub(super) struct TradeWithVillager {
    trades: Vec<ItemRef>,
}

impl TradeWithVillager {
    pub(super) const fn new() -> Self {
        Self { trades: Vec::new() }
    }

    /// Vanilla `figureOutWhatIAmWillingToTrade`.
    fn willing_to_trade(body: &VillagerEntity, target: &VillagerEntity) -> Vec<ItemRef> {
        let own = requested_items(body.profession());
        requested_items(target.profession())
            .into_iter()
            .filter(|item| !own.iter().any(|own| own.key == item.key))
            .collect()
    }
}

/// Vanilla `TradeWithVillager.throwHalfStack`: throws half of the first
/// matching stack that is over half full, or what it holds over 24.
pub(super) fn throw_half_stack(
    villager: &VillagerEntity,
    predicate: impl Fn(&ItemStack) -> bool,
    target_pos: DVec3,
) {
    let to_throw = {
        let mut inventory = villager.inventory().lock();
        let mut to_throw = ItemStack::empty();
        for slot in 0..inventory.get_container_size() {
            let stack = inventory.get_item(slot);
            if stack.is_empty() || !predicate(stack) {
                continue;
            }
            let count = if stack.count() > stack.max_stack_size() / 2 {
                stack.count() / 2
            } else if stack.count() > KEEP_AT_MOST {
                stack.count() - KEEP_AT_MOST
            } else {
                continue;
            };
            to_throw = inventory.get_item_mut(slot).split(count);
            break;
        }
        to_throw
    };
    if !to_throw.is_empty() {
        throw_toward(villager, to_throw, target_pos);
    }
}

impl Behavior<VillagerEntity> for TradeWithVillager {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &TRADE_WITH_VILLAGER_ENTRY
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        interaction_villager(body).is_some()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        interaction_villager(body).is_some()
    }

    fn start(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        let Some(target) = interaction_villager(body) else {
            return;
        };
        if let Some(body_entity) = world.get_entity_by_id(body.id()) {
            lock_gaze_and_walk_to_each_other(&body_entity, &target, TRADE_SPEED_MODIFIER, 2);
        }
        self.trades = target
            .downcast_ref::<VillagerEntity>()
            .map_or_else(Vec::new, |villager| Self::willing_to_trade(body, villager));
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        let Some(target_entity) = interaction_villager(body) else {
            return;
        };
        let Some(target) = target_entity.downcast_ref::<VillagerEntity>() else {
            return;
        };
        if body.position().distance_squared(target.position()) > TRADE_DISTANCE_SQR {
            return;
        }
        if let Some(body_entity) = world.get_entity_by_id(body.id()) {
            lock_gaze_and_walk_to_each_other(&body_entity, &target_entity, TRADE_SPEED_MODIFIER, 2);
        }
        body.gossip(target, time);
        let target_pos = target.position();
        let farmer = body.profession().key == vanilla_villager_professions::FARMER.key;
        if body.has_excess_food() && (farmer || target.wants_more_food()) {
            throw_half_stack(body, VillagerEntity::is_villager_food, target_pos);
        }
        if farmer {
            let wheat = body.inventory().lock().count_item(&vanilla_items::WHEAT);
            if wheat > ItemStack::new(&vanilla_items::WHEAT).max_stack_size() / 2 {
                throw_half_stack(body, |stack| stack.is(&vanilla_items::WHEAT), target_pos);
            }
        }
        if !self.trades.is_empty() {
            let has_any = {
                let inventory = body.inventory().lock();
                self.trades
                    .iter()
                    .any(|item| inventory.count_item(item) > 0)
            };
            if has_any {
                let trades = &self.trades;
                throw_half_stack(
                    body,
                    |stack| trades.iter().any(|item| stack.is(item)),
                    target_pos,
                );
            }
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        body.brain.memories().lock().erase(&INTERACTION_TARGET);
    }
}

const GIVE_GIFT_TO_HERO_ENTRY: [MemoryRequirement; 4] = [
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&NEAREST_VISIBLE_PLAYER, MemoryStatus::ValuePresent),
];

/// Vanilla `GiveGiftToHero`: villagers walk up to a player with Hero of the
/// Village and toss them a gift from their job's loot table.
pub(super) struct GiveGiftToHero {
    time_until_next_gift: i32,
    gift_given_during_this_run: bool,
    time_since_start: i64,
}

impl GiveGiftToHero {
    pub(super) const fn new() -> Self {
        Self {
            time_until_next_gift: MIN_TIME_BETWEEN_GIFTS,
            gift_given_during_this_run: false,
            time_since_start: 0,
        }
    }

    /// Vanilla `getNearestTargetableHero`.
    fn nearest_targetable_hero(body: &VillagerEntity) -> Option<SharedEntity> {
        body.brain
            .memories()
            .lock()
            .get(&NEAREST_VISIBLE_PLAYER)
            .filter(|player| {
                player
                    .as_living_entity()
                    .is_some_and(|living| living.has_mob_effect(HERO_OF_THE_VILLAGE))
            })
    }

    /// Vanilla `isWithinThrowingDistance`: block positions closer than 5.
    fn is_within_throwing_distance(body: &VillagerEntity, player: &SharedEntity) -> bool {
        let villager = body.block_position();
        let player = player.block_position();
        let dx = i64::from(villager.x() - player.x());
        let dy = i64::from(villager.y() - player.y());
        let dz = i64::from(villager.z() - player.z());
        let range = i64::from(THROW_GIFT_AT_DISTANCE);
        dx * dx + dy * dy + dz * dz < range * range
    }

    /// Vanilla `throwGift`.
    fn throw_gift(body: &VillagerEntity, target: &SharedEntity) {
        let position = body.position();
        let target_pos = target.position();
        let mut rng = rand::rng();
        let equipment = living_loot_equipment(body);
        // Vanilla `LootContextParamSets.GIFT`: ORIGIN and THIS_ENTITY.
        let mut context = LootContext::new(&mut rng)
            .with_origin(position.x, position.y, position.z)
            .with_this_entity(entity_loot_ref(body, Some(&equipment)));
        for item in gift_loot_table(body).get_random_items(&mut context) {
            throw_toward(body, item, target_pos);
        }
    }
}

/// Vanilla `getLootTableToThrow`.
pub(super) fn gift_loot_table(villager: &VillagerEntity) -> LootTableRef {
    if AgeableMob::is_baby(villager) {
        return &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_BABY_GIFT;
    }
    let key = &villager.profession().key;
    let gifts: [(&steel_utils::Identifier, LootTableRef); 13] = [
        (
            &vanilla_villager_professions::ARMORER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_ARMORER_GIFT,
        ),
        (
            &vanilla_villager_professions::BUTCHER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_BUTCHER_GIFT,
        ),
        (
            &vanilla_villager_professions::CARTOGRAPHER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_CARTOGRAPHER_GIFT,
        ),
        (
            &vanilla_villager_professions::CLERIC.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_CLERIC_GIFT,
        ),
        (
            &vanilla_villager_professions::FARMER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_FARMER_GIFT,
        ),
        (
            &vanilla_villager_professions::FISHERMAN.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_FISHERMAN_GIFT,
        ),
        (
            &vanilla_villager_professions::FLETCHER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_FLETCHER_GIFT,
        ),
        (
            &vanilla_villager_professions::LEATHERWORKER.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_LEATHERWORKER_GIFT,
        ),
        (
            &vanilla_villager_professions::LIBRARIAN.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_LIBRARIAN_GIFT,
        ),
        (
            &vanilla_villager_professions::MASON.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_MASON_GIFT,
        ),
        (
            &vanilla_villager_professions::SHEPHERD.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_SHEPHERD_GIFT,
        ),
        (
            &vanilla_villager_professions::TOOLSMITH.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_TOOLSMITH_GIFT,
        ),
        (
            &vanilla_villager_professions::WEAPONSMITH.key,
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_WEAPONSMITH_GIFT,
        ),
    ];
    gifts
        .into_iter()
        .find(|(profession, _)| *profession == key)
        .map_or(
            &vanilla_loot_tables::GAMEPLAY_HERO_OF_THE_VILLAGE_UNEMPLOYED_GIFT,
            |(_, table)| table,
        )
}

impl Behavior<VillagerEntity> for GiveGiftToHero {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &GIVE_GIFT_TO_HERO_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (GIFT_TIMEOUT, GIFT_TIMEOUT)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        if Self::nearest_targetable_hero(body).is_none() {
            return false;
        }
        if self.time_until_next_gift > 0 {
            self.time_until_next_gift -= 1;
            return false;
        }
        true
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        self.gift_given_during_this_run = false;
        self.time_since_start = time;
        let Some(player) = Self::nearest_targetable_hero(body) else {
            return;
        };
        body.brain
            .memories()
            .lock()
            .set(&INTERACTION_TARGET, Some(Arc::clone(&player)));
        look_at_entity(body, &player);
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        Self::nearest_targetable_hero(body).is_some() && !self.gift_given_during_this_run
    }

    fn tick(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        let Some(player) = Self::nearest_targetable_hero(body) else {
            return;
        };
        look_at_entity(body, &player);
        if Self::is_within_throwing_distance(body, &player) {
            if time - self.time_since_start > TIME_TO_DELAY_FOR_HEAD_TO_FINISH_TURNING {
                Self::throw_gift(body, &player);
                self.gift_given_during_this_run = true;
            }
        } else {
            set_walk_and_look_target_memories(
                body,
                PositionTracker::entity(player, true),
                GIFT_SPEED_MODIFIER,
                5,
            );
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        self.time_until_next_gift = MIN_TIME_BETWEEN_GIFTS
            + rand::random_range(0..=MAX_TIME_BETWEEN_GIFTS - MIN_TIME_BETWEEN_GIFTS);
        let mut memories = body.brain.memories().lock();
        memories.erase(&INTERACTION_TARGET);
        memories.erase(&WALK_TARGET);
        memories.erase(&LOOK_TARGET);
    }
}
