//! Raid members (Vanilla `Raider`) and the ominous banner they carry.
//!
//! Raids themselves come later; until then a raider never has a current raid,
//! so the raid-only behavior (joining raids, banner pickup, celebrating, raid
//! goals) stays inactive exactly as it is for a raider outside a raid.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
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
use steel_utils::translations;
use text_components::TextComponent;

use crate::entity::{EntitySpawnReason, PatrollingMonster};
use crate::inventory::prelude::EquipmentSlot;

const TAG_WAVE: &str = "Wave";
const TAG_CAN_JOIN_RAID: &str = "CanJoinRaid";
/// Vanilla `Raider.updateNoActionTime`: raiders idle twice as fast as other mobs.
const RAIDER_NO_ACTION_TIME_INCREMENT: i32 = 2;

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
}

impl RaiderState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            wave: AtomicI32::new(0),
            can_join_raid: AtomicBool::new(false),
            ticks_outside_raid: AtomicI32::new(0),
        }
    }
}

impl Default for RaiderState {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `Raider`.
// TODO: Raids: current raid, `getRaidAt` joining in `aiStep`, leader/hero bookkeeping in
// `die`, banner pickup, boss bar on hurt, and the `ObtainRaidLeaderBannerGoal`,
// `PathfindToRaidGoal`, `RaiderMoveThroughVillageGoal` and `RaiderCelebration` goals.
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

    /// Vanilla `hasActiveRaid`.
    fn has_active_raid(&self) -> bool {
        // TODO: `getCurrentRaid() != null && getCurrentRaid().isActive()` once raids exist.
        false
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

    /// Vanilla `Raider.canJoinPatrol`.
    fn can_join_patrol_raider(&self) -> bool {
        !self.has_active_raid()
    }

    /// Vanilla `Raider.updateNoActionTime`.
    fn update_no_action_time_raider(&self) {
        self.set_no_action_time(self.no_action_time() + RAIDER_NO_ACTION_TIME_INCREMENT);
    }

    /// Vanilla `Raider.removeWhenFarAway`: no current raid, so the patrol rule applies.
    fn remove_when_far_away_raider(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_patrolling(dist_sqr)
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
    }

    fn load_raider(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_patrolling(nbt);
        self.set_wave(nbt.int(TAG_WAVE).unwrap_or(0));
        self.set_can_join_raid(nbt.byte(TAG_CAN_JOIN_RAID).is_some_and(|b| b != 0));
    }
}
