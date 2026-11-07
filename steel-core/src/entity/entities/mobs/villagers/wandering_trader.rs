//! Wandering trader (Vanilla `WanderingTrader` with its `AbstractVillager` parts).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::PotionContents;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::WanderingTraderEntityData;
use steel_registry::villager_trade::trade_set;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_custom_stats, vanilla_entities, vanilla_items,
    vanilla_potions,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::xoroshiro::Xoroshiro;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey, Identifier};

use super::trading::{MerchantOffers, TradeContext, add_offers_from_trade_set};
use super::{FIRE_IN_NEIGHBOR_MALUS, FIRE_MALUS, INVENTORY_SIZE, load_inventory, save_inventory};
use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    AvoidEntityGoal, FloatGoal, Goal, GoalControls, InteractGoal, LookAtPlayerGoal,
    MoveTowardsRestrictionGoal, PanicGoal, UseItemGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entity_selector;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityEventSource as _, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, RemovalReason, SharedEntity, SpawnGroupData,
    SpawnRule, sync_dirty_mob_effects,
};
use crate::inventory::container::SimpleContainer;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

/// Vanilla `AbstractVillager` offers NBT key.
const TAG_OFFERS: &str = "Offers";
const TAG_DESPAWN_DELAY: &str = "DespawnDelay";
const TAG_WANDER_TARGET: &str = "wander_target";
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `WanderingTrader.registerGoals` speeds.
const AVOID_SPEED: f64 = 0.5;
const PANIC_SPEED: f64 = 0.5;
const WANDER_STOP_DISTANCE: f64 = 2.0;
const WANDER_SPEED: f64 = 0.35;
/// Vanilla `WanderToPositionGoal`: far targets are walked to 10 blocks at a time.
const WANDER_STEP: f64 = 10.0;

/// Vanilla `TradeSets.WANDERING_TRADER_*`, in the order `updateTrades` adds them.
const TRADE_SETS: [Identifier; 3] = [
    Identifier::vanilla_static("wandering_trader/buying"),
    Identifier::vanilla_static("wandering_trader/uncommon"),
    Identifier::vanilla_static("wandering_trader/common"),
];

/// The plain fields of vanilla `WanderingTrader`.
#[derive(Debug, Default)]
struct WanderingTraderState {
    wander_target: Option<BlockPos>,
    despawn_delay: i32,
    /// Vanilla `AbstractVillager.offers`: `None` until first needed.
    offers: Option<MerchantOffers>,
}

/// A wandering trader.
#[entity_behavior(class = "WanderingTrader", spawn_rule)]
pub struct WanderingTraderEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    entity_data: SyncMutex<WanderingTraderEntityData>,
    inventory: SyncMutex<SimpleContainer>,
    state: Arc<SyncMutex<WanderingTraderState>>,
}

// SAFETY: The owner-scoped type key uniquely identifies WanderingTraderEntity.
unsafe impl DowncastType for WanderingTraderEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/wandering_trader");
}

/// Vanilla `Zombie.class`: zombies and every subclass.
fn is_zombie(entity: &dyn LivingEntity) -> bool {
    [
        &vanilla_entities::ZOMBIE,
        &vanilla_entities::HUSK,
        &vanilla_entities::DROWNED,
        &vanilla_entities::ZOMBIE_VILLAGER,
        &vanilla_entities::ZOMBIFIED_PIGLIN,
    ]
    .into_iter()
    .any(|entity_type| entity.entity_type() == entity_type)
}

/// Vanilla `AvoidEntityGoal` for one entity class, with its default
/// `NO_CREATIVE_OR_SPECTATOR` predicate.
fn avoid_goal(
    max_dist: f32,
    is_avoided: impl Fn(&dyn LivingEntity) -> bool + Send + Sync + 'static,
) -> AvoidEntityGoal {
    AvoidEntityGoal::with_selector(max_dist, AVOID_SPEED, AVOID_SPEED, move |target, _| {
        is_avoided(target)
            && entity_selector::no_creative_or_spectator(target.as_entity_event_source())
    })
}

fn avoid_type_goal(max_dist: f32, avoided: EntityTypeRef) -> AvoidEntityGoal {
    avoid_goal(max_dist, move |target| target.entity_type() == avoided)
}

impl WanderingTraderEntity {
    /// Vanilla `SpawnPlacements` predicate: `Mob.checkMobSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Creates a new wandering trader.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved wandering trader.
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
        let mut entity_data = WanderingTraderEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let state = Arc::new(SyncMutex::new(WanderingTraderState::default()));

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(
                0,
                UseItemGoal::new(
                    PotionContents::create_item_stack(
                        &vanilla_items::POTION,
                        &vanilla_potions::INVISIBILITY,
                    ),
                    Some(&sound_events::ENTITY_WANDERING_TRADER_DISAPPEARED),
                    |mob| {
                        mob.level().is_some_and(|world| world.is_dark_outside())
                            && !mob.is_invisible()
                    },
                ),
            );
            goal_selector.add_goal(
                0,
                UseItemGoal::new(
                    ItemStack::new(&vanilla_items::MILK_BUCKET),
                    Some(&sound_events::ENTITY_WANDERING_TRADER_REAPPEARED),
                    |mob| {
                        mob.level().is_some_and(|world| world.is_bright_outside())
                            && mob.is_invisible()
                    },
                ),
            );
            // TODO(trading menu): `TradeWithPlayerGoal` 1 and `LookAtTradingPlayerGoal` 1
            // come with the trading screen.
            goal_selector.add_goal(1, avoid_goal(8.0, is_zombie));
            goal_selector.add_goal(1, avoid_type_goal(12.0, &vanilla_entities::EVOKER));
            goal_selector.add_goal(1, avoid_type_goal(8.0, &vanilla_entities::VINDICATOR));
            goal_selector.add_goal(1, avoid_type_goal(8.0, &vanilla_entities::VEX));
            goal_selector.add_goal(1, avoid_type_goal(15.0, &vanilla_entities::PILLAGER));
            goal_selector.add_goal(1, avoid_type_goal(12.0, &vanilla_entities::ILLUSIONER));
            goal_selector.add_goal(1, avoid_type_goal(10.0, &vanilla_entities::ZOGLIN));
            goal_selector.add_goal(1, PanicGoal::new(PANIC_SPEED));
            goal_selector.add_goal(
                2,
                WanderToPositionGoal {
                    state: Arc::clone(&state),
                    stop_distance: WANDER_STOP_DISTANCE,
                    speed_modifier: WANDER_SPEED,
                },
            );
            goal_selector.add_goal(4, MoveTowardsRestrictionGoal::new(WANDER_SPEED));
            goal_selector.add_goal(8, WaterAvoidingRandomStrollGoal::new(WANDER_SPEED));
            goal_selector.add_goal(9, InteractGoal::new_player(3.0, 1.0));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(8.0, 0.02, |target, _| {
                    target.as_mob().is_some()
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            entity_data: SyncMutex::new(entity_data),
            inventory: SyncMutex::new(SimpleContainer::new(INVENTORY_SIZE)),
            state,
        }
    }

    /// Vanilla `getDespawnDelay`.
    #[must_use]
    pub fn despawn_delay(&self) -> i32 {
        self.state.lock().despawn_delay
    }

    /// Vanilla `setDespawnDelay`.
    pub fn set_despawn_delay(&self, despawn_delay: i32) {
        self.state.lock().despawn_delay = despawn_delay;
    }

    /// Vanilla `setWanderTarget`.
    pub fn set_wander_target(&self, pos: Option<BlockPos>) {
        self.state.lock().wander_target = pos;
    }

    /// Vanilla `getWanderTarget`.
    #[must_use]
    pub fn wander_target(&self) -> Option<BlockPos> {
        self.state.lock().wander_target
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

    /// Vanilla `AbstractVillager.getInventory`.
    #[must_use]
    pub const fn inventory(&self) -> &SyncMutex<SimpleContainer> {
        &self.inventory
    }

    /// Vanilla `AbstractVillager.getOffers`: rolls the trades on first use.
    pub fn with_offers<T>(&self, f: impl FnOnce(&mut MerchantOffers) -> T) -> T {
        let mut state = self.state.lock();
        let offers = state.offers.get_or_insert_with(|| {
            let mut offers = MerchantOffers::new();
            Self::update_trades(&mut offers);
            offers
        });
        f(offers)
    }

    /// Vanilla `WanderingTrader.updateTrades`: one buying trade set, then the
    /// uncommon and the common ones.
    fn update_trades(offers: &mut MerchantOffers) {
        // Vanilla seeds the trade set's random sequence from the world; Steel has no
        // random sequences yet, so every roll gets a fresh seed.
        let mut random = Xoroshiro::from_seed(rand::random());
        let mut ctx = TradeContext {
            random: &mut random,
            merchant_variant: None,
        };
        for key in &TRADE_SETS {
            match trade_set(key) {
                Some(trade_set) => add_offers_from_trade_set(&mut ctx, offers, trade_set),
                None => log::debug!("Missing expected trade set {key}"),
            }
        }
    }

    /// Vanilla `maybeDespawn`.
    fn maybe_despawn(&self) {
        // TODO(trading menu): vanilla also waits while a player is trading.
        let despawn = {
            let mut state = self.state.lock();
            if state.despawn_delay > 0 {
                state.despawn_delay -= 1;
                state.despawn_delay == 0
            } else {
                false
            }
        };
        if despawn {
            self.set_removed(RemovalReason::Discarded);
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    fn save_wandering_trader(&self, nbt: &mut NbtCompound) {
        let state = self.state.lock();
        if let Some(offers) = &state.offers {
            nbt.insert(TAG_OFFERS, NbtTag::Compound(offers.to_nbt()));
        }
        save_inventory(&self.inventory, nbt);
        nbt.insert(TAG_DESPAWN_DELAY, state.despawn_delay);
        if let Some(pos) = state.wander_target {
            nbt.insert(
                TAG_WANDER_TARGET,
                NbtTag::IntArray(vec![pos.x(), pos.y(), pos.z()]),
            );
        }
    }

    fn load_wandering_trader(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let offers = nbt.compound(TAG_OFFERS).and_then(MerchantOffers::from_nbt);
        load_inventory(&self.inventory, nbt);
        let wander_target = nbt
            .int_array(TAG_WANDER_TARGET)
            .filter(|pos| pos.len() == 3)
            .map(|pos| BlockPos::new(pos[0], pos[1], pos[2]));
        {
            let mut state = self.state.lock();
            state.offers = offers;
            state.despawn_delay = nbt.int(TAG_DESPAWN_DELAY).unwrap_or(0);
            state.wander_target = wander_target;
        }
        AgeableMob::set_age(self, AgeableMob::get_age(self).max(0));
    }
}

/// Vanilla `WanderingTrader.WanderToPositionGoal`: walks to the wander target
/// the spawner gave, 10 blocks at a time.
struct WanderToPositionGoal {
    state: Arc<SyncMutex<WanderingTraderState>>,
    stop_distance: f64,
    speed_modifier: f64,
}

impl WanderToPositionGoal {
    fn is_too_far_away(mob: &dyn PathfinderMob, pos: BlockPos, distance: f64) -> bool {
        !pos.closer_to_center_than(mob.position(), distance)
    }
}

impl Goal for WanderToPositionGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.state
            .lock()
            .wander_target
            .is_some_and(|pos| Self::is_too_far_away(mob, pos, self.stop_distance))
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.state.lock().wander_target = None;
        mob.mob_base().navigation().lock().stop();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(pos) = self.state.lock().wander_target else {
            return;
        };
        if !mob.mob_base().navigation().lock().is_done() {
            return;
        }
        let target = DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()));
        if Self::is_too_far_away(mob, pos, WANDER_STEP) {
            let position = mob.position();
            let target = (target - position).normalize() * WANDER_STEP + position;
            mob.move_to_pos(target, self.speed_modifier);
        } else {
            mob.move_to_pos(target, self.speed_modifier);
        }
    }
}

impl DefaultEntityPush for WanderingTraderEntity {}

impl Entity for WanderingTraderEntity {
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
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
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

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_wandering_trader(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_wandering_trader(nbt);
    }
}

impl LivingEntity for WanderingTraderEntity {
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
        Some(&sound_events::ENTITY_WANDERING_TRADER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_WANDERING_TRADER_DEATH)
    }

    fn consume_sound_override(&self, stack: &ItemStack) -> Option<SoundEventRef> {
        Some(if stack.is(&vanilla_items::MILK_BUCKET) {
            &sound_events::ENTITY_WANDERING_TRADER_DRINK_MILK
        } else {
            &sound_events::ENTITY_WANDERING_TRADER_DRINK_POTION
        })
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        self.maybe_despawn();
        result
    }
}

impl AgeableMob for WanderingTraderEntity {
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

    fn age_boundary_changed(&self, _baby: bool) {
        self.refresh_dimensions();
    }
}

impl Mob for WanderingTraderEntity {
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
        // TODO(trading menu): `ENTITY_WANDERING_TRADER_TRADE` while trading.
        Some(&sound_events::ENTITY_WANDERING_TRADER_AMBIENT)
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    fn mob_can_be_leashed(&self) -> bool {
        false
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
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
        // TODO(trading menu): a trading wandering trader also goes to the ageable branch.
        if holds_spawn_egg || !LivingEntity::is_alive(self) || AgeableMob::is_baby(self) {
            return self.mob_interact_ageable(player, hand);
        }
        if hand == InteractionHand::MainHand {
            player.award_custom_stat(&vanilla_custom_stats::TALKED_TO_VILLAGER);
        }
        if self.with_offers(|offers| offers.is_empty()) {
            return InteractionResult::Consume;
        }
        // TODO(trading menu): set the trading player and open the trading screen.
        InteractionResult::Success
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for WanderingTraderEntity {}

#[cfg(test)]
mod tests;
