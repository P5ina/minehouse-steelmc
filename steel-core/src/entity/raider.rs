//! Raid members (Vanilla `Raider`) and the ominous banner they carry.
//!
//! The raid itself lives in [`crate::world::raid`]; a raider keeps a handle to
//! its current raid the way vanilla keeps the `Raid` object.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::CTakeItemEntity;
use steel_registry::data_components::components::{Rarity, TooltipDisplay};
use steel_registry::data_components::vanilla_components::{
    BANNER_PATTERNS, ITEM_NAME, RARITY, TOOLTIP_DISPLAY,
};
use steel_registry::data_components::{BannerPatternLayer, BannerPatternLayers};
use steel_registry::dye_color::DyeColor;
use steel_registry::item_stack::ItemStack;
use steel_registry::registry::RegistryHolder;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{vanilla_banner_patterns, vanilla_entities, vanilla_items};
use steel_utils::ChunkPos;
use steel_utils::locks::SyncMutex;
use steel_utils::translations;
use text_components::TextComponent;

use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::{
    Entity as _, EntitySpawnReason, LivingEntity, PatrollingMonster, RemovalReason, SharedEntity,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::world::World;
use crate::world::raid::{Raid, Raids};

const TAG_WAVE: &str = "Wave";
const TAG_CAN_JOIN_RAID: &str = "CanJoinRaid";
const TAG_RAID_ID: &str = "RaidId";
/// Vanilla `Raider.updateNoActionTime`: raiders idle twice as fast as other mobs.
const RAIDER_NO_ACTION_TIME_INCREMENT: i32 = 2;
/// Vanilla `Raider.pickUpItem`: the bias against dropping the replaced head item.
const REPLACED_HEAD_DROP_BIAS: f32 = 0.1;

/// Vanilla `Raid.getOminousBannerInstance`: the white banner raid captains wear.
#[must_use]
pub fn ominous_banner_instance() -> ItemStack {
    let layer = |pattern, color| BannerPatternLayer::new(RegistryHolder::reference(pattern), color);
    let patterns = BannerPatternLayers::new(vec![
        layer(&vanilla_banner_patterns::RHOMBUS, DyeColor::Cyan),
        layer(&vanilla_banner_patterns::STRIPE_BOTTOM, DyeColor::LightGray),
        layer(&vanilla_banner_patterns::STRIPE_CENTER, DyeColor::Gray),
        layer(&vanilla_banner_patterns::BORDER, DyeColor::LightGray),
        layer(&vanilla_banner_patterns::STRIPE_MIDDLE, DyeColor::Black),
        layer(
            &vanilla_banner_patterns::HALF_HORIZONTAL,
            DyeColor::LightGray,
        ),
        layer(&vanilla_banner_patterns::CIRCLE, DyeColor::LightGray),
        layer(&vanilla_banner_patterns::BORDER, DyeColor::Black),
    ]);
    let mut banner = ItemStack::new(&vanilla_items::WHITE_BANNER);
    banner.set(BANNER_PATTERNS, patterns);
    banner.set(
        TOOLTIP_DISPLAY,
        TooltipDisplay::DEFAULT.with_hidden(BANNER_PATTERNS, true),
    );
    banner.set(
        ITEM_NAME,
        TextComponent::translated(translations::BLOCK_MINECRAFT_OMINOUS_BANNER.msg()),
    );
    banner.set(RARITY, Rarity::Uncommon);
    banner
}

/// Raid fields of a raider.
pub struct RaiderState {
    wave: AtomicI32,
    can_join_raid: AtomicBool,
    ticks_outside_raid: AtomicI32,
    raid: SyncMutex<Option<Arc<Raid>>>,
}

impl RaiderState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            wave: AtomicI32::new(0),
            can_join_raid: AtomicBool::new(false),
            ticks_outside_raid: AtomicI32::new(0),
            raid: SyncMutex::new(None),
        }
    }
}

impl Default for RaiderState {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `Raider`.
pub trait Raider: PatrollingMonster {
    fn raider_state(&self) -> &RaiderState;

    /// Vanilla `IS_CELEBRATING` synced flag.
    fn is_celebrating(&self) -> bool;

    fn set_celebrating(&self, celebrating: bool);

    /// Vanilla `getCelebrateSound`.
    fn celebrate_sound(&self) -> SoundEventRef;

    fn can_join_raid(&self) -> bool {
        self.raider_state().can_join_raid.load(Ordering::Relaxed)
    }

    fn set_can_join_raid(&self, can_join_raid: bool) {
        self.raider_state()
            .can_join_raid
            .store(can_join_raid, Ordering::Relaxed);
    }

    fn wave(&self) -> i32 {
        self.raider_state().wave.load(Ordering::Relaxed)
    }

    fn set_wave(&self, wave: i32) {
        self.raider_state().wave.store(wave, Ordering::Relaxed);
    }

    fn ticks_outside_raid(&self) -> i32 {
        self.raider_state()
            .ticks_outside_raid
            .load(Ordering::Relaxed)
    }

    fn set_ticks_outside_raid(&self, ticks: i32) {
        self.raider_state()
            .ticks_outside_raid
            .store(ticks, Ordering::Relaxed);
    }

    /// Vanilla `getCurrentRaid`.
    fn current_raid(&self) -> Option<Arc<Raid>> {
        self.raider_state().raid.lock().clone()
    }

    /// Vanilla `setCurrentRaid`.
    fn set_current_raid(&self, raid: Option<Arc<Raid>>) {
        *self.raider_state().raid.lock() = raid;
    }

    /// Vanilla `hasActiveRaid`.
    fn has_active_raid(&self) -> bool {
        self.current_raid().is_some_and(|raid| raid.is_active())
    }

    /// Vanilla `hasRaid`.
    fn has_raid(&self) -> bool {
        self.level().is_some_and(|world| {
            self.current_raid().is_some() || world.get_raid_at(self.block_position()).is_some()
        })
    }

    /// Vanilla `applyRaidBuffs`, with the raid's `getEnchantOdds` passed in.
    fn apply_raid_buffs(&self, _world: &Arc<World>, _wave: i32, _enchant_odds: f32) {}

    /// The raid part of Vanilla `Raider.aiStep`, run before the mob's own.
    fn raider_ai_step(&self, entity: &SharedEntity) {
        let Some(world) = self.level() else {
            return;
        };
        if !LivingEntity::is_alive(self) || !self.can_join_raid() {
            return;
        }
        match self.current_raid() {
            None => {
                if world.game_time() % 20 == 0
                    && let Some(nearby) = world.get_raid_at(self.block_position())
                    && entity.as_raider().is_some_and(Raids::can_join_raid)
                {
                    nearby.join_raid(&world, nearby.groups_spawned(), entity, None);
                }
            }
            Some(_) => {
                if let Some(target) = self.target()
                    && (target.entity_type() == &vanilla_entities::PLAYER
                        || target.entity_type() == &vanilla_entities::IRON_GOLEM)
                {
                    self.set_no_action_time(0);
                }
            }
        }
    }

    /// The raid part of Vanilla `Raider.die`, run before `super.die`.
    fn die_raider(&self, source: &DamageSource) {
        let Some(world) = self.level() else {
            return;
        };
        let Some(raid) = self.current_raid() else {
            return;
        };
        if self.is_patrol_leader() {
            raid.remove_leader(self.wave());
        }
        if let Some(killer) = source.causing_entity()
            && killer.entity_type() == &vanilla_entities::PLAYER
        {
            raid.add_hero_of_the_village(killer.uuid());
        }
        if let Some(raider) = self.as_raider() {
            raid.remove_from_raid(&world, raider, false);
        }
    }

    /// The raid part of Vanilla `Raider.hurtServer`, run before `super.hurtServer`.
    fn hurt_server_raider(&self, world: &World) {
        if let Some(raid) = self.current_raid()
            && raid.is_active()
        {
            raid.update_bossbar(world);
        }
    }

    /// Vanilla `Raider.requiresCustomPersistence`.
    fn requires_custom_persistence_raider(&self) -> bool {
        self.is_passenger() || self.is_leashed() || self.current_raid().is_some()
    }

    /// Vanilla `isCaptain`: a patrol leader wearing the ominous banner.
    fn is_captain(&self) -> bool {
        let has_captain_banner = {
            let equipment = self.living_base().equipment().lock();
            let banner = equipment.get_ref(EquipmentSlot::Head);
            !banner.is_empty() && ItemStack::matches(banner, &ominous_banner_instance())
        };
        has_captain_banner && self.is_patrol_leader()
    }

    /// Vanilla `Raider.pickUpItem`: in an active raid whose wave has no leader,
    /// the raider takes a dropped ominous banner and leads the wave. Returns
    /// `false` for any other item, where the caller runs its `super.pickUpItem`.
    fn pick_up_item_raider(&self, world: &Arc<World>, item_entity: &ItemEntity) -> bool {
        let item_stack = item_entity.get_item();
        let Some(raid) = self.current_raid().filter(|raid| raid.is_active()) else {
            return false;
        };
        if raid.leader(self.wave()).is_some()
            || !ItemStack::matches(&item_stack, &ominous_banner_instance())
        {
            return false;
        }
        let slot = EquipmentSlot::Head;
        let current = self.equipment_in_slot(slot);
        if !current.is_empty()
            && (rand::random::<f32>() - REPLACED_HEAD_DROP_BIAS).max(0.0)
                < self.equipment_drop_chance(slot)
        {
            let _ = self.spawn_at_location(current, 0.0);
        }
        // TODO(advancements): vanilla `onItemPickup` fires
        // THROWN_ITEM_PICKED_UP_BY_ENTITY here, like `Mob::default_pick_up_item`.
        let count = item_stack.count();
        self.set_item_slot(slot, item_stack);
        world.broadcast_to_nearby(
            ChunkPos::from_entity_pos(item_entity.position()),
            CTakeItemEntity::new(item_entity.id(), self.id(), count),
            None,
        );
        item_entity.set_removed(RemovalReason::Discarded);
        if let Some(raider) = self.as_raider() {
            raid.set_leader(self.wave(), raider);
        }
        self.set_patrol_leader(true);
        true
    }

    /// Vanilla `Raider.canJoinPatrol`.
    fn can_join_patrol_raider(&self) -> bool {
        !self.has_active_raid()
    }

    /// Vanilla `Raider.updateNoActionTime`.
    fn update_no_action_time_raider(&self) {
        self.set_no_action_time(self.no_action_time() + RAIDER_NO_ACTION_TIME_INCREMENT);
    }

    /// Vanilla `Raider.removeWhenFarAway`: raid members stay, others follow the patrol rule.
    fn remove_when_far_away_raider(&self, dist_sqr: f64) -> bool {
        self.current_raid().is_none() && self.remove_when_far_away_patrolling(dist_sqr)
    }

    /// The raider and patrol part of Vanilla `finalizeSpawn`, run before the mob's own.
    fn finalize_spawn_raider(&self, spawn_reason: EntitySpawnReason) {
        self.set_can_join_raid(
            self.entity_type() != &vanilla_entities::WITCH
                || spawn_reason != EntitySpawnReason::Natural,
        );
        self.finalize_spawn_patrolling(spawn_reason);
    }

    fn save_raider(&self, nbt: &mut NbtCompound) {
        self.save_patrolling(nbt);
        nbt.insert(TAG_WAVE, self.wave());
        nbt.insert(TAG_CAN_JOIN_RAID, i8::from(self.can_join_raid()));
        if let Some(raid) = self.current_raid()
            && let Some(world) = self.level()
            && let Some(id) = world.raids().get_id(&raid)
        {
            nbt.insert(TAG_RAID_ID, id);
        }
    }

    fn load_raider(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_patrolling(nbt);
        self.set_wave(nbt.int(TAG_WAVE).unwrap_or(0));
        self.set_can_join_raid(nbt.byte(TAG_CAN_JOIN_RAID).is_some_and(|b| b != 0));
        if let Some(raid_id) = nbt.int(TAG_RAID_ID)
            && let Some(world) = self.level()
        {
            let raid = world.raids().get(raid_id);
            self.set_current_raid(raid.clone());
            if let Some(raid) = raid
                && let Some(raider) = self.as_raider()
            {
                raid.add_wave_mob(&world, self.wave(), raider, false);
                if self.is_patrol_leader() {
                    raid.set_leader(self.wave(), raider);
                }
            }
        }
    }
}
