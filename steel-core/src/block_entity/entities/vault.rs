//! Vanilla `VaultBlockEntity`, `VaultConfig`, `VaultServerData`,
//! `VaultSharedData` and `VaultState`: the trial chamber vault that takes a
//! key and ejects its reward once per player.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, Direction, EnumProperty, VaultState,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::LootContext;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::{
    REGISTRY, RegistryExt as _, level_events, sound_events, vanilla_block_entity_types,
    vanilla_items, vanilla_loot_tables,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{
    BlockPos, BlockStateId, DowncastType, DowncastTypeKey, Identifier, UuidExt as _,
    locks::SyncMutex,
};
use uuid::Uuid;

use crate::behavior::dispense::spawn_item;
use crate::block_entity::player_detector::{EntitySelector, PlayerDetector};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::{Entity as _, LivingEntity as _, entity_loot_ref, living_loot_equipment};
use crate::player::Player;
use crate::world::World;

const CONFIG_TAG: &str = "config";
const SHARED_DATA_TAG: &str = "shared_data";
const SERVER_DATA_TAG: &str = "server_data";

/// Vanilla `VaultBlockEntity.Server.UNLOCKING_DELAY_TICKS`.
const UNLOCKING_DELAY_TICKS: i64 = 14;
/// Vanilla `VaultBlockEntity.Server.DISPLAY_CYCLE_TICK_RATE`.
const DISPLAY_CYCLE_TICK_RATE: i64 = 20;
/// Vanilla `VaultBlockEntity.Server.INSERT_FAIL_SOUND_BUFFER_TICKS`.
const INSERT_FAIL_SOUND_BUFFER_TICKS: i64 = 15;
/// Vanilla `VaultState.UPDATE_CONNECTED_PLAYERS_TICK_RATE`.
const UPDATE_CONNECTED_PLAYERS_TICK_RATE: i64 = 20;
/// Vanilla `VaultState.DELAY_BETWEEN_EJECTIONS_TICKS` and
/// `DELAY_AFTER_LAST_EJECTION_TICKS`, both 20.
const DELAY_BETWEEN_EJECTIONS_TICKS: i64 = 20;
/// Vanilla `VaultState.DELAY_BEFORE_FIRST_EJECTION_TICKS`.
const DELAY_BEFORE_FIRST_EJECTION_TICKS: i64 = 20;
/// Vanilla `VaultServerData.MAX_REWARD_PLAYERS`.
const MAX_REWARD_PLAYERS: usize = 128;

const STATE: &EnumProperty<VaultState> = &BlockStateProperties::VAULT_STATE;

/// Vanilla `VaultConfig`.
#[derive(Clone, Debug, PartialEq)]
pub struct VaultConfig {
    /// Loot table rolled for the reward.
    pub loot_table: Identifier,
    /// Range in which players turn an inactive vault on.
    pub activation_range: f64,
    /// Range players must leave before an active vault turns off.
    pub deactivation_range: f64,
    /// Item (and count) the vault takes as its key.
    pub key_item: ItemStack,
    /// Loot table cycled through the display instead of `loot_table`.
    pub override_loot_table_to_display: Option<Identifier>,
    /// Which players the vault notices.
    pub player_detector: PlayerDetector,
}

impl Default for VaultConfig {
    /// Vanilla `VaultConfig.DEFAULT`.
    fn default() -> Self {
        Self {
            loot_table: vanilla_loot_tables::CHESTS_TRIAL_CHAMBERS_REWARD
                .key
                .clone(),
            activation_range: 4.0,
            deactivation_range: 4.5,
            key_item: ItemStack::new(&vanilla_items::TRIAL_KEY),
            override_loot_table_to_display: None,
            player_detector: PlayerDetector::IncludingCreativePlayers,
        }
    }
}

impl VaultConfig {
    /// Vanilla `VaultConfig.CODEC` encode: fields equal to the default are
    /// left out, an empty key item too.
    fn save(&self) -> NbtCompound {
        let default = Self::default();
        let mut nbt = NbtCompound::new();
        if self.loot_table != default.loot_table {
            nbt.insert("loot_table", self.loot_table.to_string());
        }
        if self.activation_range.to_bits() != default.activation_range.to_bits() {
            nbt.insert("activation_range", self.activation_range);
        }
        if self.deactivation_range.to_bits() != default.deactivation_range.to_bits() {
            nbt.insert("deactivation_range", self.deactivation_range);
        }
        if !self.key_item.is_empty() {
            nbt.insert("key_item", self.key_item.to_nbt_tag_ref());
        }
        if let Some(display) = &self.override_loot_table_to_display {
            nbt.insert("override_loot_table_to_display", display.to_string());
        }
        nbt
    }

    /// Vanilla `VaultConfig.CODEC` decode: each field falls back on its own,
    /// but a missing key item means no key; an activation range above the
    /// deactivation range rejects the whole config.
    fn load(nbt: &NbtCompoundView<'_, '_>) -> Option<Self> {
        let default = Self::default();
        let config = Self {
            loot_table: nbt
                .string("loot_table")
                .and_then(|key| key.to_str().parse::<Identifier>().ok())
                .unwrap_or(default.loot_table),
            activation_range: nbt
                .double("activation_range")
                .unwrap_or(default.activation_range),
            deactivation_range: nbt
                .double("deactivation_range")
                .unwrap_or(default.deactivation_range),
            key_item: nbt
                .compound("key_item")
                .and_then(|item| ItemStack::from_borrowed_compound(&item))
                .unwrap_or_else(ItemStack::empty),
            override_loot_table_to_display: nbt
                .string("override_loot_table_to_display")
                .and_then(|key| key.to_str().parse::<Identifier>().ok()),
            player_detector: default.player_detector,
        };
        (config.activation_range <= config.deactivation_range).then_some(config)
    }

    /// Vanilla `canEjectReward`.
    fn can_eject_reward(&self, state: VaultState) -> bool {
        !self.key_item.is_empty() && state != VaultState::Inactive
    }

    /// Vanilla `isValidToInsert`.
    fn is_valid_to_insert(&self, stack: &ItemStack) -> bool {
        ItemStack::is_same_item_same_components(stack, &self.key_item)
            && stack.count() >= self.key_item.count()
    }
}

/// Vanilla `VaultServerData`.
#[derive(Debug, Default)]
struct VaultServerData {
    rewarded_players: Vec<Uuid>,
    state_updating_resumes_at: i64,
    items_to_eject: Vec<ItemStack>,
    last_insert_fail_timestamp: i64,
    total_ejections_needed: i32,
    is_dirty: bool,
}

impl VaultServerData {
    fn has_rewarded_player(&self, uuid: Uuid) -> bool {
        self.rewarded_players.contains(&uuid)
    }

    /// Vanilla `addToRewardedPlayers`: the oldest entry goes past 128.
    fn add_to_rewarded_players(&mut self, uuid: Uuid) {
        if !self.rewarded_players.contains(&uuid) {
            self.rewarded_players.push(uuid);
        }
        if self.rewarded_players.len() > MAX_REWARD_PLAYERS {
            self.rewarded_players.remove(0);
        }
        self.is_dirty = true;
    }

    const fn pause_state_updating_until(&mut self, tick: i64) {
        self.state_updating_resumes_at = tick;
        self.is_dirty = true;
    }

    const fn mark_ejection_finished(&mut self) {
        self.total_ejections_needed = 0;
        self.is_dirty = true;
    }

    fn set_items_to_eject(&mut self, items: Vec<ItemStack>) {
        self.items_to_eject = items;
        self.total_ejections_needed = i32::try_from(self.items_to_eject.len()).unwrap_or(i32::MAX);
        self.is_dirty = true;
    }

    fn next_item_to_eject(&self) -> ItemStack {
        self.items_to_eject
            .last()
            .cloned()
            .unwrap_or_else(ItemStack::empty)
    }

    fn pop_next_item_to_eject(&mut self) -> ItemStack {
        match self.items_to_eject.pop() {
            Some(item) => {
                self.is_dirty = true;
                item
            }
            None => ItemStack::empty(),
        }
    }

    /// Vanilla `ejectionProgress`.
    #[expect(
        clippy::cast_precision_loss,
        reason = "vanilla converts the item counts to float"
    )]
    fn ejection_progress(&self) -> f32 {
        if self.total_ejections_needed == 1 {
            return 1.0;
        }
        let remaining = self.items_to_eject.len() as f32;
        let total = self.total_ejections_needed as f32;
        // Vanilla `Mth.inverseLerp(remaining, 1, total)`.
        1.0 - (remaining - 1.0) / (total - 1.0)
    }

    /// Vanilla `CODEC` encode.
    fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if !self.rewarded_players.is_empty() {
            nbt.insert("rewarded_players", uuid_list(&self.rewarded_players));
        }
        if self.state_updating_resumes_at != 0 {
            nbt.insert("state_updating_resumes_at", self.state_updating_resumes_at);
        }
        if !self.items_to_eject.is_empty() {
            nbt.insert("items_to_eject", item_list(&self.items_to_eject));
        }
        if self.total_ejections_needed != 0 {
            nbt.insert("total_ejections_needed", self.total_ejections_needed);
        }
        nbt
    }

    /// Vanilla `CODEC` decode followed by `set`, which keeps the current
    /// ejection total.
    fn load_into(&mut self, nbt: &NbtCompoundView<'_, '_>) {
        self.state_updating_resumes_at = nbt.long("state_updating_resumes_at").unwrap_or(0);
        self.items_to_eject = read_items(nbt, "items_to_eject");
        self.rewarded_players = read_uuids(nbt, "rewarded_players");
    }
}

/// Vanilla `VaultSharedData`: what the client sees.
#[derive(Debug)]
struct VaultSharedData {
    display_item: ItemStack,
    connected_players: Vec<Uuid>,
    connected_particles_range: f64,
    is_dirty: bool,
}

impl Default for VaultSharedData {
    fn default() -> Self {
        Self {
            display_item: ItemStack::empty(),
            connected_players: Vec::new(),
            connected_particles_range: VaultConfig::default().deactivation_range,
            is_dirty: false,
        }
    }
}

impl VaultSharedData {
    fn has_display_item(&self) -> bool {
        !self.display_item.is_empty()
    }

    fn set_display_item(&mut self, stack: &ItemStack) {
        if !ItemStack::matches(&self.display_item, stack) {
            self.display_item = stack.clone();
            self.is_dirty = true;
        }
    }

    /// Vanilla `updateConnectedPlayersWithinRange`: nearby players not yet
    /// rewarded.
    fn update_connected_players_within_range(
        &mut self,
        world: &World,
        pos: BlockPos,
        server: &VaultServerData,
        config: &VaultConfig,
        limit: f64,
    ) {
        let mut current: Vec<Uuid> = config
            .player_detector
            .detect(world, EntitySelector::SelectFromLevel, pos, limit, false)
            .into_iter()
            .filter(|uuid| !server.has_rewarded_player(*uuid))
            .collect();
        current.dedup();
        let same = current.len() == self.connected_players.len()
            && current
                .iter()
                .all(|uuid| self.connected_players.contains(uuid));
        if !same {
            self.connected_players = current;
            self.is_dirty = true;
        }
    }

    fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if !self.display_item.is_empty() {
            nbt.insert("display_item", self.display_item.to_nbt_tag_ref());
        }
        if !self.connected_players.is_empty() {
            nbt.insert("connected_players", uuid_list(&self.connected_players));
        }
        if self.connected_particles_range.to_bits()
            != VaultConfig::default().deactivation_range.to_bits()
        {
            nbt.insert("connected_particles_range", self.connected_particles_range);
        }
        nbt
    }

    /// Vanilla `CODEC` decode followed by `set`.
    fn load_into(&mut self, nbt: &NbtCompoundView<'_, '_>) {
        self.display_item = nbt
            .compound("display_item")
            .and_then(|item| ItemStack::from_borrowed_compound(&item))
            .unwrap_or_else(ItemStack::empty);
        self.connected_players = read_uuids(nbt, "connected_players");
        self.connected_particles_range = nbt
            .double("connected_particles_range")
            .unwrap_or_else(|| VaultConfig::default().deactivation_range);
    }
}

#[derive(Debug, Default)]
struct VaultData {
    config: VaultConfig,
    server: VaultServerData,
    shared: VaultSharedData,
}

/// Vault block entity.
pub struct VaultBlockEntity {
    base: BlockEntityBase,
    data: SyncMutex<VaultData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `VaultBlockEntity`.
unsafe impl DowncastType for VaultBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/vault");
}

impl VaultBlockEntity {
    /// Creates a vault with the default config.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: BlockEntityBase::new(&vanilla_block_entity_types::VAULT, level, pos, state),
            data: SyncMutex::new(VaultData::default()),
        }
    }

    /// The vault's config.
    #[must_use]
    pub fn config(&self) -> VaultConfig {
        self.data.lock().config.clone()
    }

    /// Vanilla `setConfig`.
    pub fn set_config(&self, config: VaultConfig) {
        self.data.lock().config = config;
    }

    /// The item shown spinning inside the vault.
    #[must_use]
    pub fn display_item(&self) -> ItemStack {
        self.data.lock().shared.display_item.clone()
    }

    /// Players the client draws connection particles to.
    #[must_use]
    pub fn connected_players(&self) -> Vec<Uuid> {
        self.data.lock().shared.connected_players.clone()
    }

    /// Whether `uuid` already took this vault's reward.
    #[must_use]
    pub fn has_rewarded_player(&self, uuid: Uuid) -> bool {
        self.data.lock().server.has_rewarded_player(uuid)
    }

    /// Items still waiting to be ejected, the next one last.
    #[must_use]
    pub fn items_to_eject(&self) -> Vec<ItemStack> {
        self.data.lock().server.items_to_eject.clone()
    }

    /// Vanilla `VaultBlockEntity.Server.tryInsertKey`. Returns whether the
    /// key was taken.
    pub fn try_insert_key(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        player: &Player,
        stack: &mut ItemStack,
    ) -> bool {
        let vault_state = state.get_value(STATE);
        let game_time = world.game_time();
        let unlocked = {
            let mut data = self.data.lock();
            if !data.config.can_eject_reward(vault_state) {
                return false;
            }
            if !data.config.is_valid_to_insert(stack) {
                play_insert_fail_sound(
                    world,
                    &mut data.server,
                    pos,
                    &sound_events::BLOCK_VAULT_INSERT_ITEM_FAIL,
                );
                return false;
            }
            if data.server.has_rewarded_player(player.uuid()) {
                play_insert_fail_sound(
                    world,
                    &mut data.server,
                    pos,
                    &sound_events::BLOCK_VAULT_REJECT_REWARDED_PLAYER,
                );
                return false;
            }
            let items = resolve_items_to_eject(&data.config, pos, player, stack);
            if items.is_empty() {
                return false;
            }
            player.award_stat(&vanilla_stat_types::ITEM_USED, stack.item());
            stack.consume(
                data.config.key_item.count(),
                player.has_infinite_materials(),
            );
            // Vanilla `unlock`.
            data.server.set_items_to_eject(items);
            let next = data.server.next_item_to_eject();
            data.shared.set_display_item(&next);
            data.server
                .pause_state_updating_until(game_time + UNLOCKING_DELAY_TICKS);
            true
        };
        if !unlocked {
            return false;
        }
        self.set_vault_state(
            world,
            pos,
            state,
            state.set_value(STATE, VaultState::Unlocking),
        );
        let mut data = self.data.lock();
        data.server.add_to_rewarded_players(player.uuid());
        let VaultData {
            config,
            server,
            shared,
        } = &mut *data;
        let range = config.deactivation_range;
        shared.update_connected_players_within_range(world, pos, server, config, range);
        true
    }

    /// Vanilla `setVaultState`: sets the block, then runs the exit and enter
    /// effects of the two states.
    fn set_vault_state(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        current: BlockStateId,
        new: BlockStateId,
    ) {
        world.set_block(pos, new, UpdateFlags::UPDATE_ALL);
        let from = current.get_value(STATE);
        let to = new.get_value(STATE);
        let ominous = new.get_value(&BlockStateProperties::OMINOUS);
        let mut data = self.data.lock();
        let VaultData { config, shared, .. } = &mut *data;
        on_exit(world, pos, from);
        on_enter(world, pos, to, config, shared, ominous);
    }

    /// Vanilla `tickAndGetNext`.
    fn tick_and_get_next(
        world: &Arc<World>,
        pos: BlockPos,
        current: VaultState,
        data: &mut VaultData,
    ) -> VaultState {
        let game_time = world.game_time();
        let VaultData {
            config,
            server,
            shared,
        } = data;
        match current {
            VaultState::Inactive => update_state_for_connected_players(
                world,
                pos,
                config,
                server,
                shared,
                config.activation_range,
            ),
            VaultState::Active => update_state_for_connected_players(
                world,
                pos,
                config,
                server,
                shared,
                config.deactivation_range,
            ),
            VaultState::Unlocking => {
                server.pause_state_updating_until(game_time + DELAY_BEFORE_FIRST_EJECTION_TICKS);
                VaultState::Ejecting
            }
            VaultState::Ejecting => {
                if server.items_to_eject.is_empty() {
                    server.mark_ejection_finished();
                    update_state_for_connected_players(
                        world,
                        pos,
                        config,
                        server,
                        shared,
                        config.deactivation_range,
                    )
                } else {
                    let progress = server.ejection_progress();
                    let item = server.pop_next_item_to_eject();
                    eject_result_item(world, pos, item, progress);
                    let next = server.next_item_to_eject();
                    shared.set_display_item(&next);
                    server.pause_state_updating_until(game_time + DELAY_BETWEEN_EJECTIONS_TICKS);
                    VaultState::Ejecting
                }
            }
        }
    }
}

/// Vanilla `VaultState.updateStateForConnectedPlayers`.
fn update_state_for_connected_players(
    world: &World,
    pos: BlockPos,
    config: &VaultConfig,
    server: &mut VaultServerData,
    shared: &mut VaultSharedData,
    range: f64,
) -> VaultState {
    shared.update_connected_players_within_range(world, pos, server, config, range);
    server.pause_state_updating_until(world.game_time() + UPDATE_CONNECTED_PLAYERS_TICK_RATE);
    if shared.connected_players.is_empty() {
        VaultState::Inactive
    } else {
        VaultState::Active
    }
}

/// Vanilla `VaultState.onEnter`.
fn on_enter(
    world: &World,
    pos: BlockPos,
    to: VaultState,
    config: &VaultConfig,
    shared: &mut VaultSharedData,
    ominous: bool,
) {
    match to {
        VaultState::Inactive => {
            shared.set_display_item(&ItemStack::empty());
            world.level_event(
                level_events::ANIMATION_VAULT_DEACTIVATE,
                pos,
                i32::from(ominous),
                None,
            );
        }
        VaultState::Active => {
            if !shared.has_display_item() {
                cycle_display_item_from_loot_table(pos, to, config, shared);
            }
            world.level_event(
                level_events::ANIMATION_VAULT_ACTIVATE,
                pos,
                i32::from(ominous),
                None,
            );
        }
        VaultState::Unlocking => play_sound(world, pos, &sound_events::BLOCK_VAULT_INSERT_ITEM),
        VaultState::Ejecting => play_sound(world, pos, &sound_events::BLOCK_VAULT_OPEN_SHUTTER),
    }
}

/// Vanilla `VaultState.onExit`.
fn on_exit(world: &World, pos: BlockPos, from: VaultState) {
    if from == VaultState::Ejecting {
        play_sound(world, pos, &sound_events::BLOCK_VAULT_CLOSE_SHUTTER);
    }
}

fn play_sound(world: &World, pos: BlockPos, sound: SoundEventRef) {
    world.play_sound(sound, SoundSource::Blocks, pos, 1.0, 1.0, None);
}

/// Vanilla `playInsertFailSound`: at most once every 15 ticks.
fn play_insert_fail_sound(
    world: &World,
    server: &mut VaultServerData,
    pos: BlockPos,
    sound: SoundEventRef,
) {
    let game_time = world.game_time();
    if game_time >= server.last_insert_fail_timestamp + INSERT_FAIL_SOUND_BUFFER_TICKS {
        play_sound(world, pos, sound);
        server.last_insert_fail_timestamp = game_time;
    }
}

/// Vanilla `VaultState.ejectResultItem`.
fn eject_result_item(world: &Arc<World>, pos: BlockPos, item: ItemStack, progress: f32) {
    let position = DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()) + 1.2,
        f64::from(pos.z()) + 0.5,
    );
    spawn_item(world, item, 2.0, Direction::Up, position);
    world.level_event(level_events::ANIMATION_VAULT_EJECT_ITEM, pos, 0, None);
    world.play_sound(
        &sound_events::BLOCK_VAULT_EJECT_ITEM,
        SoundSource::Blocks,
        pos,
        1.0,
        0.8 + 0.4 * progress,
        None,
    );
}

/// Vanilla `cycleDisplayItemFromLootTable`.
fn cycle_display_item_from_loot_table(
    pos: BlockPos,
    state: VaultState,
    config: &VaultConfig,
    shared: &mut VaultSharedData,
) {
    if !config.can_eject_reward(state) {
        shared.set_display_item(&ItemStack::empty());
        return;
    }
    let key = config
        .override_loot_table_to_display
        .as_ref()
        .unwrap_or(&config.loot_table);
    let display = random_display_item(pos, key);
    shared.set_display_item(&display);
}

/// Vanilla `getRandomDisplayItemFromLootTable`.
fn random_display_item(pos: BlockPos, key: &Identifier) -> ItemStack {
    let Some(table) = REGISTRY.loot_tables.by_key(key) else {
        return ItemStack::empty();
    };
    let mut rng = rand::rng();
    let (x, y, z) = pos.get_center();
    let mut items = {
        let mut ctx = LootContext::new(&mut rng).with_origin(x, y, z);
        table.get_random_items(&mut ctx)
    };
    if items.is_empty() {
        return ItemStack::empty();
    }
    let index = rand::random_range(0..items.len());
    items.swap_remove(index)
}

/// Vanilla `resolveItemsToEject`.
fn resolve_items_to_eject(
    config: &VaultConfig,
    pos: BlockPos,
    player: &Player,
    key: &ItemStack,
) -> Vec<ItemStack> {
    let Some(table) = REGISTRY.loot_tables.by_key(&config.loot_table) else {
        return Vec::new();
    };
    let mut rng = rand::rng();
    let (x, y, z) = pos.get_center();
    let equipment = living_loot_equipment(player);
    let mut ctx = LootContext::new(&mut rng)
        .with_origin(x, y, z)
        .with_luck(player.get_luck())
        .with_this_entity(entity_loot_ref(player, Some(&equipment)))
        .with_tool(key);
    table.get_random_items(&mut ctx)
}

fn uuid_list(uuids: &[Uuid]) -> NbtList {
    NbtList::from(
        uuids
            .iter()
            .map(|uuid| NbtTag::IntArray(uuid.to_int_array().to_vec()))
            .collect::<Vec<_>>(),
    )
}

fn item_list(items: &[ItemStack]) -> NbtList {
    NbtList::from(
        items
            .iter()
            .map(ItemStack::to_nbt_tag_ref)
            .collect::<Vec<_>>(),
    )
}

fn read_uuids(nbt: &NbtCompoundView<'_, '_>, key: &str) -> Vec<Uuid> {
    let arrays: Vec<Vec<i32>> = nbt
        .list(key)
        .and_then(|list| {
            list.int_arrays().map(|arrays| {
                arrays
                    .iter()
                    .map(simdnbt::raw_list::RawList::to_vec)
                    .collect()
            })
        })
        .unwrap_or_default();
    let mut uuids = Vec::new();
    for array in arrays {
        if let Some(uuid) = Uuid::from_int_array(&array)
            && !uuids.contains(&uuid)
        {
            uuids.push(uuid);
        }
    }
    uuids
}

fn read_items(nbt: &NbtCompoundView<'_, '_>, key: &str) -> Vec<ItemStack> {
    nbt.list(key)
        .and_then(|list| list.compounds())
        .map_or_else(Vec::new, |compounds| {
            compounds
                .into_iter()
                .filter_map(|compound| ItemStack::from_borrowed_compound(&compound))
                .collect()
        })
}

impl BlockEntity for VaultBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let mut data = self.data.lock();
        if let Some(server) = nbt.compound(SERVER_DATA_TAG) {
            data.server.load_into(&server);
        }
        data.config = nbt
            .compound(CONFIG_TAG)
            .and_then(|config| VaultConfig::load(&config))
            .unwrap_or_default();
        if let Some(shared) = nbt.compound(SHARED_DATA_TAG) {
            data.shared.load_into(&shared);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let data = self.data.lock();
        nbt.insert(CONFIG_TAG, data.config.save());
        nbt.insert(SHARED_DATA_TAG, data.shared.save());
        nbt.insert(SERVER_DATA_TAG, data.server.save());
    }

    /// Vanilla `getUpdateTag`: only the shared data.
    fn get_update_tag(&self) -> Option<NbtCompound> {
        let mut nbt = NbtCompound::new();
        nbt.insert(SHARED_DATA_TAG, self.data.lock().shared.save());
        Some(nbt)
    }

    /// Vanilla `VaultBlockEntity.Server.tick`.
    fn tick(&self, world: &Arc<World>) {
        let pos = self.base.pos();
        let state = world.get_block_state(pos);
        let current = state.get_value(STATE);
        let game_time = world.game_time();

        let next = {
            let mut data = self.data.lock();
            if game_time % DISPLAY_CYCLE_TICK_RATE == 0 && current == VaultState::Active {
                let VaultData { config, shared, .. } = &mut *data;
                cycle_display_item_from_loot_table(pos, current, config, shared);
            }
            (game_time >= data.server.state_updating_resumes_at)
                .then(|| Self::tick_and_get_next(world, pos, current, &mut data))
        };

        if let Some(next) = next {
            let next_state = state.set_value(STATE, next);
            if next_state != state {
                self.set_vault_state(world, pos, state, next_state);
            }
        }

        let shared_dirty = {
            let mut data = self.data.lock();
            let any = data.server.is_dirty || data.shared.is_dirty;
            let shared = data.shared.is_dirty;
            data.server.is_dirty = false;
            data.shared.is_dirty = false;
            any.then_some(shared)
        };
        if let Some(shared_dirty) = shared_dirty {
            self.set_changed();
            if shared_dirty {
                world.send_block_updated(pos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::{init_vanilla_registry, vanilla_blocks};

    use super::*;

    fn test_vault() -> VaultBlockEntity {
        init_vanilla_registry();
        VaultBlockEntity::new(
            Weak::new(),
            BlockPos::new(0, 64, 0),
            vanilla_blocks::VAULT.default_state(),
        )
    }

    fn reload(saved: &NbtCompound) -> VaultBlockEntity {
        let mut bytes = Vec::new();
        saved.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(bytes.as_slice())).expect("readable NBT");
        let vault = test_vault();
        vault.load_additional(&borrowed);
        vault
    }

    #[test]
    fn a_default_config_saves_only_its_key() {
        let vault = test_vault();
        let saved = vault.save_custom_only();
        let config = saved.compound(CONFIG_TAG).expect("config written");
        assert!(config.compound("key_item").is_some());
        assert!(config.string("loot_table").is_none());
        assert!(config.double("activation_range").is_none());

        assert_eq!(reload(&saved).config(), VaultConfig::default());
    }

    #[test]
    fn a_config_without_a_key_loads_with_no_key() {
        let mut nbt = NbtCompound::new();
        nbt.insert(CONFIG_TAG, NbtCompound::new());
        let vault = reload(&nbt);
        assert!(vault.config().key_item.is_empty());
    }

    #[test]
    fn an_activation_range_past_the_deactivation_range_falls_back_to_default() {
        let mut config = NbtCompound::new();
        config.insert("activation_range", 10.0_f64);
        config.insert("loot_table", "minecraft:chests/simple_dungeon");
        let mut nbt = NbtCompound::new();
        nbt.insert(CONFIG_TAG, config);
        assert_eq!(reload(&nbt).config(), VaultConfig::default());
    }

    #[test]
    fn rewarded_players_and_pending_items_survive_but_the_total_does_not() {
        let vault = test_vault();
        let player = Uuid::from_u128(7);
        {
            let mut data = vault.data.lock();
            data.server.add_to_rewarded_players(player);
            data.server.set_items_to_eject(vec![
                ItemStack::new(&vanilla_items::DIAMOND),
                ItemStack::new(&vanilla_items::EMERALD),
            ]);
            data.shared
                .set_display_item(&ItemStack::new(&vanilla_items::EMERALD));
        }
        let saved = vault.save_custom_only();
        assert_eq!(
            saved
                .compound(SERVER_DATA_TAG)
                .and_then(|server| server.int("total_ejections_needed")),
            Some(2)
        );

        let reloaded = reload(&saved);
        assert!(reloaded.has_rewarded_player(player));
        assert_eq!(reloaded.items_to_eject().len(), 2);
        assert!(reloaded.display_item().is(&vanilla_items::EMERALD));
        // Vanilla `VaultServerData.set` does not copy the ejection total.
        assert_eq!(reloaded.data.lock().server.total_ejections_needed, 0);
    }

    #[test]
    fn the_oldest_rewarded_player_is_forgotten_past_128() {
        let vault = test_vault();
        let mut data = vault.data.lock();
        for id in 0..=128_u128 {
            data.server.add_to_rewarded_players(Uuid::from_u128(id));
        }
        assert_eq!(data.server.rewarded_players.len(), MAX_REWARD_PLAYERS);
        assert!(!data.server.has_rewarded_player(Uuid::from_u128(0)));
        assert!(data.server.has_rewarded_player(Uuid::from_u128(128)));
    }

    #[test]
    fn ejection_pitch_rises_from_the_first_item_to_the_last() {
        let mut server = VaultServerData::default();
        server.set_items_to_eject(vec![
            ItemStack::new(&vanilla_items::DIAMOND),
            ItemStack::new(&vanilla_items::EMERALD),
            ItemStack::new(&vanilla_items::GOLD_INGOT),
        ]);
        assert!(server.ejection_progress().abs() < f32::EPSILON);
        let _ = server.pop_next_item_to_eject();
        assert!((server.ejection_progress() - 0.5).abs() < f32::EPSILON);
        let _ = server.pop_next_item_to_eject();
        assert!((server.ejection_progress() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_update_tag_holds_only_the_shared_data() {
        let vault = test_vault();
        let tag = vault.get_update_tag().expect("vault syncs");
        assert!(tag.compound(SHARED_DATA_TAG).is_some());
        assert!(tag.compound(SERVER_DATA_TAG).is_none());
        assert!(tag.compound(CONFIG_TAG).is_none());
    }
}
