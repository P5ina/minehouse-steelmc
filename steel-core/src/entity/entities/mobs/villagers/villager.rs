//! Villager (Vanilla `Villager` with its `AbstractVillager` parts).
//!
//! This part carries the villager's data, gossip, inventory, saving,
//! lightning, trading and its brain (see [`brain`]). Jobs, beds, breeding,
//! golem calls and restocking come in later parts.

use std::borrow::Cow;
use std::sync::{Arc, Weak};
use std::{mem, ptr};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::BlockRef;
use steel_registry::data_components::vanilla_components::{VILLAGER_FOOD, VILLAGER_VARIANT};
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::VillagerEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::villager_type::VillagerTypeRef;
use steel_registry::{
    REGISTRY, RegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_custom_stats, vanilla_entities, vanilla_items, vanilla_mob_effects, vanilla_timelines,
    vanilla_villager_professions, vanilla_villager_types,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::random::xoroshiro::Xoroshiro;
use steel_utils::types::{Difficulty, InteractionHand};
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey, GlobalPos, Identifier};
use text_components::TextComponent;
use text_components::translation::TranslatedMessage;
use uuid::Uuid;

use super::trading::{
    MerchantOffer, MerchantOffers, TradeContext, add_offers_from_trade_set, profession_trade_set,
};
use super::villager_data::{
    MIN_VILLAGER_LEVEL, can_level_up, max_xp_per_level, read_villager_data, villager_type_at,
    write_villager_data,
};
use super::{
    FIRE_IN_NEIGHBOR_MALUS, FIRE_MALUS, INVENTORY_SIZE, load_inventory,
    pick_up_item_into_inventory, save_inventory,
};
use super::{
    Merchant, open_trading_screen, play_trade_updated_sound, record_trade, send_merchant_offers,
    spawn_trade_experience, trading_player_of,
};
use crate::behavior::InteractionResult;
use crate::entity::ai::brain::memory::MemoryModuleType;
use crate::entity::ai::brain::memory_types::{
    HOME, JOB_SITE, LAST_WOKEN, MEETING_POINT, POTENTIAL_JOB_SITE,
};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::gossip::{GossipContainer, GossipType, ReputationEventType};
use crate::entity::ai::path::PathType;
use crate::entity::conversion::ConversionParams;
use crate::entity::damage::DamageSource;
use crate::entity::entities::{ItemEntity, LightningBoltEntity};
use crate::entity::living_base::MobEffectInstance;
use crate::entity::remove_entity_name_actions;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityEventSource as _, EntityPose, EntitySpawnReason, EntitySyncedData,
    LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData,
    SpawnRule, sync_dirty_mob_effects,
};
use crate::inventory::container::SimpleContainer;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

/// Vanilla `Villager.BABY_DIMENSIONS`.
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.49, 0.98, 0.63);
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `setRequiredPathLength(48.0F)`.
const REQUIRED_PATH_LENGTH: f32 = 48.0;
/// Vanilla `Mob.createMobAttributes` follow range.
const DEFAULT_FOLLOW_RANGE: f64 = 16.0;
/// Vanilla `rewardTradeXp`: Regeneration a leveled-up villager gets.
const LEVEL_UP_REGENERATION_TICKS: i32 = 200;
/// Vanilla `setUnhappy`: ticks the villager shakes its head.
const UNHAPPY_TICKS: i32 = 40;
/// Vanilla `AbstractVillager` offers NBT key.
const TAG_OFFERS: &str = "Offers";
/// Vanilla `GOSSIP_DECAY_INTERVAL`.
const GOSSIP_DECAY_INTERVAL: i64 = 24_000;
/// Vanilla `Villager.GOSSIP_COOLDOWN`.
const GOSSIP_COOLDOWN: i64 = 1_200;
/// Vanilla `Villager.MAX_GOSSIP_TOPICS`.
const MAX_GOSSIP_TOPICS: i32 = 10;
/// Vanilla `allowedToRestock`: restocks a villager gets per day.
const MAX_RESTOCKS_PER_DAY: i32 = 2;
/// Vanilla `allowedToRestock`: ticks between the restocks of a day.
const RESTOCK_INTERVAL: i64 = 2_400;
/// Vanilla `shouldRestock`: ticks after a restock that count as a new day.
const HALF_DAY: i64 = 12_000;
/// Vanilla `VillagerProfession.requestedItems`: only farmers ask for wheat,
/// seeds and bone meal.
pub(super) fn requested_items(profession: VillagerProfessionRef) -> Vec<ItemRef> {
    if profession.key == vanilla_villager_professions::FARMER.key {
        vec![
            &vanilla_items::WHEAT,
            &vanilla_items::WHEAT_SEEDS,
            &vanilla_items::BEETROOT_SEEDS,
            &vanilla_items::BONE_MEAL,
        ]
    } else {
        Vec::new()
    }
}

fn is_requested_item(profession: VillagerProfessionRef, item: ItemRef) -> bool {
    requested_items(profession)
        .into_iter()
        .any(|requested| requested.key == item.key)
}

/// Vanilla `VillagerProfession.secondaryPoi`: the blocks a profession
/// walks between while working; only farmers have them.
pub(super) fn secondary_poi(profession: VillagerProfessionRef) -> &'static [BlockRef] {
    static FARMER_SECONDARY_POI: [BlockRef; 1] = [&vanilla_blocks::FARMLAND];
    if profession.key == vanilla_villager_professions::FARMER.key {
        &FARMER_SECONDARY_POI
    } else {
        &[]
    }
}

/// The plain fields of vanilla `Villager`.
#[derive(Debug, Default)]
struct VillagerState {
    food_level: i32,
    gossips: GossipContainer,
    last_gossip_decay_time: i64,
    villager_xp: i32,
    last_restock_game_time: i64,
    number_of_restocks_today: i32,
    /// Vanilla `lastRestockCheckDay`: not saved, like vanilla.
    last_restock_check_day: i64,
    /// Vanilla `AbstractVillager.offers`: `None` until first needed.
    offers: Option<MerchantOffers>,
    /// Vanilla `AbstractVillager.tradingPlayer`.
    trading_player: Option<Uuid>,
    /// Vanilla `lastTradedPlayer`: rewarded with gossip on the next AI step.
    last_traded_player: Option<Uuid>,
    /// Vanilla `lastGossipTime`: not saved, like vanilla.
    last_gossip_time: i64,
    /// A behavior changed the profession mid-tick; vanilla rebuilds the
    /// brain right there, Steel once the brain has finished ticking.
    brain_refresh_pending: bool,
}

/// A villager.
#[entity_behavior(class = "Villager", spawn_rule)]
pub struct VillagerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    entity_data: SyncMutex<VillagerEntityData>,
    inventory: SyncMutex<SimpleContainer>,
    state: SyncMutex<VillagerState>,
    brain: Brain<Self>,
}

// SAFETY: The owner-scoped type key uniquely identifies VillagerEntity.
unsafe impl DowncastType for VillagerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/villager");
}

impl VillagerEntity {
    /// Vanilla `SpawnPlacements` predicate: `Mob.checkMobSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Creates a new villager.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved villager.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::FireInNeighbor, FIRE_IN_NEIGHBOR_MALUS);
            malus.set(PathType::Fire, FIRE_MALUS);
        }
        let follow_range = living_base
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(DEFAULT_FOLLOW_RANGE);
        {
            let mut navigation = mob_base.navigation().lock();
            navigation.set_can_open_doors(true);
            navigation.set_can_float(true);
            navigation.set_required_path_length(REQUIRED_PATH_LENGTH, follow_range);
        }
        *mob_base.can_pick_up_loot().lock() = true;
        let mut entity_data = VillagerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let brain = brain::make_brain(false, &vanilla_villager_professions::NONE);
        brain.set_schedule(brain::schedule_for(false));

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            entity_data: SyncMutex::new(entity_data),
            inventory: SyncMutex::new(SimpleContainer::new(INVENTORY_SIZE)),
            state: SyncMutex::new(VillagerState::default()),
            brain,
        }
    }

    /// Vanilla `refreshBrain` and `registerBrainGoals`: rebuilds the brain
    /// for the villager's age, keeping the saved memories, and picks up its
    /// schedule right away.
    fn refresh_brain(&self) {
        let baby = AgeableMob::is_baby(self);
        let world = self.level();
        if let Some(world) = &world {
            self.brain.stop_all(world, self);
        }
        let new_brain = brain::make_brain(baby, self.profession());
        // Vanilla `BRAIN_PROVIDER.makeBrain(this, oldBrain.pack())`.
        new_brain.load_packed(&self.brain.save());
        new_brain.set_schedule(brain::schedule_for(baby));
        self.brain.replace_with(new_brain);
        if let Some(world) = &world {
            self.brain.update_activity_from_schedule(world);
        }
    }

    /// Asks for [`Self::refresh_brain`] once the brain has finished this
    /// tick, for behaviors that change the profession while it ticks.
    fn request_brain_refresh(&self) {
        self.state.lock().brain_refresh_pending = true;
    }

    /// Vanilla `releasePoi`: frees the POI remembered in `memory`, if it
    /// still is the kind of POI that memory holds.
    fn release_poi(&self, memory: &'static MemoryModuleType<GlobalPos>) {
        let Some(world) = self.level() else {
            return;
        };
        let Some(global) = self.brain.memories().lock().get(memory) else {
            return;
        };
        // Vanilla looks the POI up in its own dimension; a villager only
        // reaches its own world's POIs, which is where it claimed them.
        if global.dimension != world.key {
            return;
        }
        let mut storage = world.poi_storage.lock();
        let Some(poi_type) = storage.get_type(global.pos) else {
            return;
        };
        let matches = if ptr::eq(memory, &raw const HOME) {
            brain::is_home(poi_type)
        } else if ptr::eq(memory, &raw const JOB_SITE) {
            brain::held_job_site(self.profession(), poi_type)
        } else if ptr::eq(memory, &raw const POTENTIAL_JOB_SITE) {
            brain::is_acquirable_job(poi_type)
        } else if ptr::eq(memory, &raw const MEETING_POINT) {
            brain::is_meeting(poi_type)
        } else {
            false
        };
        if matches {
            let _released = storage.release_ticket(global.pos);
        }
    }

    /// Vanilla `releaseAllPois`.
    fn release_all_pois(&self) {
        self.release_poi(&HOME);
        self.release_poi(&JOB_SITE);
        self.release_poi(&POTENTIAL_JOB_SITE);
        self.release_poi(&MEETING_POINT);
    }

    /// Vanilla `getVillagerData().type()`.
    #[must_use]
    pub fn villager_type(&self) -> VillagerTypeRef {
        let id = self.entity_data.lock().villager_data.get().villager_type;
        usize::try_from(id)
            .ok()
            .and_then(|id| REGISTRY.villager_types.by_id(id))
            .unwrap_or(&vanilla_villager_types::PLAINS)
    }

    /// Vanilla `getVillagerData().profession()`.
    #[must_use]
    pub fn profession(&self) -> VillagerProfessionRef {
        let id = self.entity_data.lock().villager_data.get().profession;
        usize::try_from(id)
            .ok()
            .and_then(|id| REGISTRY.villager_professions.by_id(id))
            .unwrap_or(&vanilla_villager_professions::NONE)
    }

    /// Vanilla `getVillagerData().level()`.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.entity_data.lock().villager_data.get().level
    }

    /// Vanilla `setVillagerData`; the level is clamped like the
    /// `VillagerData` constructor.
    pub fn set_villager_data(
        &self,
        villager_type: VillagerTypeRef,
        profession: VillagerProfessionRef,
        level: i32,
    ) {
        let type_id = REGISTRY.villager_types.id_from_key(&villager_type.key);
        let profession_id = REGISTRY.villager_professions.id_from_key(&profession.key);
        let (Some(type_id), Some(profession_id)) = (type_id, profession_id) else {
            log::error!(
                "villager data {} / {} is not registered",
                villager_type.key,
                profession.key
            );
            return;
        };
        let (Ok(type_id), Ok(profession_id)) =
            (i32::try_from(type_id), i32::try_from(profession_id))
        else {
            return;
        };
        let mut data = *self.entity_data.lock().villager_data.get();
        // Vanilla `setVillagerData`: a new profession drops the old offers.
        if data.profession != profession_id {
            self.state.lock().offers = None;
        }
        data.villager_type = type_id;
        data.profession = profession_id;
        data.level = level.max(MIN_VILLAGER_LEVEL);
        self.entity_data.lock().villager_data.set(data);
    }

    fn set_villager_type(&self, villager_type: VillagerTypeRef) {
        self.set_villager_data(villager_type, self.profession(), self.villager_level());
    }

    fn set_profession(&self, profession: VillagerProfessionRef) {
        self.set_villager_data(self.villager_type(), profession, self.villager_level());
    }

    /// Vanilla `getVillagerDataFinalized`.
    #[must_use]
    pub fn villager_data_finalized(&self) -> bool {
        *self.entity_data.lock().villager_data_finalized.get()
    }

    /// Vanilla `setVillagerDataFinalized`.
    pub fn set_villager_data_finalized(&self, finalized: bool) {
        self.entity_data
            .lock()
            .villager_data_finalized
            .set(finalized);
    }

    /// Vanilla `VillagerDataHolder.finalizeVillagerType`.
    fn finalize_villager_type(&self, world: &World, pos: BlockPos) {
        if !self.villager_data_finalized() {
            self.set_villager_type(villager_type_at(world, pos));
            self.set_villager_data_finalized(true);
        }
    }

    /// Vanilla `AbstractVillager.getUnhappyCounter`.
    #[must_use]
    pub fn unhappy_counter(&self) -> i32 {
        *self
            .entity_data
            .lock()
            .abstract_villager
            .unhappy_counter
            .get()
    }

    /// Vanilla `AbstractVillager.setUnhappyCounter`.
    pub fn set_unhappy_counter(&self, value: i32) {
        self.entity_data
            .lock()
            .abstract_villager
            .unhappy_counter
            .set(value);
    }

    /// Vanilla `setUnhappy`: shakes its head and says no.
    fn set_unhappy(&self) {
        self.set_unhappy_counter(UNHAPPY_TICKS);
        self.make_sound(Some(&sound_events::ENTITY_VILLAGER_NO));
    }

    /// Vanilla `Villager.hasFarmSeeds`.
    fn has_farm_seeds(&self) -> bool {
        use crate::inventory::container::Container as _;
        let inventory = self.inventory.lock();
        (0..inventory.get_container_size()).any(|slot| {
            let stack = inventory.get_item(slot);
            !stack.is_empty() && stack.item().has_tag(&ItemTag::VILLAGER_PLANTABLE_SEEDS)
        })
    }

    /// Vanilla `AbstractVillager.getInventory`.
    #[must_use]
    pub const fn inventory(&self) -> &SyncMutex<SimpleContainer> {
        &self.inventory
    }

    /// Vanilla `getVillagerXp`.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.state.lock().villager_xp
    }

    /// Vanilla `setVillagerXp`.
    pub fn set_villager_xp(&self, value: i32) {
        self.state.lock().villager_xp = value;
    }

    /// Vanilla `getPlayerReputation`.
    #[must_use]
    pub fn player_reputation(&self, player: Uuid) -> i32 {
        self.state.lock().gossips.reputation(player, |_| true)
    }

    /// Vanilla `getGossips`, as a copy.
    #[must_use]
    pub fn gossips(&self) -> GossipContainer {
        self.state.lock().gossips.clone()
    }

    /// Vanilla `setGossips`: merges `gossips` into what this villager knows.
    pub fn set_gossips(&self, gossips: &GossipContainer) {
        self.state.lock().gossips.put_all(gossips);
    }

    /// Vanilla `Villager.gossip`: hears up to ten pieces of the other
    /// villager's gossip, at most once a minute for each of them.
    pub(super) fn gossip(&self, target: &Self, timestamp: i64) {
        let ready = |last: i64| timestamp < last || timestamp >= last + GOSSIP_COOLDOWN;
        let target_gossips = {
            let target_state = target.state.lock();
            if !ready(target_state.last_gossip_time) {
                return;
            }
            target_state.gossips.clone()
        };
        let new_gossip = {
            let mut state = self.state.lock();
            if !ready(state.last_gossip_time) {
                return;
            }
            let mut random = Xoroshiro::from_seed(rand::random());
            let new_gossip =
                state
                    .gossips
                    .transfer_from(&target_gossips, &mut random, MAX_GOSSIP_TOPICS);
            state.last_gossip_time = timestamp;
            new_gossip
        };
        target.state.lock().last_gossip_time = timestamp;
        // TODO(villager brain): vanilla `spawnGolemIfNeeded(level, timestamp, 5)`.
        if new_gossip > 0
            && let Some(player) = trading_player_of(self)
        {
            self.update_special_prices(&player);
        }
    }

    /// Vanilla `countFoodPointsInInventory`.
    fn count_food_points_in_inventory(&self) -> i32 {
        use crate::inventory::container::Container as _;
        let inventory = self.inventory.lock();
        (0..inventory.get_container_size())
            .map(|slot| {
                let stack = inventory.get_item(slot);
                stack
                    .get(VILLAGER_FOOD)
                    .map_or(0, |food| stack.count() * food.nutrition())
            })
            .sum()
    }

    /// Whether villagers count `stack` as food (vanilla `VILLAGER_FOOD`).
    pub(super) fn is_villager_food(stack: &ItemStack) -> bool {
        stack.get(VILLAGER_FOOD).is_some()
    }

    /// Vanilla `hasExcessFood`.
    pub(super) fn has_excess_food(&self) -> bool {
        self.count_food_points_in_inventory() >= 24
    }

    /// Vanilla `wantsMoreFood`.
    pub(super) fn wants_more_food(&self) -> bool {
        self.count_food_points_in_inventory() < 12
    }

    /// Vanilla `Villager.canBreed`: fed enough, awake and grown up.
    pub(super) fn can_breed(&self) -> bool {
        let food_level = self.state.lock().food_level;
        food_level + self.count_food_points_in_inventory() >= 12
            && !self.is_sleeping()
            && AgeableMob::get_age(self) == 0
    }

    /// Vanilla `eatAndDigestFood`: eats from the inventory until full, then
    /// uses up twelve food points.
    pub(super) fn eat_and_digest_food(&self) {
        self.eat_until_full();
        self.state.lock().food_level -= 12;
    }

    /// Vanilla `eatUntilFull`.
    fn eat_until_full(&self) {
        use crate::inventory::container::Container as _;
        if self.state.lock().food_level >= 12 {
            return;
        }
        let mut inventory = self.inventory.lock();
        for slot in 0..inventory.get_container_size() {
            let (nutrition, item_count) = {
                let stack = inventory.get_item(slot);
                let Some(food) = stack.get(VILLAGER_FOOD) else {
                    continue;
                };
                (food.nutrition(), stack.count())
            };
            let mut state = self.state.lock();
            let mut to_remove = 0;
            for _ in 0..item_count {
                state.food_level += nutrition;
                to_remove += 1;
                if state.food_level >= 12 {
                    drop(state);
                    inventory.remove_item(slot, to_remove);
                    return;
                }
            }
            drop(state);
            inventory.remove_item(slot, to_remove);
        }
    }

    /// Vanilla `onReputationEventFrom`.
    pub fn on_reputation_event_from(&self, event: ReputationEventType, source: Uuid) {
        let mut state = self.state.lock();
        match event {
            ReputationEventType::ZombieVillagerCured => {
                state.gossips.add(source, GossipType::MajorPositive, 20);
                state.gossips.add(source, GossipType::MinorPositive, 25);
            }
            ReputationEventType::Trade => state.gossips.add(source, GossipType::Trading, 2),
            ReputationEventType::VillagerHurt => {
                state.gossips.add(source, GossipType::MinorNegative, 25);
            }
            ReputationEventType::VillagerKilled => {
                state.gossips.add(source, GossipType::MajorNegative, 25);
            }
            ReputationEventType::GolemKilled => {}
        }
        drop(state);
        if let Some(player) = trading_player_of(self)
            && player.uuid() == source
        {
            self.update_special_prices(&player);
        }
    }

    /// Vanilla `maybeDecayGossip`.
    fn maybe_decay_gossip(&self, game_time: i64) {
        let mut state = self.state.lock();
        if state.last_gossip_decay_time == 0 {
            state.last_gossip_decay_time = game_time;
        } else if game_time >= state.last_gossip_decay_time + GOSSIP_DECAY_INTERVAL {
            state.gossips.decay();
            state.last_gossip_decay_time = game_time;
        }
    }

    /// Vanilla `AbstractVillager.getOffers`: rolls the first trades on first use.
    pub fn with_offers<T>(&self, f: impl FnOnce(&mut MerchantOffers) -> T) -> T {
        let missing = self.state.lock().offers.is_none();
        if missing {
            let mut offers = MerchantOffers::new();
            self.update_trades(&mut offers);
            let mut state = self.state.lock();
            let offers = state.offers.get_or_insert(offers);
            return f(offers);
        }
        let mut state = self.state.lock();
        let offers = state.offers.get_or_insert_with(MerchantOffers::new);
        f(offers)
    }

    /// Vanilla `Villager.updateTrades`: adds the trade set of the current
    /// profession and level. Callers that store the offers re-price them for
    /// the trading player afterwards, see [`Self::increase_merchant_career`].
    fn update_trades(&self, offers: &mut MerchantOffers) {
        let Some(trade_set) = profession_trade_set(self.profession(), self.villager_level()) else {
            return;
        };
        // Vanilla seeds the trade set's random sequence from the world; Steel has no
        // random sequences yet, so every roll gets a fresh seed.
        let mut random = Xoroshiro::from_seed(rand::random());
        let mut ctx = TradeContext {
            random: &mut random,
            merchant_variant: Some(self.villager_type()),
        };
        add_offers_from_trade_set(&mut ctx, offers, trade_set);
    }

    /// Vanilla `Villager.increaseMerchantCareer`: levels up and adds that level's trades.
    pub fn increase_merchant_career(&self) {
        self.set_villager_data(
            self.villager_type(),
            self.profession(),
            self.villager_level() + 1,
        );
        // Vanilla `updateTrades` goes through `getOffers`, which first rolls the
        // new level's trades when the villager never had offers.
        self.with_offers(|_| ());
        let mut offers = self.state.lock().offers.take().unwrap_or_default();
        self.update_trades(&mut offers);
        self.state.lock().offers = Some(offers);
        if let Some(player) = trading_player_of(self) {
            self.update_special_prices(&player);
        }
    }

    /// Vanilla `Villager.startTrading`.
    fn start_trading(&self, player: &Player) {
        self.update_special_prices(player);
        Merchant::set_trading_player(self, Some(player.uuid()));
        let Some(merchant) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return;
        };
        open_trading_screen(merchant, player, self.display_name(), self.villager_level());
    }

    /// Vanilla `Villager.stopTrading`.
    fn stop_trading(&self) {
        Merchant::set_trading_player(self, None);
    }

    /// Vanilla `resetSpecialPrices`.
    fn reset_special_prices(&self) {
        self.with_offers(|offers| {
            for offer in offers.iter_mut() {
                offer.reset_special_price_diff();
            }
        });
    }

    /// Vanilla `updateSpecialPrices`: reputation and Hero of the Village
    /// discounts for `player`, then the trading player's screen is refreshed.
    fn update_special_prices(&self, player: &Player) {
        self.reset_special_prices();
        let reputation = self.player_reputation(player.uuid());
        let hero_modifier = player
            .mob_effect(vanilla_mob_effects::HERO_OF_THE_VILLAGE)
            .map_or(0.0, |hero| {
                f64::from(0.3_f32 + 0.0625_f32 * hero.amplifier() as f32)
            });
        let has_offers = self.with_offers(|offers| {
            for offer in offers.iter_mut() {
                if reputation != 0 {
                    let discount = (reputation as f32 * offer.price_multiplier()).floor() as i32;
                    offer.add_to_special_price_diff(-discount);
                }
                if hero_modifier > 0.0 {
                    let cost_reduction =
                        (hero_modifier * f64::from(offer.base_cost_a().count())).floor() as i32;
                    offer.add_to_special_price_diff(-cost_reduction.max(1));
                }
            }
            !offers.is_empty()
        });

        if has_offers
            && let Some(trading_player) = trading_player_of(self)
            && let Some(container_id) = trading_player.update_merchant_sell_item()
        {
            send_merchant_offers(&trading_player, container_id, self, self.villager_level());
        }
    }

    /// Vanilla `shouldIncreaseLevel`.
    fn should_increase_level(&self) -> bool {
        let level = self.villager_level();
        can_level_up(level) && self.villager_xp() >= max_xp_per_level(level)
    }

    /// Vanilla `Villager.rewardTradeXp`.
    fn reward_trade_xp(&self, offer_xp: i32, reward_exp: bool) {
        let mut pop_xp = 3 + rand::random_range(0..4);
        {
            let mut state = self.state.lock();
            state.villager_xp += offer_xp;
            state.last_traded_player = state.trading_player;
        }
        if self.should_increase_level() {
            self.increase_merchant_career();
            self.add_mob_effect(MobEffectInstance::with_duration(
                vanilla_mob_effects::REGENERATION,
                LEVEL_UP_REGENERATION_TICKS,
                0,
            ));
            pop_xp += 5;
        }
        if reward_exp && let Some(world) = self.level() {
            spawn_trade_experience(&world, self.position(), pop_xp);
        }
    }

    /// Vanilla `playWorkSound`.
    pub fn play_work_sound(&self) {
        self.make_sound(self.profession().work_sound);
    }

    /// Vanilla `restock`: refills every offer, once per call.
    pub fn restock(&self, game_time: i64) {
        self.update_demand();
        self.with_offers(|offers| {
            for offer in offers.iter_mut() {
                offer.reset_uses();
            }
        });
        if let Some(player) = trading_player_of(self) {
            self.update_special_prices(&player);
        }
        let mut state = self.state.lock();
        state.last_restock_game_time = game_time;
        state.number_of_restocks_today += 1;
    }

    /// Vanilla `needsToRestock`.
    fn needs_to_restock(&self) -> bool {
        self.with_offers(|offers| offers.iter().any(MerchantOffer::needs_restock))
    }

    /// Vanilla `allowedToRestock`: twice a day, two minutes apart.
    fn allowed_to_restock(&self, game_time: i64) -> bool {
        let state = self.state.lock();
        state.number_of_restocks_today == 0
            || state.number_of_restocks_today < MAX_RESTOCKS_PER_DAY
                && game_time > state.last_restock_game_time + RESTOCK_INTERVAL
    }

    /// Vanilla `shouldRestock`: a new day (half a day since the last
    /// restock, or a new overworld day) resets the restock count first.
    pub fn should_restock(&self, world: &World) -> bool {
        let game_time = world.game_time();
        let current_day = world
            .clock_total_ticks(vanilla_timelines::DAY.clock)
            .map_or(0, |ticks| {
                i64::from(vanilla_timelines::DAY.period_count(ticks))
            });
        let is_new_day = {
            let mut state = self.state.lock();
            let is_new_day = game_time > state.last_restock_game_time + HALF_DAY
                || state.last_restock_check_day > 0 && current_day > state.last_restock_check_day;
            state.last_restock_check_day = current_day;
            if is_new_day {
                state.last_restock_game_time = game_time;
            }
            is_new_day
        };
        if is_new_day {
            self.reset_number_of_restocks();
        }
        self.allowed_to_restock(game_time) && self.needs_to_restock()
    }

    /// Vanilla `catchUpDemand`: the demand updates of the restocks it missed.
    fn catch_up_demand(&self) {
        let missed_updates = MAX_RESTOCKS_PER_DAY - self.state.lock().number_of_restocks_today;
        if missed_updates > 0 {
            self.with_offers(|offers| {
                for offer in offers.iter_mut() {
                    offer.reset_uses();
                }
            });
        }
        for _ in 0..missed_updates {
            self.update_demand();
        }
        if let Some(player) = trading_player_of(self) {
            self.update_special_prices(&player);
        }
    }

    /// Vanilla `updateDemand`.
    fn update_demand(&self) {
        self.with_offers(|offers| {
            for offer in offers.iter_mut() {
                offer.update_demand();
            }
        });
    }

    /// Vanilla `resetNumberOfRestocks`.
    fn reset_number_of_restocks(&self) {
        self.catch_up_demand();
        self.state.lock().number_of_restocks_today = 0;
    }

    /// Vanilla `AbstractVillager.playCelebrateSound`.
    pub fn play_celebrate_sound(&self) {
        self.make_sound(Some(&sound_events::ENTITY_VILLAGER_CELEBRATE));
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    fn save_villager(&self, nbt: &mut NbtCompound) {
        write_villager_data(
            nbt,
            self.villager_type(),
            self.profession(),
            self.villager_level(),
        );
        nbt.insert(
            "VillagerDataFinalized",
            i8::from(self.villager_data_finalized()),
        );
        let state = self.state.lock();
        // Vanilla `putByte`: the food level wraps to a byte.
        nbt.insert("FoodLevel", state.food_level as i8);
        nbt.insert("Gossips", state.gossips.to_nbt());
        nbt.insert("Xp", state.villager_xp);
        nbt.insert("LastRestock", state.last_restock_game_time);
        nbt.insert("LastGossipDecay", state.last_gossip_decay_time);
        nbt.insert("RestocksToday", state.number_of_restocks_today);
        if let Some(offers) = &state.offers {
            nbt.insert(TAG_OFFERS, NbtTag::Compound(offers.to_nbt()));
        }
    }

    fn load_villager(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let villager_data = read_villager_data(nbt);
        let finalized = nbt
            .byte("VillagerDataFinalized")
            .is_some_and(|value| value != 0);
        if finalized || villager_data.is_some() {
            self.set_villager_data_finalized(true);
            let (villager_type, profession, level) = villager_data.unwrap_or((
                &vanilla_villager_types::PLAINS,
                &vanilla_villager_professions::NONE,
                MIN_VILLAGER_LEVEL,
            ));
            self.set_villager_data(villager_type, profession, level);
        }

        let gossips = nbt
            .list("Gossips")
            .and_then(|list| list.compounds())
            .and_then(GossipContainer::from_nbt);
        let mut state = self.state.lock();
        state.food_level = i32::from(nbt.byte("FoodLevel").unwrap_or(0));
        state.gossips.clear();
        if let Some(gossips) = gossips {
            state.gossips.put_all(&gossips);
        }
        state.villager_xp = nbt.int("Xp").unwrap_or(0);
        state.last_restock_game_time = nbt.long("LastRestock").unwrap_or(0);
        state.last_gossip_decay_time = nbt.long("LastGossipDecay").unwrap_or(0);
        state.number_of_restocks_today = nbt.int("RestocksToday").unwrap_or(0);
        state.offers = nbt.compound(TAG_OFFERS).and_then(MerchantOffers::from_nbt);
        drop(state);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        self.refresh_brain();
    }
}

impl DefaultEntityPush for VillagerEntity {}

impl Merchant for VillagerEntity {
    fn trading_player(&self) -> Option<Uuid> {
        self.state.lock().trading_player
    }

    /// Vanilla `Villager.setTradingPlayer`: clearing it stops trading.
    fn set_trading_player(&self, player: Option<Uuid>) {
        let should_stop = {
            let mut state = self.state.lock();
            let should_stop = state.trading_player.is_some() && player.is_none();
            state.trading_player = player;
            should_stop
        };
        if should_stop {
            self.reset_special_prices();
        }
    }

    fn with_merchant_offers(&self, f: &mut dyn FnMut(&mut MerchantOffers)) {
        self.with_offers(|offers| f(offers));
    }

    fn notify_trade(&self, offer: usize) {
        // Vanilla also fires the `TRADE` advancement trigger; Steel has no advancements.
        if let Some((xp, reward_exp)) = self.with_offers(|offers| record_trade(self, offers, offer))
        {
            self.reward_trade_xp(xp, reward_exp);
        }
    }

    fn notify_trade_updated(&self, result: &ItemStack) {
        play_trade_updated_sound(
            self,
            result,
            &sound_events::ENTITY_VILLAGER_YES,
            &sound_events::ENTITY_VILLAGER_NO,
        );
    }

    fn merchant_xp(&self) -> i32 {
        self.villager_xp()
    }

    fn can_restock(&self) -> bool {
        true
    }
}

impl Entity for VillagerEntity {
    /// Vanilla `Villager.getTypeName`: the profession's name.
    fn name(&self) -> TextComponent {
        self.custom_name().map_or_else(
            || {
                let profession = &self.profession().key;
                TextComponent::translated(TranslatedMessage {
                    key: Cow::Owned(format!(
                        "entity.{}.villager.{}",
                        profession.namespace, profession.path
                    )),
                    fallback: None,
                    args: None,
                })
            },
            remove_entity_name_actions,
        )
    }

    /// Vanilla `AbstractVillager.teleport`: stops trading.
    fn on_teleported(&self) {
        self.stop_trading();
    }

    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
        let unhappy_counter = self.unhappy_counter();
        if unhappy_counter > 0 {
            self.set_unhappy_counter(unhappy_counter - 1);
        }
        if let Some(world) = self.level() {
            self.maybe_decay_gossip(world.game_time());
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(VILLAGER_VARIANT) {
            self.set_villager_type(variant.value());
            self.set_villager_data_finalized(true);
        }
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            BABY_DIMENSIONS.scale(scale)
        } else if self.entity_type.fixed {
            self.entity_type.dimensions
        } else {
            self.entity_type.dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn max_up_step(&self) -> f32 {
        self.attributes()
            .lock()
            .get_value(vanilla_attributes::STEP_HEIGHT)
            .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn thunder_hit(&self, world: &World, lightning: &LightningBoltEntity) {
        if world.difficulty() == Difficulty::Peaceful {
            self.default_thunder_hit(world);
            return;
        }
        log::info!(
            "Villager {} was struck by lightning {}.",
            self.uuid(),
            lightning.uuid()
        );
        let Some(level) = self.level() else {
            self.default_thunder_hit(world);
            return;
        };
        let witch = self.convert_to(
            &vanilla_entities::WITCH,
            ConversionParams::single(false, false),
            Some(&|witch: &dyn Mob| {
                let _ = witch.finalize_spawn(&level, EntitySpawnReason::Conversion, None);
                witch.set_persistence_required();
                self.release_all_pois();
            }),
        );
        if witch.is_none() {
            self.default_thunder_hit(world);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        save_inventory(&self.inventory, nbt);
        nbt.insert("Brain", self.brain.save());
        self.save_villager(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        load_inventory(&self.inventory, nbt);
        self.load_villager(nbt);
    }
}

impl LivingEntity for VillagerEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VILLAGER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VILLAGER_DEATH)
    }

    /// Vanilla `AbstractVillager.die`: stops trading after dying.
    fn die(&self, source: &DamageSource) {
        // Vanilla `Villager.die` frees its POIs before `AbstractVillager.die`.
        self.release_all_pois();
        self.default_die(source);
        self.stop_trading();
    }

    fn set_last_hurt_by_mob(&self, target: Option<&SharedEntity>) {
        if let Some(attacker) = target {
            self.on_reputation_event_from(ReputationEventType::VillagerHurt, attacker.uuid());
            if LivingEntity::is_alive(self) && attacker.as_player().is_some() {
                self.broadcast_entity_event(EntityStatus::VillagerAngry);
            }
        }
        self.living_base
            .set_last_hurt_by_mob(target, self.tick_count());
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
    }

    /// Vanilla `Villager.stopSleeping`: remembers when it woke up.
    fn stop_sleeping(&self) {
        self.default_stop_sleeping();
        if let Some(world) = self.level() {
            self.brain
                .memories()
                .lock()
                .set(&LAST_WOKEN, Some(world.game_time()));
        }
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        result
    }
}

impl AgeableMob for VillagerEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob().age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob_mut()
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob_mut().baby.set(baby);
    }

    /// Vanilla `Villager.ageBoundaryReached`: babies and adults have
    /// different brains.
    fn age_boundary_changed(&self, _baby: bool) {
        self.refresh_dimensions();
        if self.level().is_some() {
            self.refresh_brain();
        }
    }

    fn breed_variant_key(&self) -> Option<&Identifier> {
        Some(&self.villager_type().key)
    }

    /// Vanilla `getBreedOffspring`: the baby gets the type of the biome, this
    /// villager or its partner, and no profession.
    fn set_breed_variant_key(&self, key: &Identifier) -> bool {
        let Some(villager_type) = REGISTRY.villager_types.by_key(key) else {
            return false;
        };
        self.set_villager_data(
            villager_type,
            &vanilla_villager_professions::NONE,
            self.villager_level(),
        );
        self.set_villager_data_finalized(true);
        true
    }

    fn initialize_breed_offspring(&self, partner: &dyn AgeableMob, offspring: &dyn AgeableMob) {
        let biome_roll = rand::random::<f64>();
        let villager_type = if biome_roll < 0.5 {
            match self.level() {
                Some(world) => villager_type_at(&world, self.block_position()),
                None => &vanilla_villager_types::PLAINS,
            }
        } else if biome_roll < 0.75 {
            self.villager_type()
        } else {
            partner
                .breed_variant_key()
                .and_then(|key| REGISTRY.villager_types.by_key(key))
                .unwrap_or_else(|| self.villager_type())
        };
        if !offspring.set_breed_variant_key(&villager_type.key) {
            log::error!(
                "villager offspring could not take type {}",
                villager_type.key
            );
        }
    }
}

impl Mob for VillagerEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if self.is_sleeping() {
            None
        } else if self.is_trading() {
            Some(&sound_events::ENTITY_VILLAGER_TRADE)
        } else {
            Some(&sound_events::ENTITY_VILLAGER_AMBIENT)
        }
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    fn mob_can_be_leashed(&self) -> bool {
        false
    }

    fn wants_to_pick_up(&self, item_stack: &ItemStack) -> bool {
        let item = item_stack.item();
        (item.has_tag(&ItemTag::VILLAGER_PICKS_UP)
            || item_stack.get(VILLAGER_FOOD).is_some()
            || is_requested_item(self.profession(), item))
            && self.inventory.lock().can_add_item(item_stack)
    }

    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        pick_up_item_into_inventory(self, &self.inventory, world, item_entity);
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if spawn_reason == EntitySpawnReason::Breeding {
            self.set_profession(&vanilla_villager_professions::NONE);
        }
        self.finalize_villager_type(world, self.block_position());
        // Vanilla `AbstractVillager.finalizeSpawn`: no random babies.
        let group_data = group_data.or(Some(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_should_spawn_baby(false),
        )));
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let holds_spawn_egg = player
            .inventory
            .lock()
            .get_item_in_hand(hand)
            .is(&vanilla_items::VILLAGER_SPAWN_EGG);
        if holds_spawn_egg
            || !LivingEntity::is_alive(self)
            || self.is_trading()
            || self.is_sleeping()
        {
            return self.mob_interact_ageable(player, hand);
        }
        if AgeableMob::is_baby(self) {
            self.set_unhappy();
            return InteractionResult::Success;
        }
        let no_offers = self.with_offers(|offers| offers.is_empty());
        if hand == InteractionHand::MainHand {
            if no_offers {
                self.set_unhappy();
            }
            player.award_custom_stat(&vanilla_custom_stats::TALKED_TO_VILLAGER);
        }
        if no_offers {
            return InteractionResult::Consume;
        }
        self.start_trading(player);
        InteractionResult::Success
    }

    /// Vanilla `Villager.customServerAiStep`.
    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
        }
        if mem::take(&mut self.state.lock().brain_refresh_pending) {
            self.refresh_brain();
        }
        let last_traded_player = self.state.lock().last_traded_player.take();
        if let Some(player) = last_traded_player {
            self.on_reputation_event_from(ReputationEventType::Trade, player);
            self.broadcast_entity_event(EntityStatus::VillagerHappy);
        }
        // TODO(raids): sweat (entity event 42) during an active raid.
        let no_offers = self
            .state
            .lock()
            .offers
            .as_ref()
            .is_none_or(MerchantOffers::is_empty);
        if no_offers && self.is_trading() {
            self.stop_trading();
        }
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for VillagerEntity {}

mod brain;

#[cfg(test)]
mod tests;
