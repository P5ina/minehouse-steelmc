//! Vanilla `TrialSpawnerStateData` and its `Packed` codec.

use rand::rngs::StdRng;
use rand::{RngExt as _, SeedableRng as _, rng};
use simdnbt::borrow::NbtCompound as NbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::blocks::properties::TrialSpawnerState;
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::LootContext;
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::{BlockPos, Identifier, PackedBlockPos, UuidExt as _};
use uuid::Uuid;

use super::config::TrialSpawnerConfig;
use crate::block_entity::entities::SpawnData;
use crate::world::World;

const REGISTERED_PLAYERS_TAG: &str = "registered_players";
const CURRENT_MOBS_TAG: &str = "current_mobs";
const COOLDOWN_ENDS_AT_TAG: &str = "cooldown_ends_at";
const NEXT_MOB_SPAWNS_AT_TAG: &str = "next_mob_spawns_at";
const TOTAL_MOBS_SPAWNED_TAG: &str = "total_mobs_spawned";
const SPAWN_DATA_TAG: &str = "spawn_data";
const EJECTING_LOOT_TABLE_TAG: &str = "ejecting_loot_table";

/// Vanilla `TrialSpawnerStateData`.
#[derive(Default)]
pub(super) struct StateData {
    /// Vanilla `detectedPlayers`, in the order they were found.
    pub(super) detected_players: Vec<Uuid>,
    /// Vanilla `currentMobs`, in the order they spawned.
    pub(super) current_mobs: Vec<Uuid>,
    pub(super) cooldown_ends_at: i64,
    pub(super) next_mob_spawns_at: i64,
    pub(super) total_mobs_spawned: i32,
    pub(super) next_spawn_data: Option<SpawnData>,
    pub(super) ejecting_loot_table: Option<Identifier>,
    /// Whether vanilla `displayEntity` is set. It is never cleared, like the
    /// vanilla field.
    pub(super) has_display_entity: bool,
    /// Vanilla `dispensing`: the ominous items, each weighted by its count.
    dispensing: Option<Vec<ItemStack>>,
}

impl StateData {
    /// Vanilla `reset`.
    pub(super) fn reset(&mut self) {
        self.current_mobs.clear();
        self.next_spawn_data = None;
        self.reset_statistics();
    }

    /// Vanilla `resetStatistics`.
    pub(super) fn reset_statistics(&mut self) {
        self.detected_players.clear();
        self.total_mobs_spawned = 0;
        self.next_mob_spawns_at = 0;
        self.cooldown_ends_at = 0;
    }

    /// Vanilla `isReadyToOpenShutter`, in vanilla's float math.
    #[expect(
        clippy::cast_precision_loss,
        reason = "vanilla compares the tick counts as floats"
    )]
    pub(super) fn is_ready_to_open_shutter(
        &self,
        game_time: i64,
        delay_before_open: f32,
        target_cooldown_length: i32,
    ) -> bool {
        let cooldown_started_at = self.cooldown_ends_at - i64::from(target_cooldown_length);
        game_time as f32 >= cooldown_started_at as f32 + delay_before_open
    }

    /// Vanilla `isReadyToEjectItems`, in vanilla's float math.
    #[expect(
        clippy::cast_precision_loss,
        reason = "vanilla takes the remainder of the tick count as a float"
    )]
    pub(super) fn is_ready_to_eject_items(
        &self,
        game_time: i64,
        time_between_ejections: f32,
        target_cooldown_length: i32,
    ) -> bool {
        let cooldown_started_at = self.cooldown_ends_at - i64::from(target_cooldown_length);
        (game_time - cooldown_started_at) as f32 % time_between_ejections == 0.0
    }

    /// Vanilla `getDispensingItems(...).getRandom(...)`: one ominous item,
    /// picked by weight from a roll that is fixed for this area of the world.
    pub(super) fn dispensing_item(
        &mut self,
        world: &World,
        config: &TrialSpawnerConfig,
        pos: BlockPos,
    ) -> Option<ItemStack> {
        if self.dispensing.is_none() {
            let table = REGISTRY
                .loot_tables
                .by_key(&config.items_to_drop_when_ominous)?;
            #[expect(
                clippy::cast_sign_loss,
                reason = "the seed's bits are reused as the random source's seed"
            )]
            let seed = low_resolution_position_seed(world.seed(), pos) as u64;
            let mut random = StdRng::seed_from_u64(seed);
            let drops = table.get_random_items(&mut LootContext::new(&mut random));
            if drops.is_empty() {
                return None;
            }
            self.dispensing = Some(drops);
        }
        let drops = self.dispensing.as_ref()?;
        let total = drops
            .iter()
            .map(|drop| i64::from(drop.count()))
            .sum::<i64>();
        if total <= 0 {
            return None;
        }
        let mut selection = rng().random_range(0..total);
        for drop in drops {
            let weight = i64::from(drop.count());
            if selection < weight {
                return Some(drop.copy_with_count(1));
            }
            selection -= weight;
        }
        None
    }

    /// Vanilla `Packed.MAP_CODEC` decode: each field falls back on its own.
    pub(super) fn load(&mut self, nbt: &NbtCompoundView<'_, '_>) {
        self.detected_players = read_uuids(nbt, REGISTERED_PLAYERS_TAG);
        self.current_mobs = read_uuids(nbt, CURRENT_MOBS_TAG);
        self.cooldown_ends_at = nbt.long(COOLDOWN_ENDS_AT_TAG).unwrap_or(0);
        self.next_mob_spawns_at = nbt.long(NEXT_MOB_SPAWNS_AT_TAG).unwrap_or(0);
        self.total_mobs_spawned = nbt
            .int(TOTAL_MOBS_SPAWNED_TAG)
            .filter(|total| *total >= 0)
            .unwrap_or(0);
        self.next_spawn_data = nbt
            .compound(SPAWN_DATA_TAG)
            .and_then(|data| SpawnData::from_nbt(&data));
        self.ejecting_loot_table = nbt
            .string(EJECTING_LOOT_TABLE_TAG)
            .and_then(|key| key.to_str().parse().ok());
    }

    /// Vanilla `Packed.MAP_CODEC` encode: defaults are left out.
    pub(super) fn save(&self, nbt: &mut NbtCompound) {
        if !self.detected_players.is_empty() {
            nbt.insert(REGISTERED_PLAYERS_TAG, uuid_list(&self.detected_players));
        }
        if !self.current_mobs.is_empty() {
            nbt.insert(CURRENT_MOBS_TAG, uuid_list(&self.current_mobs));
        }
        if self.cooldown_ends_at != 0 {
            nbt.insert(COOLDOWN_ENDS_AT_TAG, self.cooldown_ends_at);
        }
        if self.next_mob_spawns_at != 0 {
            nbt.insert(NEXT_MOB_SPAWNS_AT_TAG, self.next_mob_spawns_at);
        }
        if self.total_mobs_spawned != 0 {
            nbt.insert(TOTAL_MOBS_SPAWNED_TAG, self.total_mobs_spawned);
        }
        if let Some(data) = &self.next_spawn_data {
            nbt.insert(SPAWN_DATA_TAG, data.to_nbt());
        }
        if let Some(table) = &self.ejecting_loot_table {
            nbt.insert(EJECTING_LOOT_TABLE_TAG, table.to_string());
        }
    }

    /// Vanilla `getUpdateTag`.
    pub(super) fn update_tag(&self, state: TrialSpawnerState) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if state == TrialSpawnerState::Active {
            nbt.insert(NEXT_MOB_SPAWNS_AT_TAG, self.next_mob_spawns_at);
        }
        if let Some(data) = &self.next_spawn_data {
            nbt.insert(SPAWN_DATA_TAG, data.to_nbt());
        }
        nbt
    }
}

/// Vanilla `lowResolutionPosition`: the seed plus the packed position of the
/// 30×20×30 cell around `pos`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "vanilla floors the float cell coordinates into ints"
)]
fn low_resolution_position_seed(seed: i64, pos: BlockPos) -> i64 {
    let cell = BlockPos::new(
        (pos.x() as f32 / 30.0).floor() as i32,
        (pos.y() as f32 / 20.0).floor() as i32,
        (pos.z() as f32 / 30.0).floor() as i32,
    );
    seed.wrapping_add(PackedBlockPos::from(cell).as_raw())
}

fn uuid_list(uuids: &[Uuid]) -> NbtList {
    NbtList::from(
        uuids
            .iter()
            .map(|uuid| NbtTag::IntArray(uuid.to_int_array().to_vec()))
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
