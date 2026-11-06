//! Vanilla `ZombieVillager`: a zombie that keeps the villager it once was.
//!
//! It carries the villager's data, trades, gossip and experience so that
//! curing can hand them back. Curing itself (the golden apple and the
//! conversion back into a villager) lives with the villager.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_registry::entity_data::VillagerData;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_entity_data::ZombieVillagerEntityData;
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::villager_type::VillagerTypeRef;
use steel_registry::{
    REGISTRY, RegistryExt as _, vanilla_mob_effects, vanilla_villager_professions,
    vanilla_villager_types,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{DowncastType, DowncastTypeKey, UuidExt as _};
use uuid::Uuid;

use super::{ZombieState, ZombieVariant, ZombieVariantSaveData, zombie_mob_base};
use crate::entity::ai::gossip::GossipContainer;
use crate::entity::entities::mobs::villagers::villager_data::{
    read_villager_data, villager_type_at, write_villager_data,
};
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySpawnReason, LivingEntity, LivingEntityBase, MobBase,
    MobEffectInstance, Monster, SpawnGroupData, SpawnRule,
};
use crate::world::World;

/// Vanilla `ZombieVillager.NOT_CONVERTING`.
const NOT_CONVERTING: i32 = -1;
/// Vanilla `ZombieVillager.DEFAULT_XP`.
const DEFAULT_XP: i32 = 0;

/// Server-only zombie villager state that is not synced to clients.
pub(super) struct ZombieVillagerState {
    /// Vanilla `villagerConversionTime`.
    conversion_time: i32,
    /// Vanilla `conversionStarter`.
    conversion_starter: Option<Uuid>,
    /// Vanilla `gossips`.
    gossips: Option<GossipContainer>,
    /// Vanilla `tradeOffers`, kept in its saved `MerchantOffers` form until
    /// merchant offers exist.
    trade_offers: Option<NbtTag>,
    /// Vanilla `villagerXp`.
    villager_xp: i32,
}

impl ZombieVillagerState {
    const fn new() -> Self {
        Self {
            conversion_time: NOT_CONVERTING,
            conversion_starter: None,
            gossips: None,
            trade_offers: None,
            villager_xp: DEFAULT_XP,
        }
    }
}

/// A hostile zombie villager.
#[entity_behavior(class = "ZombieVillager", spawn_rule)]
pub struct ZombieVillagerEntity {
    pub(super) base: EntityBase,
    pub(super) entity_type: EntityTypeRef,
    pub(super) living_base: LivingEntityBase,
    pub(super) mob_base: MobBase,
    pub(super) entity_data: SyncMutex<ZombieVillagerEntityData>,
    pub(super) zombie_state: SyncMutex<ZombieState>,
    villager_state: SyncMutex<ZombieVillagerState>,
}

// SAFETY: The owner-scoped type key uniquely identifies ZombieVillagerEntity.
unsafe impl DowncastType for ZombieVillagerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/zombie_villager");
}

impl ZombieVillagerEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new zombie villager.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved zombie villager.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        // Vanilla `defineSynchedData`: `initializeZombieVillagerData` picks a
        // random profession.
        let mut random = LegacyRandom::from_seed(rand::random());
        let mut entity_data = ZombieVillagerEntityData::new(&mut random);
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: zombie_mob_base(ZombieVariant::ZombieVillager),
            entity_data: SyncMutex::new(entity_data),
            zombie_state: SyncMutex::new(ZombieState::new()),
            villager_state: SyncMutex::new(ZombieVillagerState::new()),
        }
    }

    /// Vanilla `ZombieVillager.finalizeSpawn`: the biome villager type, then
    /// the zombie spawn.
    pub(super) fn finalize_spawn_zombie_villager(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_villager_type(world, self.block_position());
        let mut random = LegacyRandom::from_seed(rand::random());
        self.finalize_zombie_spawn(world, &mut random, spawn_reason, group_data)
    }

    /// Vanilla `ZombieVillager.isConverting`.
    #[must_use]
    pub fn is_converting(&self) -> bool {
        *self.entity_data.lock().converting.get()
    }

    /// Vanilla `ZombieVillager.startConverting`: the cure countdown, with
    /// strength for its length and the cure sound on clients.
    pub fn start_converting(&self, player: Option<Uuid>, time: i32) {
        {
            let mut state = self.villager_state.lock();
            state.conversion_starter = player;
            state.conversion_time = time;
        }
        self.entity_data.lock().converting.set(true);
        let _ = self.remove_mob_effect(vanilla_mob_effects::WEAKNESS);
        let difficulty = self
            .level()
            .map_or(0, |world| i32::from(world.difficulty() as u8));
        let _ = self.add_mob_effect(MobEffectInstance::with_duration(
            vanilla_mob_effects::STRENGTH,
            time,
            (difficulty - 1).min(0),
        ));
        self.broadcast_entity_event(EntityStatus::ZombieConverting);
    }

    /// Vanilla `villagerConversionTime`.
    #[must_use]
    pub fn villager_conversion_time(&self) -> i32 {
        self.villager_state.lock().conversion_time
    }

    /// Vanilla `ZombieVillager.setVillagerConversionTime`.
    pub fn set_villager_conversion_time(&self, conversion_time: i32) {
        self.villager_state.lock().conversion_time = conversion_time;
    }

    /// Vanilla `conversionStarter`: the player who started the cure.
    #[must_use]
    pub fn conversion_starter(&self) -> Option<Uuid> {
        self.villager_state.lock().conversion_starter
    }

    /// Vanilla `getVillagerXp`.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.villager_state.lock().villager_xp
    }

    /// Vanilla `setVillagerXp`.
    pub fn set_villager_xp(&self, villager_xp: i32) {
        self.villager_state.lock().villager_xp = villager_xp;
    }

    /// Vanilla `tradeOffers`, in its saved `MerchantOffers` form.
    #[must_use]
    pub fn trade_offers(&self) -> Option<NbtTag> {
        self.villager_state.lock().trade_offers.clone()
    }

    /// Vanilla `setTradeOffers`, in its saved `MerchantOffers` form.
    pub fn set_trade_offers(&self, trade_offers: Option<NbtTag>) {
        self.villager_state.lock().trade_offers = trade_offers;
    }

    /// Vanilla `gossips`.
    #[must_use]
    pub fn gossips(&self) -> Option<GossipContainer> {
        self.villager_state.lock().gossips.clone()
    }

    /// Vanilla `setGossips`.
    pub fn set_gossips(&self, gossips: GossipContainer) {
        self.villager_state.lock().gossips = Some(gossips);
    }

    /// Vanilla `ZombieVillager.removeWhenFarAway`.
    pub(super) fn remove_when_far_away_zombie_villager(&self) -> bool {
        !self.is_converting() && self.villager_xp() == 0
    }

    /// Vanilla `ZombieVillager.getVoicePitch`: babies sound an octave higher.
    pub(super) fn voice_pitch_zombie_villager(&self) -> f32 {
        let base = if LivingEntity::is_baby(self) {
            2.0
        } else {
            1.0
        };
        (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + base
    }
}

impl ZombieVillagerEntity {
    /// Vanilla `getVillagerData()`.
    #[must_use]
    pub fn villager_data(&self) -> VillagerData {
        *self.entity_data.lock().villager_data.get()
    }

    /// Vanilla `getVillagerData().type()`.
    #[must_use]
    pub fn villager_type(&self) -> VillagerTypeRef {
        usize::try_from(self.villager_data().villager_type)
            .ok()
            .and_then(|id| REGISTRY.villager_types.by_id(id))
            .unwrap_or(&vanilla_villager_types::PLAINS)
    }

    /// Vanilla `getVillagerData().profession()`.
    #[must_use]
    pub fn profession(&self) -> VillagerProfessionRef {
        usize::try_from(self.villager_data().profession)
            .ok()
            .and_then(|id| REGISTRY.villager_professions.by_id(id))
            .unwrap_or(&vanilla_villager_professions::NONE)
    }

    /// Vanilla `getVillagerData().level()`.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_data().level
    }

    /// Vanilla `ZombieVillager.setVillagerData`: a new profession drops the trades.
    pub fn set_villager_data(
        &self,
        villager_type: VillagerTypeRef,
        profession: VillagerProfessionRef,
        level: i32,
    ) {
        let type_id = REGISTRY
            .villager_types
            .id_from_key(&villager_type.key)
            .and_then(|id| i32::try_from(id).ok());
        let profession_id = REGISTRY
            .villager_professions
            .id_from_key(&profession.key)
            .and_then(|id| i32::try_from(id).ok());
        let (Some(type_id), Some(profession_id)) = (type_id, profession_id) else {
            log::error!(
                "villager data {} / {} is not registered",
                villager_type.key,
                profession.key
            );
            return;
        };
        if self.villager_data().profession != profession_id {
            self.villager_state.lock().trade_offers = None;
        }
        self.entity_data
            .lock()
            .villager_data
            .set(VillagerData::new(type_id, profession_id, level));
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
    fn finalize_villager_type(&self, world: &World, pos: steel_utils::BlockPos) {
        if !self.villager_data_finalized() {
            self.set_villager_data(
                villager_type_at(world, pos),
                self.profession(),
                self.villager_level(),
            );
            self.set_villager_data_finalized(true);
        }
    }
}

impl ZombieVariantSaveData for ZombieVillagerEntity {
    /// Vanilla `ZombieVillager.addAdditionalSaveData`.
    fn save_variant_data(&self, nbt: &mut NbtCompound) {
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
        let converting = self.is_converting();
        let state = self.villager_state.lock();
        if let Some(offers) = &state.trade_offers {
            nbt.insert("Offers", offers.clone());
        }
        if let Some(gossips) = &state.gossips {
            nbt.insert("Gossips", gossips.to_nbt());
        }
        nbt.insert(
            "ConversionTime",
            if converting {
                state.conversion_time
            } else {
                NOT_CONVERTING
            },
        );
        if let Some(starter) = state.conversion_starter {
            nbt.insert(
                "ConversionPlayer",
                NbtTag::IntArray(starter.to_int_array().to_vec()),
            );
        }
        nbt.insert("Xp", state.villager_xp);
    }

    /// Vanilla `ZombieVillager.readAdditionalSaveData`.
    fn load_variant_data(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let villager_data = read_villager_data(nbt);
        let finalized = nbt
            .byte("VillagerDataFinalized")
            .is_some_and(|value| value != 0);
        if finalized || villager_data.is_some() {
            self.set_villager_data_finalized(true);
            if let Some((villager_type, profession, level)) = villager_data {
                self.set_villager_data(villager_type, profession, level);
            } else {
                let mut random = LegacyRandom::from_seed(rand::random());
                let data = *ZombieVillagerEntityData::new(&mut random)
                    .villager_data
                    .get();
                self.entity_data.lock().villager_data.set(data);
            }
        }

        {
            let mut state = self.villager_state.lock();
            state.trade_offers = nbt.get("Offers").map(|tag| tag.to_owned());
            state.gossips = nbt
                .list("Gossips")
                .and_then(|list| list.compounds())
                .and_then(GossipContainer::from_nbt);
        }
        let conversion_time = nbt.int("ConversionTime").unwrap_or(NOT_CONVERTING);
        if conversion_time == NOT_CONVERTING {
            self.entity_data.lock().converting.set(false);
            self.villager_state.lock().conversion_time = NOT_CONVERTING;
        } else {
            let starter = nbt
                .int_array("ConversionPlayer")
                .and_then(|array| Uuid::from_int_array(&array));
            self.start_converting(starter, conversion_time);
        }
        self.villager_state.lock().villager_xp = nbt.int("Xp").unwrap_or(DEFAULT_XP);
    }
}
