//! Vanilla `TrialSpawnerConfig`, the `trial_spawner` registry entries and
//! `TrialSpawner.FullConfig`.

use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use rustc_hash::FxHashMap;
use simdnbt::borrow::{NbtCompound as NbtCompoundView, NbtTag as BorrowedNbtTag, read_compound};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::data_components::components::EntityData;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_loot_tables;
use steel_utils::Identifier;
use steel_utils::nbt::{NbtNumeric as _, json_to_nbt, merge_nbt_compounds};

use crate::block_entity::entities::{SpawnData, WeightedSpawnData};

/// Vanilla `TrialSpawner.DEFAULT_TARGET_COOLDOWN_LENGTH`.
pub const DEFAULT_TARGET_COOLDOWN_LENGTH: i32 = 36_000;
/// Vanilla `TrialSpawner.DEFAULT_PLAYER_SCAN_RANGE`.
pub const DEFAULT_PLAYER_SCAN_RANGE: i32 = 14;

/// One weighted loot table of `loot_tables_to_eject`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightedLootTable {
    /// Relative selection weight.
    pub weight: i32,
    /// Loot table key.
    pub table: Identifier,
}

/// Vanilla `TrialSpawnerConfig`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrialSpawnerConfig {
    /// Horizontal spread of spawn positions around the spawner.
    pub spawn_range: i32,
    /// Mobs spawned per wave for one player.
    pub total_mobs: f32,
    /// Mobs alive at once for one player.
    pub simultaneous_mobs: f32,
    /// Extra mobs per wave for each additional player.
    pub total_mobs_added_per_player: f32,
    /// Extra mobs alive at once for each additional player.
    pub simultaneous_mobs_added_per_player: f32,
    /// Ticks between two spawns.
    pub ticks_between_spawn: i32,
    /// Mobs the spawner picks from.
    pub spawn_potentials: Vec<WeightedSpawnData>,
    /// Loot tables a reward is rolled from, one per wave.
    pub loot_tables_to_eject: Vec<WeightedLootTable>,
    /// Loot table of the items an ominous spawner drops on players.
    pub items_to_drop_when_ominous: Identifier,
}

impl Default for TrialSpawnerConfig {
    /// Vanilla `TrialSpawnerConfig.Builder` defaults.
    fn default() -> Self {
        Self {
            spawn_range: 4,
            total_mobs: 6.0,
            simultaneous_mobs: 2.0,
            total_mobs_added_per_player: 2.0,
            simultaneous_mobs_added_per_player: 1.0,
            ticks_between_spawn: 40,
            spawn_potentials: Vec::new(),
            loot_tables_to_eject: vec![
                WeightedLootTable {
                    weight: 1,
                    table: vanilla_loot_tables::SPAWNERS_TRIAL_CHAMBER_CONSUMABLES
                        .key
                        .clone(),
                },
                WeightedLootTable {
                    weight: 1,
                    table: vanilla_loot_tables::SPAWNERS_TRIAL_CHAMBER_KEY.key.clone(),
                },
            ],
            items_to_drop_when_ominous:
                vanilla_loot_tables::SPAWNERS_TRIAL_CHAMBER_ITEMS_TO_DROP_WHEN_OMINOUS
                    .key
                    .clone(),
        }
    }
}

impl TrialSpawnerConfig {
    /// Vanilla `calculateTargetTotalMobs`.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "vanilla floors the float total into an int"
    )]
    pub fn calculate_target_total_mobs(&self, additional_players: usize) -> i32 {
        (self.total_mobs + self.total_mobs_added_per_player * additional_players as f32).floor()
            as i32
    }

    /// Vanilla `calculateTargetSimultaneousMobs`.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "vanilla floors the float total into an int"
    )]
    pub fn calculate_target_simultaneous_mobs(&self, additional_players: usize) -> i32 {
        (self.simultaneous_mobs
            + self.simultaneous_mobs_added_per_player * additional_players as f32)
            .floor() as i32
    }

    /// Vanilla `ticksBetweenItemSpawners`.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "vanilla reads the interval through the config instance"
    )]
    pub const fn ticks_between_item_spawners(&self) -> i64 {
        160
    }

    /// Vanilla `withSpawning(EntityType)`: only `entity_type`, nothing else.
    #[must_use]
    pub fn with_spawning(&self, entity_type: EntityTypeRef) -> Self {
        let mut entity = NbtCompound::new();
        entity.insert("id", entity_type.key.to_string());
        self.with_spawn_data(entity)
    }

    /// Vanilla `withSpawning(TypedEntityData)`: the entity data with its type.
    #[must_use]
    pub fn with_spawning_data(&self, entity_data: &EntityData) -> Self {
        let mut entity = NbtCompound::new();
        merge_nbt_compounds(&mut entity, entity_data.data().as_compound());
        while entity.remove("id").is_some() {}
        entity.insert("id", entity_data.entity_type().key.to_string());
        self.with_spawn_data(entity)
    }

    fn with_spawn_data(&self, entity: NbtCompound) -> Self {
        Self {
            spawn_potentials: vec![WeightedSpawnData {
                weight: 1,
                data: SpawnData {
                    entity_to_spawn: entity,
                    custom_spawn_rules: None,
                    equipment: None,
                },
            }],
            ..self.clone()
        }
    }

    /// Vanilla `DIRECT_CODEC` decode: a present field that is out of range or
    /// malformed rejects the whole config.
    pub(crate) fn from_nbt(nbt: &NbtCompoundView<'_, '_>) -> Option<Self> {
        let default = Self::default();
        let int_in = |key: &str, min: i32, max: i32, fallback: i32| -> Option<i32> {
            match nbt.get(key) {
                None => Some(fallback),
                Some(tag) => tag.codec_i32().filter(|value| (min..=max).contains(value)),
            }
        };
        let float_at_least_zero = |key: &str, fallback: f32| -> Option<f32> {
            match nbt.get(key) {
                None => Some(fallback),
                Some(tag) => tag.codec_f32().filter(|value| *value >= 0.0),
            }
        };
        let spawn_potentials = match nbt.get("spawn_potentials") {
            None => Vec::new(),
            Some(tag) => {
                let list = tag.list()?;
                let mut entries = Vec::new();
                for entry in list.compounds().unwrap_or_default() {
                    let weight = entry.get("weight")?.codec_i32().filter(|w| *w >= 0)?;
                    let data = SpawnData::from_nbt(&entry.compound("data")?)?;
                    entries.push(WeightedSpawnData { weight, data });
                }
                entries
            }
        };
        let loot_tables_to_eject = match nbt.get("loot_tables_to_eject") {
            None => default.loot_tables_to_eject.clone(),
            Some(tag) => {
                let list = tag.list()?;
                let mut entries = Vec::new();
                for entry in list.compounds().unwrap_or_default() {
                    let weight = entry.get("weight")?.codec_i32().filter(|w| *w >= 0)?;
                    let table = entry.string("data")?.to_str().parse().ok()?;
                    entries.push(WeightedLootTable { weight, table });
                }
                entries
            }
        };
        let items_to_drop_when_ominous = match nbt.get("items_to_drop_when_ominous") {
            None => default.items_to_drop_when_ominous.clone(),
            Some(tag) => tag.string()?.to_str().parse().ok()?,
        };
        Some(Self {
            spawn_range: int_in("spawn_range", 1, 128, default.spawn_range)?,
            total_mobs: float_at_least_zero("total_mobs", default.total_mobs)?,
            simultaneous_mobs: float_at_least_zero("simultaneous_mobs", default.simultaneous_mobs)?,
            total_mobs_added_per_player: float_at_least_zero(
                "total_mobs_added_per_player",
                default.total_mobs_added_per_player,
            )?,
            simultaneous_mobs_added_per_player: float_at_least_zero(
                "simultaneous_mobs_added_per_player",
                default.simultaneous_mobs_added_per_player,
            )?,
            ticks_between_spawn: int_in(
                "ticks_between_spawn",
                0,
                i32::MAX,
                default.ticks_between_spawn,
            )?,
            spawn_potentials,
            loot_tables_to_eject,
            items_to_drop_when_ominous,
        })
    }

    /// Vanilla `DIRECT_CODEC` encode: fields equal to the default are left out.
    pub(crate) fn to_nbt(&self) -> NbtCompound {
        let default = Self::default();
        let mut nbt = NbtCompound::new();
        if self.spawn_range != default.spawn_range {
            nbt.insert("spawn_range", self.spawn_range);
        }
        let floats = [
            ("total_mobs", self.total_mobs, default.total_mobs),
            (
                "simultaneous_mobs",
                self.simultaneous_mobs,
                default.simultaneous_mobs,
            ),
            (
                "total_mobs_added_per_player",
                self.total_mobs_added_per_player,
                default.total_mobs_added_per_player,
            ),
            (
                "simultaneous_mobs_added_per_player",
                self.simultaneous_mobs_added_per_player,
                default.simultaneous_mobs_added_per_player,
            ),
        ];
        for (key, value, fallback) in floats {
            if value.to_bits() != fallback.to_bits() {
                nbt.insert(key, value);
            }
        }
        if self.ticks_between_spawn != default.ticks_between_spawn {
            nbt.insert("ticks_between_spawn", self.ticks_between_spawn);
        }
        if !self.spawn_potentials.is_empty() {
            let entries = self
                .spawn_potentials
                .iter()
                .map(|entry| {
                    let mut compound = NbtCompound::new();
                    compound.insert("data", entry.data.to_nbt());
                    compound.insert("weight", entry.weight);
                    compound
                })
                .collect::<Vec<_>>();
            nbt.insert("spawn_potentials", NbtList::Compound(entries));
        }
        if self.loot_tables_to_eject != default.loot_tables_to_eject {
            let entries = self
                .loot_tables_to_eject
                .iter()
                .map(|entry| {
                    let mut compound = NbtCompound::new();
                    compound.insert("data", entry.table.to_string());
                    compound.insert("weight", entry.weight);
                    compound
                })
                .collect::<Vec<_>>();
            nbt.insert("loot_tables_to_eject", NbtList::Compound(entries));
        }
        if self.items_to_drop_when_ominous != default.items_to_drop_when_ominous {
            nbt.insert(
                "items_to_drop_when_ominous",
                self.items_to_drop_when_ominous.to_string(),
            );
        }
        nbt
    }
}

/// Vanilla `Holder<TrialSpawnerConfig>`: a registry entry or an inline config.
#[derive(Clone, Debug)]
pub struct TrialSpawnerConfigHolder {
    key: Option<Identifier>,
    value: Arc<TrialSpawnerConfig>,
}

impl PartialEq for TrialSpawnerConfigHolder {
    fn eq(&self, other: &Self) -> bool {
        match (&self.key, &other.key) {
            (Some(left), Some(right)) => left == right,
            (None, None) => self.value == other.value,
            _ => false,
        }
    }
}

impl TrialSpawnerConfigHolder {
    /// Vanilla `Holder.direct(config)`.
    #[must_use]
    pub fn direct(config: TrialSpawnerConfig) -> Self {
        Self {
            key: None,
            value: Arc::new(config),
        }
    }

    /// The `trial_spawner` registry entry `key`, if it exists.
    #[must_use]
    pub fn reference(key: &Identifier) -> Option<Self> {
        let value = VANILLA_CONFIGS.get(key)?;
        Some(Self {
            key: Some(key.clone()),
            value: Arc::clone(value),
        })
    }

    /// The config value.
    #[must_use]
    pub fn value(&self) -> &TrialSpawnerConfig {
        &self.value
    }

    /// The registry key, for a registry entry.
    #[must_use]
    pub const fn key(&self) -> Option<&Identifier> {
        self.key.as_ref()
    }

    /// Vanilla `RegistryFileCodec` decode: a registry key or an inline config.
    fn from_nbt(tag: &BorrowedNbtTag<'_, '_>) -> Option<Self> {
        if let Some(key) = tag.string() {
            return Self::reference(&key.to_str().parse().ok()?);
        }
        TrialSpawnerConfig::from_nbt(&tag.compound()?).map(Self::direct)
    }

    fn to_nbt(&self) -> NbtTag {
        match &self.key {
            Some(key) => NbtTag::String(key.to_string().into()),
            None => NbtTag::Compound(self.value.to_nbt()),
        }
    }
}

/// Vanilla `TrialSpawner.FullConfig`.
#[derive(Clone, Debug, PartialEq)]
pub struct FullConfig {
    /// Config used while the spawner is not ominous.
    pub normal: TrialSpawnerConfigHolder,
    /// Config used while the spawner is ominous.
    pub ominous: TrialSpawnerConfigHolder,
    /// Ticks the spawner rests after a wave.
    pub target_cooldown_length: i32,
    /// Range in which players are detected.
    pub required_player_range: i32,
}

impl Default for FullConfig {
    /// Vanilla `FullConfig.DEFAULT`.
    fn default() -> Self {
        Self {
            normal: TrialSpawnerConfigHolder::direct(TrialSpawnerConfig::default()),
            ominous: TrialSpawnerConfigHolder::direct(TrialSpawnerConfig::default()),
            target_cooldown_length: DEFAULT_TARGET_COOLDOWN_LENGTH,
            required_player_range: DEFAULT_PLAYER_SCAN_RANGE,
        }
    }
}

impl FullConfig {
    /// Vanilla `MAP_CODEC` decode: any invalid field rejects the config.
    pub(crate) fn from_nbt(nbt: &NbtCompoundView<'_, '_>) -> Option<Self> {
        let default = Self::default();
        let holder = |key: &str, fallback: &TrialSpawnerConfigHolder| match nbt.get(key) {
            None => Some(fallback.clone()),
            Some(tag) => TrialSpawnerConfigHolder::from_nbt(&tag),
        };
        let target_cooldown_length = match nbt.get("target_cooldown_length") {
            None => default.target_cooldown_length,
            Some(tag) => tag.codec_i32().filter(|value| *value >= 0)?,
        };
        let required_player_range = match nbt.get("required_player_range") {
            None => default.required_player_range,
            Some(tag) => tag.codec_i32().filter(|value| (1..=128).contains(value))?,
        };
        Some(Self {
            normal: holder("normal_config", &default.normal)?,
            ominous: holder("ominous_config", &default.ominous)?,
            target_cooldown_length,
            required_player_range,
        })
    }

    /// Vanilla `MAP_CODEC` encode into the block entity's own compound.
    pub(crate) fn save(&self, nbt: &mut NbtCompound) {
        let default = Self::default();
        if self.normal != default.normal {
            nbt.insert("normal_config", self.normal.to_nbt());
        }
        if self.ominous != default.ominous {
            nbt.insert("ominous_config", self.ominous.to_nbt());
        }
        if self.target_cooldown_length != default.target_cooldown_length {
            nbt.insert("target_cooldown_length", self.target_cooldown_length);
        }
        if self.required_player_range != default.required_player_range {
            nbt.insert("required_player_range", self.required_player_range);
        }
    }

    /// Vanilla `overrideEntity`: both configs spawn only `entity_type`.
    #[must_use]
    pub fn override_entity(&self, entity_type: EntityTypeRef) -> Self {
        Self {
            normal: TrialSpawnerConfigHolder::direct(
                self.normal.value().with_spawning(entity_type),
            ),
            ominous: TrialSpawnerConfigHolder::direct(
                self.ominous.value().with_spawning(entity_type),
            ),
            ..self.clone()
        }
    }

    /// Vanilla `overrideEntityData`.
    #[must_use]
    pub fn override_entity_data(&self, entity_data: &EntityData) -> Self {
        Self {
            normal: TrialSpawnerConfigHolder::direct(
                self.normal.value().with_spawning_data(entity_data),
            ),
            ominous: TrialSpawnerConfigHolder::direct(
                self.ominous.value().with_spawning_data(entity_data),
            ),
            ..self.clone()
        }
    }
}

macro_rules! vanilla_trial_spawner_configs {
    ($($path:literal),* $(,)?) => {
        [$((
            $path,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../steel-utils/build_assets/builtin_datapacks/minecraft/trial_spawner/",
                $path,
                ".json"
            )),
        )),*]
    };
}

/// The vanilla `trial_spawner` registry, from the built-in datapack.
static VANILLA_CONFIGS: LazyLock<FxHashMap<Identifier, Arc<TrialSpawnerConfig>>> =
    LazyLock::new(|| {
        let sources = vanilla_trial_spawner_configs![
            "trial_chamber/breeze/normal",
            "trial_chamber/breeze/ominous",
            "trial_chamber/melee/husk/normal",
            "trial_chamber/melee/husk/ominous",
            "trial_chamber/melee/spider/normal",
            "trial_chamber/melee/spider/ominous",
            "trial_chamber/melee/zombie/normal",
            "trial_chamber/melee/zombie/ominous",
            "trial_chamber/ranged/poison_skeleton/normal",
            "trial_chamber/ranged/poison_skeleton/ominous",
            "trial_chamber/ranged/skeleton/normal",
            "trial_chamber/ranged/skeleton/ominous",
            "trial_chamber/ranged/stray/normal",
            "trial_chamber/ranged/stray/ominous",
            "trial_chamber/slow_ranged/poison_skeleton/normal",
            "trial_chamber/slow_ranged/poison_skeleton/ominous",
            "trial_chamber/slow_ranged/skeleton/normal",
            "trial_chamber/slow_ranged/skeleton/ominous",
            "trial_chamber/slow_ranged/stray/normal",
            "trial_chamber/slow_ranged/stray/ominous",
            "trial_chamber/small_melee/baby_zombie/normal",
            "trial_chamber/small_melee/baby_zombie/ominous",
            "trial_chamber/small_melee/cave_spider/normal",
            "trial_chamber/small_melee/cave_spider/ominous",
            "trial_chamber/small_melee/silverfish/normal",
            "trial_chamber/small_melee/silverfish/ominous",
            "trial_chamber/small_melee/slime/normal",
            "trial_chamber/small_melee/slime/ominous",
        ];
        sources
            .into_iter()
            .map(|(path, json)| {
                let config = parse_json_config(json)
                    .unwrap_or_else(|| panic!("invalid vanilla trial spawner config {path}"));
                (Identifier::vanilla(path.to_owned()), Arc::new(config))
            })
            .collect()
    });

fn parse_json_config(json: &str) -> Option<TrialSpawnerConfig> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let NbtTag::Compound(compound) = json_to_nbt(&value)? else {
        return None;
    };
    let mut bytes = Vec::new();
    compound.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(bytes.as_slice())).ok()?;
    TrialSpawnerConfig::from_nbt(&(&borrowed).into())
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_entities};

    use super::*;

    #[test]
    fn every_vanilla_config_loads() {
        init_vanilla_registry();
        assert_eq!(VANILLA_CONFIGS.len(), 28);
        let breeze = TrialSpawnerConfigHolder::reference(&Identifier::vanilla_static(
            "trial_chamber/breeze/normal",
        ))
        .expect("breeze config");
        let config = breeze.value();
        assert_eq!(config.total_mobs.to_bits(), 2.0_f32.to_bits());
        assert_eq!(config.ticks_between_spawn, 20);
        assert_eq!(config.spawn_potentials.len(), 1);
        assert_eq!(
            config.loot_tables_to_eject,
            TrialSpawnerConfig::default().loot_tables_to_eject
        );
    }

    #[test]
    fn target_mobs_grow_with_additional_players() {
        let config = TrialSpawnerConfig::default();
        assert_eq!(config.calculate_target_total_mobs(0), 6);
        assert_eq!(config.calculate_target_total_mobs(2), 10);
        assert_eq!(config.calculate_target_simultaneous_mobs(3), 5);
    }

    #[test]
    fn overriding_the_entity_keeps_the_rest_of_the_config() {
        init_vanilla_registry();
        let full = FullConfig {
            normal: TrialSpawnerConfigHolder::reference(&Identifier::vanilla_static(
                "trial_chamber/melee/zombie/normal",
            ))
            .expect("zombie config"),
            ..FullConfig::default()
        };
        let overridden = full.override_entity(&vanilla_entities::PIG);
        assert_eq!(overridden.normal.key(), None);
        assert_eq!(
            overridden.normal.value().ticks_between_spawn,
            full.normal.value().ticks_between_spawn
        );
        let entity = &overridden.normal.value().spawn_potentials[0]
            .data
            .entity_to_spawn;
        assert_eq!(
            entity.string("id").map(|id| id.to_str().into_owned()),
            Some("minecraft:pig".to_owned())
        );
        assert_eq!(entity.len(), 1);
    }
}
