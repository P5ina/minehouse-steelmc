//! Vanilla raids: `net.minecraft.world.entity.raid.Raid` and the per-level
//! `Raids` saved data.
//!
//! A raid tracks its members by entity id and UUID and resolves them through
//! the world, so it holds no strong reference to an entity: a member that is
//! no longer in the world reads as removed, the same as a removed vanilla
//! raider object.

use crate::player::advancement::criteria;
use std::f32::consts::{PI, TAU};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glam::DVec3;
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use steel_math::trig::{cos, sin};
use steel_protocol::packets::game::{BossBarColor, BossBarOverlay, CSound, SoundSource};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_custom_stats::{RAID_TRIGGER, RAID_WIN};
use steel_registry::vanilla_entities::{EVOKER, PILLAGER, RAVAGER, VINDICATOR, WITCH};
use steel_registry::vanilla_game_rules::RAIDS;
use steel_registry::vanilla_mob_effects::{HERO_OF_THE_VILLAGE, RAID_OMEN};
use steel_registry::vanilla_poi_type_tags::PoiTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, sound_events, vanilla_blocks,
};
use steel_utils::locks::SyncMutex;
use steel_utils::saved_data::{SavedDataManager, names as saved_data_names};
use steel_utils::{BlockPos, Identifier, SectionPos, translations};
use text_components::{Modifier as _, TextComponent};
use uuid::Uuid;

use crate::boss_event::ServerBossEvent;
use crate::chunk::heightmap::HeightmapType;
use crate::entity::{
    Entity as _, EntitySpawnReason, LivingEntity, MobEffectInstance, Raider, SharedEntity,
    SpawnPlacements, create_entity_instance, ominous_banner_instance,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::player::Player;
use crate::poi::OccupationStatus;
use crate::world::World;
use steel_utils::types::Difficulty;

#[cfg(test)]
mod tests;

/// Vanilla `Raid.VALID_RAID_RADIUS_SQR`: how close a position must be to a raid center.
pub const VALID_RAID_RADIUS_SQR: i32 = 9216;
/// Vanilla `Raid.RAID_REMOVAL_THRESHOLD_SQR`: raiders farther than this leave the raid.
const RAID_REMOVAL_THRESHOLD_SQR: f64 = 12544.0;
/// Vanilla `Raid.MAX_NO_ACTION_TIME`.
pub const MAX_NO_ACTION_TIME: i32 = 2400;
/// Vanilla `Raid.MAX_CELEBRATION_TICKS`.
pub const MAX_CELEBRATION_TICKS: i32 = 600;
/// Vanilla `Raid.DEFAULT_MAX_RAID_OMEN_LEVEL`.
pub const DEFAULT_MAX_RAID_OMEN_LEVEL: i32 = 5;
const ALLOW_SPAWNING_WITHIN_VILLAGE_SECONDS_THRESHOLD: i32 = 7;
const SECTION_RADIUS_FOR_FINDING_NEW_VILLAGE_CENTER: i32 = 2;
const RAID_TIMEOUT_TICKS: i64 = 48_000;
const NUM_SPAWN_ATTEMPTS: i32 = 5;
const POST_RAID_TICK_LIMIT: i32 = 40;
const DEFAULT_PRE_RAID_TICKS: i32 = 300;
const OUTSIDE_RAID_BOUNDS_TIMEOUT: i32 = 30;
const LOW_MOB_THRESHOLD: usize = 2;
const HERO_OF_THE_VILLAGE_DURATION: i32 = 48_000;
/// Vanilla `Raids.createOrExtendRaid`: the village POI search radius.
const VILLAGE_POI_RADIUS: i32 = 64;
/// Vanilla `Raider.tickCount > 600` before outside-raid bookkeeping starts.
const RAIDER_SETTLE_TICKS: i32 = 600;
/// Vanilla `Raid.playSound`: the horn plays this far from the listener toward the raid.
const HORN_DISTANCE: f64 = 13.0;
const HORN_RANGE: f64 = 64.0;
const HORN_VOLUME: f32 = 64.0;

/// Vanilla `Raid.RaidStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RaidStatus {
    /// Waves are still coming.
    Ongoing,
    /// Every wave was beaten; raiders are gone and heroes are rewarded.
    Victory,
    /// The village was lost after the first wave spawned.
    Loss,
    /// The raid ended and is dropped on the next tick.
    Stopped,
}

/// Vanilla `Raid.RaiderType`: per-wave base spawn counts, index = wave.
const RAIDER_TYPES: [(RaiderType, [i32; 8]); 5] = [
    (RaiderType::Vindicator, [0, 0, 2, 0, 1, 4, 2, 5]),
    (RaiderType::Evoker, [0, 0, 0, 0, 0, 1, 1, 2]),
    (RaiderType::Pillager, [0, 4, 3, 3, 4, 4, 4, 2]),
    (RaiderType::Witch, [0, 0, 0, 0, 3, 0, 0, 1]),
    (RaiderType::Ravager, [0, 0, 0, 1, 0, 1, 0, 2]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaiderType {
    Vindicator,
    Evoker,
    Pillager,
    Witch,
    Ravager,
}

impl RaiderType {
    fn entity_type(self) -> EntityTypeRef {
        match self {
            Self::Vindicator => &VINDICATOR,
            Self::Evoker => &EVOKER,
            Self::Pillager => &PILLAGER,
            Self::Witch => &WITCH,
            Self::Ravager => &RAVAGER,
        }
    }
}

/// Vanilla `Raid.getNumGroups`.
#[must_use]
pub const fn num_groups(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 3,
        Difficulty::Normal => 5,
        Difficulty::Hard => 7,
    }
}

/// Vanilla `Vec3i.distSqr`.
fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let dx = f64::from(a.x() - b.x());
    let dy = f64::from(a.y() - b.y());
    let dz = f64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}

/// A raid member, resolved through the world on use.
#[derive(Debug, Clone, Copy)]
struct RaidMember {
    id: i32,
    uuid: Uuid,
}

impl RaidMember {
    fn of(raider: &dyn Raider) -> Self {
        Self {
            id: raider.id(),
            uuid: raider.uuid(),
        }
    }

    fn resolve(self, world: &World) -> Option<SharedEntity> {
        world
            .get_entity_by_id(self.id)
            .filter(|entity| entity.uuid() == self.uuid && entity.as_raider().is_some())
    }
}

struct RaidState {
    /// Wave number to its leader.
    leaders: Vec<(i32, RaidMember)>,
    /// Wave number to its members, in wave order.
    raiders: Vec<(i32, Vec<RaidMember>)>,
    heroes_of_the_village: Vec<Uuid>,
    ticks_active: i64,
    started: bool,
    total_health: f32,
    raid_omen_level: i32,
    groups_spawned: i32,
    post_raid_ticks: i32,
    raid_cooldown_ticks: i32,
    num_groups: i32,
    status: RaidStatus,
    celebration_ticks: i32,
    wave_spawn_pos: Option<BlockPos>,
}

impl RaidState {
    fn total_raiders_alive(&self) -> usize {
        self.raiders.iter().map(|(_, members)| members.len()).sum()
    }

    fn wave_mut(&mut self, wave: i32) -> &mut Vec<RaidMember> {
        let index = if let Some(index) = self.raiders.iter().position(|(w, _)| *w == wave) {
            index
        } else {
            self.raiders.push((wave, Vec::new()));
            self.raiders.len() - 1
        };
        &mut self.raiders[index].1
    }

    const fn has_first_wave_spawned(&self) -> bool {
        self.groups_spawned > 0
    }

    const fn is_final_wave(&self) -> bool {
        self.groups_spawned == self.num_groups
    }

    const fn has_bonus_wave(&self) -> bool {
        self.raid_omen_level > 1
    }

    const fn has_spawned_bonus_wave(&self) -> bool {
        self.groups_spawned > self.num_groups
    }

    const fn has_more_waves(&self) -> bool {
        if self.has_bonus_wave() {
            !self.has_spawned_bonus_wave()
        } else {
            !self.is_final_wave()
        }
    }

    fn should_spawn_bonus_group(&self) -> bool {
        self.is_final_wave() && self.total_raiders_alive() == 0 && self.has_bonus_wave()
    }

    fn should_spawn_group(&self) -> bool {
        self.raid_cooldown_ticks == 0
            && (self.groups_spawned < self.num_groups || self.should_spawn_bonus_group())
            && self.total_raiders_alive() == 0
    }

    const fn is_over(&self) -> bool {
        matches!(self.status, RaidStatus::Victory | RaidStatus::Loss)
    }

    /// Vanilla `getEnchantOdds`.
    const fn enchant_odds(&self) -> f32 {
        match self.raid_omen_level {
            2 => 0.1,
            3 => 0.25,
            4 => 0.5,
            5 => 0.75,
            _ => 0.0,
        }
    }
}

/// Vanilla `Raid`.
pub struct Raid {
    center: SyncMutex<BlockPos>,
    active: AtomicBool,
    boss_event: ServerBossEvent,
    state: SyncMutex<RaidState>,
}

fn raid_name() -> TextComponent {
    TextComponent::translated(translations::EVENT_MINECRAFT_RAID.msg())
}

impl Raid {
    /// Vanilla `new Raid(center, difficulty)`.
    #[must_use]
    pub fn new(center: BlockPos, difficulty: Difficulty) -> Self {
        let raid = Self::with_state(
            center,
            true,
            RaidState {
                leaders: Vec::new(),
                raiders: Vec::new(),
                heroes_of_the_village: Vec::new(),
                ticks_active: 0,
                started: false,
                total_health: 0.0,
                raid_omen_level: 0,
                groups_spawned: 0,
                post_raid_ticks: 0,
                raid_cooldown_ticks: DEFAULT_PRE_RAID_TICKS,
                num_groups: num_groups(difficulty),
                status: RaidStatus::Ongoing,
                celebration_ticks: 0,
                wave_spawn_pos: None,
            },
        );
        raid.boss_event.set_progress(0.0);
        raid
    }

    fn with_state(center: BlockPos, active: bool, state: RaidState) -> Self {
        Self {
            center: SyncMutex::new(center),
            active: AtomicBool::new(active),
            boss_event: ServerBossEvent::new(
                Uuid::new_v4(),
                raid_name(),
                BossBarColor::Red,
                BossBarOverlay::Notched10,
            ),
            state: SyncMutex::new(state),
        }
    }

    fn from_saved(saved: &SavedRaid) -> Self {
        Self::with_state(
            BlockPos::new(saved.center[0], saved.center[1], saved.center[2]),
            saved.active,
            RaidState {
                leaders: Vec::new(),
                raiders: Vec::new(),
                heroes_of_the_village: saved.heroes_of_the_village.clone(),
                ticks_active: saved.ticks_active,
                started: saved.started,
                total_health: saved.total_health,
                raid_omen_level: saved.raid_omen_level,
                groups_spawned: saved.groups_spawned,
                post_raid_ticks: saved.post_raid_ticks,
                raid_cooldown_ticks: saved.cooldown_ticks,
                num_groups: saved.group_count,
                status: saved.status,
                celebration_ticks: 0,
                wave_spawn_pos: None,
            },
        )
    }

    fn to_saved(&self, id: i32) -> SavedRaid {
        let state = self.state.lock();
        let center = self.center();
        SavedRaid {
            id,
            started: state.started,
            active: self.is_active(),
            ticks_active: state.ticks_active,
            raid_omen_level: state.raid_omen_level,
            groups_spawned: state.groups_spawned,
            cooldown_ticks: state.raid_cooldown_ticks,
            post_raid_ticks: state.post_raid_ticks,
            total_health: state.total_health,
            group_count: state.num_groups,
            status: state.status,
            center: [center.x(), center.y(), center.z()],
            heroes_of_the_village: state.heroes_of_the_village.clone(),
        }
    }

    /// The raid's boss bar.
    #[must_use]
    pub const fn boss_event(&self) -> &ServerBossEvent {
        &self.boss_event
    }

    /// Vanilla `getCenter`.
    #[must_use]
    pub fn center(&self) -> BlockPos {
        *self.center.lock()
    }

    /// Vanilla `isActive`.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Vanilla `getStatus` (debug only in vanilla).
    #[must_use]
    pub fn status(&self) -> RaidStatus {
        self.state.lock().status
    }

    /// Vanilla `isOver`.
    #[must_use]
    pub fn is_over(&self) -> bool {
        self.state.lock().is_over()
    }

    /// Vanilla `isStopped`.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.state.lock().status == RaidStatus::Stopped
    }

    /// Vanilla `isVictory`.
    #[must_use]
    pub fn is_victory(&self) -> bool {
        self.state.lock().status == RaidStatus::Victory
    }

    /// Vanilla `isLoss`.
    #[must_use]
    pub fn is_loss(&self) -> bool {
        self.state.lock().status == RaidStatus::Loss
    }

    /// Vanilla `isBetweenWaves`.
    #[must_use]
    pub fn is_between_waves(&self) -> bool {
        let state = self.state.lock();
        state.has_first_wave_spawned()
            && state.total_raiders_alive() == 0
            && state.raid_cooldown_ticks > 0
    }

    /// Vanilla `isStarted`.
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.state.lock().started
    }

    /// Vanilla `getGroupsSpawned`.
    #[must_use]
    pub fn groups_spawned(&self) -> i32 {
        self.state.lock().groups_spawned
    }

    /// Vanilla `getTotalHealth`.
    #[must_use]
    pub fn total_health(&self) -> f32 {
        self.state.lock().total_health
    }

    /// Vanilla `getTotalRaidersAlive`.
    #[must_use]
    pub fn total_raiders_alive(&self) -> usize {
        self.state.lock().total_raiders_alive()
    }

    /// Vanilla `getMaxRaidOmenLevel`.
    #[must_use]
    #[expect(clippy::unused_self, reason = "vanilla instance getter")]
    pub const fn max_raid_omen_level(&self) -> i32 {
        DEFAULT_MAX_RAID_OMEN_LEVEL
    }

    /// Vanilla `getRaidOmenLevel`.
    #[must_use]
    pub fn raid_omen_level(&self) -> i32 {
        self.state.lock().raid_omen_level
    }

    /// Vanilla `setRaidOmenLevel`.
    pub fn set_raid_omen_level(&self, level: i32) {
        self.state.lock().raid_omen_level = level;
    }

    /// Vanilla `getEnchantOdds`.
    #[must_use]
    pub fn enchant_odds(&self) -> f32 {
        self.state.lock().enchant_odds()
    }

    /// Vanilla `getNumGroups(level.getDifficulty())` as fixed when the raid started.
    #[must_use]
    pub fn num_groups(&self) -> i32 {
        self.state.lock().num_groups
    }

    /// Vanilla `addHeroOfTheVillage`.
    pub fn add_hero_of_the_village(&self, killer: Uuid) {
        let mut state = self.state.lock();
        if !state.heroes_of_the_village.contains(&killer) {
            state.heroes_of_the_village.push(killer);
        }
    }

    /// The UUIDs that become heroes when the raid is won.
    #[must_use]
    pub fn heroes_of_the_village(&self) -> Vec<Uuid> {
        self.state.lock().heroes_of_the_village.clone()
    }

    /// Vanilla `absorbRaidOmen`.
    pub fn absorb_raid_omen(&self, player: &Player) -> bool {
        let Some(effect) = player.living_base().mob_effect(RAID_OMEN) else {
            return false;
        };
        let first_wave_spawned = {
            let mut state = self.state.lock();
            state.raid_omen_level = (state.raid_omen_level + effect.amplifier() + 1)
                .clamp(0, DEFAULT_MAX_RAID_OMEN_LEVEL);
            state.has_first_wave_spawned()
        };
        if !first_wave_spawned {
            player.award_custom_stat(&RAID_TRIGGER);
            player.trigger_player_criteria(&criteria::RAID_OMEN);
        }
        true
    }

    /// Vanilla `stop`.
    pub fn stop(&self) {
        let mut state = self.state.lock();
        self.stop_locked(&mut state);
    }

    fn stop_locked(&self, state: &mut RaidState) {
        self.active.store(false, Ordering::Relaxed);
        self.boss_event.remove_all_players();
        state.status = RaidStatus::Stopped;
    }

    /// Vanilla `getLeader`: the member UUID leading `wave`.
    #[must_use]
    pub fn leader(&self, wave: i32) -> Option<Uuid> {
        self.state
            .lock()
            .leaders
            .iter()
            .find(|(w, _)| *w == wave)
            .map(|(_, member)| member.uuid)
    }

    /// Vanilla `setLeader`: records the leader and puts the ominous banner on its head.
    pub fn set_leader(&self, wave: i32, raider: &dyn Raider) {
        Self::set_leader_locked(&mut self.state.lock(), wave, raider);
    }

    fn set_leader_locked(state: &mut RaidState, wave: i32, raider: &dyn Raider) {
        let member = RaidMember::of(raider);
        match state.leaders.iter_mut().find(|(w, _)| *w == wave) {
            Some(entry) => entry.1 = member,
            None => state.leaders.push((wave, member)),
        }
        let _ = raider
            .living_base()
            .equipment()
            .lock()
            .set(EquipmentSlot::Head, ominous_banner_instance());
        raider.set_equipment_drop_chance(EquipmentSlot::Head, 2.0);
    }

    /// Vanilla `removeLeader`.
    pub fn remove_leader(&self, wave: i32) {
        self.state.lock().leaders.retain(|(w, _)| *w != wave);
    }

    /// Vanilla `getAllRaiders`: the members currently in the world.
    #[must_use]
    pub fn all_raiders(&self, world: &World) -> Vec<SharedEntity> {
        let members: Vec<RaidMember> = self
            .state
            .lock()
            .raiders
            .iter()
            .flat_map(|(_, members)| members.iter().copied())
            .collect();
        members
            .into_iter()
            .filter_map(|member| member.resolve(world))
            .collect()
    }

    /// Vanilla `getHealthOfLivingRaiders`.
    #[must_use]
    pub fn health_of_living_raiders(&self, world: &World) -> f32 {
        Self::health_of_living_raiders_locked(&self.state.lock(), world)
    }

    fn health_of_living_raiders_locked(state: &RaidState, world: &World) -> f32 {
        state
            .raiders
            .iter()
            .flat_map(|(_, members)| members.iter())
            .filter_map(|member| member.resolve(world))
            .filter_map(|entity| entity.as_living_entity().map(LivingEntity::get_health))
            .sum()
    }

    /// Vanilla `updateBossbar`.
    pub fn update_bossbar(&self, world: &World) {
        self.update_bossbar_locked(&self.state.lock(), world);
    }

    fn update_bossbar_locked(&self, state: &RaidState, world: &World) {
        let health = Self::health_of_living_raiders_locked(state, world);
        // Java float division: 0 / 0 is NaN and Mth.clamp(NaN) keeps NaN.
        let progress = health / state.total_health;
        self.boss_event.set_progress(java_clamp(progress, 0.0, 1.0));
    }

    /// Vanilla `addWaveMob(level, wave, raider, updateHealth)`.
    pub fn add_wave_mob(
        &self,
        world: &World,
        wave: i32,
        raider: &dyn Raider,
        update_health: bool,
    ) -> bool {
        let mut state = self.state.lock();
        self.add_wave_mob_locked(&mut state, world, wave, raider, update_health)
    }

    fn add_wave_mob_locked(
        &self,
        state: &mut RaidState,
        world: &World,
        wave: i32,
        raider: &dyn Raider,
        update_health: bool,
    ) -> bool {
        let member = RaidMember::of(raider);
        let members = state.wave_mut(wave);
        members.retain(|existing| existing.uuid != member.uuid);
        members.push(member);
        if update_health {
            state.total_health += raider.get_health();
        }
        self.update_bossbar_locked(state, world);
        true
    }

    /// Vanilla `removeFromRaid`.
    pub fn remove_from_raid(
        &self,
        world: &World,
        raider: &dyn Raider,
        remove_from_total_health: bool,
    ) {
        let mut state = self.state.lock();
        self.remove_from_raid_locked(&mut state, world, raider, remove_from_total_health);
    }

    fn remove_from_raid_locked(
        &self,
        state: &mut RaidState,
        world: &World,
        raider: &dyn Raider,
        remove_from_total_health: bool,
    ) {
        let uuid = raider.uuid();
        let wave = raider.wave();
        let Some((_, members)) = state.raiders.iter_mut().find(|(w, _)| *w == wave) else {
            return;
        };
        let before = members.len();
        members.retain(|member| member.uuid != uuid);
        if members.len() == before {
            return;
        }
        if remove_from_total_health {
            state.total_health -= raider.get_health();
        }
        raider.set_current_raid(None);
        self.update_bossbar_locked(state, world);
    }

    /// Vanilla `joinRaid`. A fresh raider (`spawn_at` set) was created at the
    /// spawn position and is finalized, buffed and added to the world here.
    pub fn join_raid(
        self: &Arc<Self>,
        world: &Arc<World>,
        wave: i32,
        raider_entity: &SharedEntity,
        spawn_at: Option<BlockPos>,
    ) {
        let mut state = self.state.lock();
        self.join_raid_locked(&mut state, world, wave, raider_entity, spawn_at);
    }

    fn join_raid_locked(
        self: &Arc<Self>,
        state: &mut RaidState,
        world: &Arc<World>,
        wave: i32,
        raider_entity: &SharedEntity,
        spawn_at: Option<BlockPos>,
    ) {
        let Some(raider) = raider_entity.as_raider() else {
            return;
        };
        if !self.add_wave_mob_locked(state, world, wave, raider, true) {
            return;
        }
        raider.set_current_raid(Some(Arc::clone(self)));
        raider.set_wave(wave);
        raider.set_can_join_raid(true);
        raider.set_ticks_outside_raid(0);
        if spawn_at.is_some() {
            raider.finalize_spawn(world, EntitySpawnReason::Event, None);
            raider.apply_raid_buffs(world, wave, state.enchant_odds());
            raider.set_on_ground(true);
            if let Err(error) =
                world.try_add_fresh_entity_with_passengers(Arc::clone(raider_entity))
            {
                log::debug!("Raider spawn failed: {error:?}");
            }
        }
    }

    /// Vanilla `tick`.
    #[expect(
        clippy::too_many_lines,
        reason = "keeps vanilla Raid.tick in one ordered body"
    )]
    pub fn tick(self: &Arc<Self>, world: &Arc<World>) {
        let mut state = self.state.lock();
        if state.status == RaidStatus::Stopped {
            return;
        }
        if state.status == RaidStatus::Ongoing {
            let old_active = self.is_active();
            let active = world.is_full_chunk_loaded_at(self.center());
            self.active.store(active, Ordering::Relaxed);
            if world.difficulty() == Difficulty::Peaceful {
                self.stop_locked(&mut state);
                return;
            }
            if old_active != active {
                self.boss_event.set_visible(active);
            }
            if !active {
                return;
            }
            if !world.is_village(self.center()) {
                self.move_raid_center_to_nearby_village_section(world);
            }
            if !world.is_village(self.center()) {
                if state.groups_spawned > 0 {
                    state.status = RaidStatus::Loss;
                } else {
                    self.stop_locked(&mut state);
                }
            }

            state.ticks_active += 1;
            if state.ticks_active >= RAID_TIMEOUT_TICKS {
                self.stop_locked(&mut state);
                return;
            }

            let raiders_alive = state.total_raiders_alive();
            if raiders_alive == 0 && state.has_more_waves() {
                if state.raid_cooldown_ticks <= 0 {
                    if state.raid_cooldown_ticks == 0 && state.groups_spawned > 0 {
                        state.raid_cooldown_ticks = DEFAULT_PRE_RAID_TICKS;
                        self.boss_event.set_name(raid_name());
                        return;
                    }
                } else {
                    let has_cached_wave_spawn_pos = state.wave_spawn_pos.is_some();
                    let mut should_try_to_find_spawn_pos =
                        !has_cached_wave_spawn_pos && state.raid_cooldown_ticks % 5 == 0;
                    if let Some(pos) = state.wave_spawn_pos
                        && !world.is_entity_ticking_chunk_loaded(pos)
                    {
                        should_try_to_find_spawn_pos = true;
                    }
                    if should_try_to_find_spawn_pos {
                        state.wave_spawn_pos = self.find_random_spawn_pos(&state, world, 8);
                    }
                    if state.raid_cooldown_ticks == DEFAULT_PRE_RAID_TICKS
                        || state.raid_cooldown_ticks % 20 == 0
                    {
                        self.update_players(world);
                    }
                    state.raid_cooldown_ticks -= 1;
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "vanilla float math on a small tick count"
                    )]
                    let progress = (DEFAULT_PRE_RAID_TICKS - state.raid_cooldown_ticks) as f32
                        / DEFAULT_PRE_RAID_TICKS as f32;
                    self.boss_event.set_progress(progress.clamp(0.0, 1.0));
                }
            }

            if state.ticks_active % 20 == 0 {
                self.update_players(world);
                self.update_raiders(&mut state, world);
                if raiders_alive > 0 && raiders_alive <= LOW_MOB_THRESHOLD {
                    self.boss_event.set_name(raid_name().add_children(vec![
                        TextComponent::plain(" - "),
                        TextComponent::translated(
                            translations::EVENT_MINECRAFT_RAID_RAIDERS_REMAINING
                                .message([TextComponent::from(raiders_alive.to_string())]),
                        ),
                    ]));
                } else {
                    self.boss_event.set_name(raid_name());
                }
            }

            let mut sound_played = false;
            let mut attempt = 0;
            while state.should_spawn_group() {
                let spawn_pos = state
                    .wave_spawn_pos
                    .or_else(|| self.find_random_spawn_pos(&state, world, 20));
                if let Some(spawn_pos) = spawn_pos {
                    state.started = true;
                    self.spawn_group(&mut state, world, spawn_pos);
                    if !sound_played {
                        self.play_sound(world, spawn_pos);
                        sound_played = true;
                    }
                } else {
                    attempt += 1;
                }
                if attempt > NUM_SPAWN_ATTEMPTS {
                    self.stop_locked(&mut state);
                    break;
                }
            }

            if state.started && !state.has_more_waves() && raiders_alive == 0 {
                if state.post_raid_ticks < POST_RAID_TICK_LIMIT {
                    state.post_raid_ticks += 1;
                } else {
                    state.status = RaidStatus::Victory;
                    let heroes = state.heroes_of_the_village.clone();
                    let amplifier = state.raid_omen_level - 1;
                    drop(state);
                    for hero in heroes {
                        let Some(entity) = world.get_entity_by_uuid(&hero) else {
                            continue;
                        };
                        if entity.is_spectator() {
                            continue;
                        }
                        let Some(living) = entity.as_living_entity() else {
                            continue;
                        };
                        living.add_mob_effect(
                            MobEffectInstance::with_duration(
                                HERO_OF_THE_VILLAGE,
                                HERO_OF_THE_VILLAGE_DURATION,
                                amplifier,
                            )
                            .with_ambient(false)
                            .with_visible(false)
                            .with_show_icon(true),
                        );
                        if let Some(player) = entity.as_player() {
                            player.award_custom_stat(&RAID_WIN);
                            player.trigger_player_criteria(&criteria::RAID_WIN);
                        }
                    }
                }
            }
        } else if state.is_over() {
            state.celebration_ticks += 1;
            if state.celebration_ticks >= MAX_CELEBRATION_TICKS {
                self.stop_locked(&mut state);
                return;
            }
            if state.celebration_ticks % 20 == 0 {
                self.update_players(world);
                self.boss_event.set_visible(true);
                if state.status == RaidStatus::Victory {
                    self.boss_event.set_progress(0.0);
                    self.boss_event.set_name(TextComponent::translated(
                        translations::EVENT_MINECRAFT_RAID_VICTORY_FULL.msg(),
                    ));
                } else {
                    self.boss_event.set_name(TextComponent::translated(
                        translations::EVENT_MINECRAFT_RAID_DEFEAT_FULL.msg(),
                    ));
                }
            }
        }
    }

    /// Vanilla `updatePlayers` with `validPlayer`.
    fn update_players(self: &Arc<Self>, world: &World) {
        let current = self.boss_event.players();
        let mut in_raid = Vec::new();
        world.players.iter_players(|_, player| {
            if LivingEntity::is_alive(player.as_ref())
                && world
                    .get_raid_at(player.block_position())
                    .is_some_and(|raid| Arc::ptr_eq(&raid, self))
            {
                in_raid.push(Arc::clone(player));
            }
            true
        });
        for player in &in_raid {
            if !current.iter().any(|p| Arc::ptr_eq(p, player)) {
                self.boss_event.add_player(player);
            }
        }
        for player in &current {
            if !in_raid.iter().any(|p| Arc::ptr_eq(p, player)) {
                self.boss_event.remove_player(player);
            }
        }
    }

    /// Vanilla `moveRaidCenterToNearbyVillageSection`.
    fn move_raid_center_to_nearby_village_section(&self, world: &World) {
        let center = self.center();
        let section = SectionPos::from_block_pos(center);
        let r = SECTION_RADIUS_FOR_FINDING_NEW_VILLAGE_CENTER;
        let mut best: Option<(f64, BlockPos)> = None;
        // `SectionPos.cube` walks x, then y, then z; `min` keeps the first of equal distances.
        for x in section.x() - r..=section.x() + r {
            for y in section.y() - r..=section.y() + r {
                for z in section.z() - r..=section.z() + r {
                    if world.sections_to_village(SectionPos::new(x, y, z)) > 1 {
                        continue;
                    }
                    let section_center = BlockPos::new((x << 4) + 8, (y << 4) + 8, (z << 4) + 8);
                    let distance = dist_sqr(section_center, center);
                    if best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, section_center));
                    }
                }
            }
        }
        if let Some((_, pos)) = best {
            *self.center.lock() = pos;
        }
    }

    /// Vanilla `updateRaiders`.
    fn update_raiders(&self, state: &mut RaidState, world: &Arc<World>) {
        let center = self.center();
        let mut to_remove: Vec<SharedEntity> = Vec::new();
        let mut gone: Vec<Uuid> = Vec::new();
        for (_, members) in &state.raiders {
            for member in members {
                let Some(entity) = member.resolve(world) else {
                    gone.push(member.uuid);
                    continue;
                };
                let Some(raider) = entity.as_raider() else {
                    continue;
                };
                let raider_pos = raider.block_position();
                if raider.is_removed()
                    || raider
                        .level()
                        .is_none_or(|level| !Arc::ptr_eq(&level, world))
                    || dist_sqr(center, raider_pos) >= RAID_REMOVAL_THRESHOLD_SQR
                {
                    to_remove.push(Arc::clone(&entity));
                } else if raider.tick_count() > RAIDER_SETTLE_TICKS {
                    if world.get_entity_by_uuid(&raider.uuid()).is_none() {
                        to_remove.push(Arc::clone(&entity));
                    }
                    if !world.is_village(raider_pos) && raider.no_action_time() > MAX_NO_ACTION_TIME
                    {
                        raider.set_ticks_outside_raid(raider.ticks_outside_raid() + 1);
                    }
                    if raider.ticks_outside_raid() >= OUTSIDE_RAID_BOUNDS_TIMEOUT {
                        to_remove.push(Arc::clone(&entity));
                    }
                }
            }
        }
        // A member no longer in this world is a removed vanilla raider object.
        if !gone.is_empty() {
            for (_, members) in &mut state.raiders {
                members.retain(|member| !gone.contains(&member.uuid));
            }
            self.update_bossbar_locked(state, world);
        }
        for entity in to_remove {
            let Some(raider) = entity.as_raider() else {
                continue;
            };
            self.remove_from_raid_locked(state, world, raider, true);
            if raider.is_patrol_leader() {
                let wave = raider.wave();
                state.leaders.retain(|(w, _)| *w != wave);
            }
        }
    }

    /// Vanilla `playSound`: the raid horn for every nearby or raiding player.
    fn play_sound(&self, world: &World, sound_origin: BlockPos) {
        let in_raid = self.boss_event.players();
        let seed: i64 = rand::random();
        let raid_x = f64::from(sound_origin.x()) + 0.5;
        let raid_z = f64::from(sound_origin.z()) + 0.5;
        world.players.iter_players(|_, player| {
            let pos = player.position();
            let dx = raid_x - pos.x;
            let dz = raid_z - pos.z;
            let distance = (dx * dx + dz * dz).sqrt();
            let x = pos.x + HORN_DISTANCE / distance * dx;
            let z = pos.z + HORN_DISTANCE / distance * dz;
            if distance <= HORN_RANGE || in_raid.iter().any(|p| Arc::ptr_eq(p, player)) {
                player.send_packet(CSound::new(
                    &sound_events::EVENT_RAID_HORN,
                    SoundSource::Neutral,
                    DVec3::new(x, pos.y, z),
                    HORN_VOLUME,
                    1.0,
                    seed,
                ));
            }
            true
        });
    }

    /// Vanilla `spawnGroup`.
    fn spawn_group(self: &Arc<Self>, state: &mut RaidState, world: &Arc<World>, pos: BlockPos) {
        let mut leader_set = false;
        let group_number = state.groups_spawned + 1;
        state.total_health = 0.0;
        let difficulty = world.current_difficulty_at(pos).difficulty();
        let is_bonus_group = state.should_spawn_bonus_group();
        let mut random = rand::rng();
        let spawn_position = DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 1.0,
            f64::from(pos.z()) + 0.5,
        );

        for (raider_type, spawns_per_wave) in RAIDER_TYPES {
            let wave_index = if is_bonus_group {
                state.num_groups
            } else {
                group_number
            };
            let base = usize::try_from(wave_index)
                .ok()
                .and_then(|index| spawns_per_wave.get(index).copied())
                .unwrap_or(0);
            let num_spawns = base
                + potential_bonus_spawns(
                    raider_type,
                    &mut random,
                    group_number,
                    difficulty,
                    is_bonus_group,
                );
            let mut ravagers_spawned = 0;
            for _ in 0..num_spawns {
                let Ok(entity) =
                    create_entity_instance(world, raider_type.entity_type(), spawn_position)
                else {
                    break;
                };
                let Some(raider) = entity.as_raider() else {
                    break;
                };
                if !leader_set && raider.can_be_leader() {
                    raider.set_patrol_leader(true);
                    Self::set_leader_locked(state, group_number, raider);
                    leader_set = true;
                }
                self.join_raid_locked(state, world, group_number, &entity, Some(pos));
                if raider_type == RaiderType::Ravager {
                    let rider_type = if group_number == num_groups(Difficulty::Normal) {
                        Some(&PILLAGER)
                    } else if group_number >= num_groups(Difficulty::Hard) {
                        Some(if ravagers_spawned == 0 {
                            &EVOKER
                        } else {
                            &VINDICATOR
                        })
                    } else {
                        None
                    };
                    ravagers_spawned += 1;
                    if let Some(rider_type) = rider_type
                        && let Ok(rider) = create_entity_instance(world, rider_type, spawn_position)
                    {
                        self.join_raid_locked(state, world, group_number, &rider, Some(pos));
                        rider.snap_to(
                            DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z())),
                            0.0,
                            0.0,
                        );
                        rider.start_riding(&entity);
                    }
                }
            }
        }

        state.wave_spawn_pos = None;
        state.groups_spawned += 1;
        self.update_bossbar_locked(state, world);
    }

    /// Vanilla `findRandomSpawnPos`.
    fn find_random_spawn_pos(
        &self,
        state: &RaidState,
        world: &World,
        max_tries: i32,
    ) -> Option<BlockPos> {
        let center = self.center();
        let seconds_remaining = state.raid_cooldown_ticks / 20;
        #[expect(
            clippy::cast_precision_loss,
            reason = "vanilla float math on a small count"
        )]
        let how_far = 0.22_f32 * seconds_remaining as f32 - 0.24;
        let mut random = rand::rng();
        let start_angle = random.random::<f32>() * TAU;
        #[expect(clippy::cast_possible_truncation, reason = "vanilla Mth.floor")]
        let floor = |value: f32| value.floor() as i32;
        for i in 0..max_tries {
            #[expect(clippy::cast_precision_loss, reason = "small loop index")]
            let angle = start_angle + PI * i as f32 / 8.0;
            let spawn_x = center.x()
                + floor(cos(angle) * 32.0 * how_far)
                + random.random_range(0..3) * floor(how_far);
            let spawn_z = center.z()
                + floor(sin(angle) * 32.0 * how_far)
                + random.random_range(0..3) * floor(how_far);
            let spawn_y = world
                .heightmap_pos(
                    HeightmapType::WorldSurface,
                    BlockPos::new(spawn_x, 0, spawn_z),
                )
                .y();
            if (spawn_y - center.y()).abs() > 96 {
                continue;
            }
            let spawn_pos = BlockPos::new(spawn_x, spawn_y, spawn_z);
            if world.is_village(spawn_pos)
                && seconds_remaining > ALLOW_SPAWNING_WITHIN_VILLAGE_SECONDS_THRESHOLD
            {
                continue;
            }
            let delta = 10;
            if super::custom_spawner::has_chunks_at(
                world,
                spawn_pos.x() - delta,
                spawn_pos.z() - delta,
                spawn_pos.x() + delta,
                spawn_pos.z() + delta,
            ) && world.is_entity_ticking_chunk_loaded(spawn_pos)
                && (SpawnPlacements::is_spawn_position_ok(&RAVAGER, world, spawn_pos)
                    || (world.get_block_state(spawn_pos.below()).get_block()
                        == &vanilla_blocks::SNOW
                        && world.get_block_state(spawn_pos).is_air()))
            {
                return Some(spawn_pos);
            }
        }
        None
    }
}

/// Java `Mth.clamp(float)`: NaN passes through unchanged.
fn java_clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Vanilla `getPotentialBonusSpawns`.
fn potential_bonus_spawns(
    raider_type: RaiderType,
    random: &mut impl rand::Rng,
    wave: i32,
    difficulty: Difficulty,
    is_bonus_wave: bool,
) -> i32 {
    let is_easy = difficulty == Difficulty::Easy;
    let is_normal = difficulty == Difficulty::Normal;
    let bonus_spawns = match raider_type {
        RaiderType::Vindicator | RaiderType::Pillager => {
            if is_easy {
                random.random_range(0..2)
            } else if is_normal {
                1
            } else {
                2
            }
        }
        RaiderType::Evoker => return 0,
        RaiderType::Witch => {
            if is_easy || wave <= 2 || wave == 4 {
                return 0;
            }
            1
        }
        RaiderType::Ravager => i32::from(!is_easy && is_bonus_wave),
    };
    if bonus_spawns > 0 {
        random.random_range(0..=bonus_spawns)
    } else {
        0
    }
}

/// One saved raid (vanilla `Raid.MAP_CODEC` plus its `Raids.RaidWithId` id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedRaid {
    id: i32,
    started: bool,
    active: bool,
    ticks_active: i64,
    raid_omen_level: i32,
    groups_spawned: i32,
    cooldown_ticks: i32,
    post_raid_ticks: i32,
    total_health: f32,
    group_count: i32,
    status: RaidStatus,
    center: [i32; 3],
    heroes_of_the_village: Vec<Uuid>,
}

/// Vanilla `Raids.CODEC`, persisted as `data/raids.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedRaids {
    #[serde(default)]
    raids: Vec<SavedRaid>,
    next_id: i32,
    tick: i32,
}

impl Default for SavedRaids {
    fn default() -> Self {
        Self {
            raids: Vec::new(),
            next_id: 1,
            tick: 0,
        }
    }
}

struct RaidsState {
    raids: Vec<(i32, Arc<Raid>)>,
    next_id: i32,
    tick: i32,
}

/// Vanilla `Raids`: the level's raids by id.
pub struct Raids {
    state: SyncMutex<RaidsState>,
}

impl Default for Raids {
    fn default() -> Self {
        Self::from_saved(&SavedRaids::default())
    }
}

impl Raids {
    /// Loads `data/raids.toml`, starting empty when it is absent or unreadable.
    pub(crate) async fn load(saved_data: &SavedDataManager, world: &Identifier) -> Self {
        let saved = saved_data
            .load_or_default::<SavedRaids>(saved_data_names::RAIDS)
            .await
            .unwrap_or_else(|error| {
                log::warn!(
                    "Could not load raid data for world {world}; starting with none: {error}"
                );
                SavedRaids::default()
            });
        Self::from_saved(&saved)
    }

    pub(crate) fn from_saved(saved: &SavedRaids) -> Self {
        Self {
            state: SyncMutex::new(RaidsState {
                raids: saved
                    .raids
                    .iter()
                    .map(|raid| (raid.id, Arc::new(Raid::from_saved(raid))))
                    .collect(),
                next_id: saved.next_id,
                tick: saved.tick,
            }),
        }
    }

    pub(crate) fn to_saved(&self) -> SavedRaids {
        let (raids, next_id, tick) = {
            let state = self.state.lock();
            (state.raids.clone(), state.next_id, state.tick)
        };
        SavedRaids {
            raids: raids.iter().map(|(id, raid)| raid.to_saved(*id)).collect(),
            next_id,
            tick,
        }
    }

    fn snapshot(&self) -> Vec<(i32, Arc<Raid>)> {
        self.state.lock().raids.clone()
    }

    /// Vanilla `get`.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<Arc<Raid>> {
        self.state
            .lock()
            .raids
            .iter()
            .find(|(raid_id, _)| *raid_id == id)
            .map(|(_, raid)| Arc::clone(raid))
    }

    /// Vanilla `getId`.
    #[must_use]
    pub fn get_id(&self, raid: &Arc<Raid>) -> Option<i32> {
        self.state
            .lock()
            .raids
            .iter()
            .find(|(_, other)| Arc::ptr_eq(other, raid))
            .map(|(id, _)| *id)
    }

    /// Every tracked raid with its id.
    #[must_use]
    pub fn all(&self) -> Vec<(i32, Arc<Raid>)> {
        self.snapshot()
    }

    /// Vanilla `tick`.
    pub fn tick(&self, world: &Arc<World>) {
        self.state.lock().tick += 1;
        let raids_enabled = world.get_game_rule(&RAIDS);
        let mut stopped = Vec::new();
        for (_, raid) in self.snapshot() {
            if !raids_enabled {
                raid.stop();
            }
            if raid.is_stopped() {
                stopped.push(raid);
            } else {
                raid.tick(world);
            }
        }
        if !stopped.is_empty() {
            self.state
                .lock()
                .raids
                .retain(|(_, raid)| !stopped.iter().any(|s| Arc::ptr_eq(s, raid)));
        }
    }

    /// Vanilla `canJoinRaid`.
    #[must_use]
    pub fn can_join_raid(raider: &dyn Raider) -> bool {
        LivingEntity::is_alive(raider)
            && raider.can_join_raid()
            && raider.no_action_time() <= MAX_NO_ACTION_TIME
    }

    /// Vanilla `createOrExtendRaid`.
    pub fn create_or_extend_raid(
        &self,
        world: &World,
        player: &Player,
        raid_position: BlockPos,
    ) -> Option<Arc<Raid>> {
        if player.is_spectator()
            || !world.get_game_rule(&RAIDS)
            || !world.dimension_type.can_start_raid
        {
            return None;
        }
        let village_pois = world.poi_storage.lock().get_in_horizontal_square(
            &|poi_type_id| {
                REGISTRY
                    .poi_types
                    .by_id(poi_type_id)
                    .is_some_and(|poi_type| {
                        REGISTRY.poi_types.is_in_tag(poi_type, &PoiTag::VILLAGE)
                    })
            },
            raid_position,
            VILLAGE_POI_RADIUS,
            OccupationStatus::Occupied,
        );
        let radius_sqr = f64::from(VILLAGE_POI_RADIUS * VILLAGE_POI_RADIUS);
        let mut count = 0_u32;
        let mut totals = DVec3::ZERO;
        for (pos, _) in village_pois {
            if dist_sqr(pos, raid_position) > radius_sqr {
                continue;
            }
            totals += DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()));
            count += 1;
        }
        let raid_center = if count > 0 {
            let average = totals * (1.0 / f64::from(count));
            BlockPos::containing(average.x, average.y, average.z)
        } else {
            raid_position
        };

        let raid = {
            let mut state = self.state.lock();
            let existing = nearest_active(&state.raids, raid_center, VALID_RAID_RADIUS_SQR);
            let raid =
                existing.unwrap_or_else(|| Arc::new(Raid::new(raid_center, world.difficulty())));
            if !raid.is_started() && !state.raids.iter().any(|(_, r)| Arc::ptr_eq(r, &raid)) {
                state.next_id += 1;
                let id = state.next_id;
                state.raids.push((id, Arc::clone(&raid)));
            }
            raid
        };
        if !raid.is_started() || raid.raid_omen_level() < raid.max_raid_omen_level() {
            raid.absorb_raid_omen(player);
        }
        Some(raid)
    }

    /// Vanilla `getNearbyRaid`.
    #[must_use]
    pub fn get_nearby_raid(&self, pos: BlockPos, max_dist_sqr: i32) -> Option<Arc<Raid>> {
        nearest_active(&self.snapshot(), pos, max_dist_sqr)
    }
}

fn nearest_active(
    raids: &[(i32, Arc<Raid>)],
    pos: BlockPos,
    max_dist_sqr: i32,
) -> Option<Arc<Raid>> {
    let mut closest = None;
    let mut closest_distance = f64::from(max_dist_sqr);
    for (_, raid) in raids {
        let distance = dist_sqr(raid.center(), pos);
        if raid.is_active() && distance < closest_distance {
            closest = Some(Arc::clone(raid));
            closest_distance = distance;
        }
    }
    closest
}
