//! Conversion of vanilla player saves (`players/data`, `players/stats`, `players/advancements`).
//!
//! Vanilla 26.x keeps one `<uuid>.dat` per player plus `<uuid>.json` stats and advancements.
//! Each player is written to Steel's player storage of one domain, and the domain becomes the
//! player's last active domain.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::DateTime;
use rustc_hash::FxHashMap;
use serde_json::Value;
use simdnbt::borrow;
use simdnbt::owned::{NbtCompound, NbtList};
use steel_registry::item_stack::ItemStack;
use steel_registry::stat::Stat;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::{BlockPos, Identifier};
use uuid::Uuid;

use super::entities::{convert_entity, entity_uuid};
use super::level::read_gzip_root;
use super::report::ImportReport;
use super::{AnvilImportError, chunk, nbt};
use crate::level_data::RespawnData;
use crate::player::PlayerRespawnConfig;
use crate::player::advancement::progress::{AdvancementProgress, CriterionProgress};
use crate::player::player_data::{
    PLAYER_DATA_VERSION, PersistentAbilities, PersistentEnderPearl, PersistentPlayerData,
    PersistentRootVehicle, PersistentSlot, PersistentStat,
};
use crate::player::player_data_storage::{GlobalPlayerData, PlayerDataStorage};

/// Vanilla `EntityEquipment` keys and the player container slots Steel stores them in.
const EQUIPMENT_SLOTS: [(&str, i8); 7] = [
    ("feet", 36),
    ("legs", 37),
    ("chest", 38),
    ("head", 39),
    ("offhand", 40),
    ("body", 41),
    ("saddle", 42),
];

/// Vanilla advancement criteria timestamps, `yyyy-MM-dd HH:mm:ss Z`.
const ADVANCEMENT_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S %z";

/// What the player import wrote and what it could not carry over.
#[derive(Debug, Default)]
pub struct PlayerReport {
    /// Players written to Steel's storage.
    pub players: usize,
    /// Players that could not be read, with the reason.
    pub failed_players: Vec<(PathBuf, String)>,
    /// Items Steel does not know, dropped from inventories and ender chests.
    pub unknown_items: BTreeMap<String, usize>,
    /// Stats Steel does not know.
    pub unknown_stats: BTreeMap<String, usize>,
    /// Advancements Steel does not know.
    pub unknown_advancements: BTreeMap<String, usize>,
    /// Vehicles and ender pearls dropped because Steel does not know their type.
    pub unknown_entities: BTreeMap<String, usize>,
    /// Active effects dropped because Steel does not save player effects yet, by effect id.
    pub dropped_effects: BTreeMap<String, usize>,
    /// Players whose recipe book was dropped because Steel has no recipe book yet.
    pub dropped_recipe_books: usize,
}

impl PlayerReport {
    fn merge_entities(&mut self, entities: ImportReport) {
        for (id, count) in entities.unknown_entities {
            *self.unknown_entities.entry(id).or_default() += count;
        }
    }
}

impl fmt::Display for PlayerReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "players: {}", self.players)?;
        for (title, map) in [
            ("Unknown items dropped", &self.unknown_items),
            ("Unknown stats dropped", &self.unknown_stats),
            ("Unknown advancements dropped", &self.unknown_advancements),
            (
                "Unknown vehicles and ender pearls dropped",
                &self.unknown_entities,
            ),
            (
                "Active effects dropped (Steel does not save them yet)",
                &self.dropped_effects,
            ),
        ] {
            if !map.is_empty() {
                writeln!(f, "{title}:")?;
                for (key, count) in map {
                    writeln!(f, "  {key}: {count}")?;
                }
            }
        }
        if self.dropped_recipe_books > 0 {
            writeln!(
                f,
                "Recipe books dropped (Steel has no recipe book yet): {}",
                self.dropped_recipe_books
            )?;
        }
        if !self.failed_players.is_empty() {
            writeln!(f, "Failed players:")?;
            for (path, reason) in &self.failed_players {
                writeln!(f, "  {}: {reason}", path.display())?;
            }
        }
        Ok(())
    }
}

/// Imports every player of a vanilla save into Steel's player storage under `save_path`.
///
/// Fails without writing anything if one of the players already has Steel data in `domain`.
pub async fn import_players(
    world: &Path,
    save_path: &Path,
    domain: &str,
) -> Result<PlayerReport, AnvilImportError> {
    let players_dir = world.join("players");
    let player_files = list_player_files(&players_dir.join("data"))?;
    let storage = PlayerDataStorage::on_disk(save_path.to_path_buf())
        .await
        .map_err(|source| AnvilImportError::Io {
            path: save_path.to_path_buf(),
            source,
        })?;

    for (uuid, _) in &player_files {
        let existing =
            storage
                .load_domain(domain, *uuid)
                .await
                .map_err(|source| AnvilImportError::Io {
                    path: save_path.to_path_buf(),
                    source,
                })?;
        if existing.is_some() {
            return Err(AnvilImportError::PlayerDataExists {
                uuid: *uuid,
                domain: domain.to_owned(),
            });
        }
    }

    let mut report = PlayerReport::default();
    for (uuid, path) in player_files {
        let data = match convert_player(&players_dir, uuid, &path, &mut report) {
            Ok(data) => data,
            Err(reason) => {
                report.failed_players.push((path, reason));
                continue;
            }
        };
        let io_error = |source| AnvilImportError::Io {
            path: save_path.to_path_buf(),
            source,
        };
        storage
            .save_domain_data(domain, uuid, &data)
            .await
            .map_err(io_error)?;
        storage
            .save_global(
                uuid,
                &GlobalPlayerData {
                    last_active_domain: domain.to_owned(),
                },
            )
            .await
            .map_err(io_error)?;
        report.players += 1;
    }
    Ok(report)
}

/// Lists `<uuid>.dat` files, sorted by UUID. A missing directory has no players.
fn list_player_files(dir: &Path) -> Result<Vec<(Uuid, PathBuf)>, AnvilImportError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AnvilImportError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|source| AnvilImportError::Io {
                path: dir.to_path_buf(),
                source,
            })?
            .path();
        let uuid = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".dat"))
            .and_then(|stem| Uuid::parse_str(stem).ok());
        if let Some(uuid) = uuid {
            files.push((uuid, path));
        }
    }
    files.sort();
    Ok(files)
}

/// Converts one player. Missing stats or advancements files mean the player has none.
pub(super) fn convert_player(
    players_dir: &Path,
    uuid: Uuid,
    data_path: &Path,
    report: &mut PlayerReport,
) -> Result<PersistentPlayerData, String> {
    let root = read_gzip_root(data_path).map_err(|error| error.to_string())?;
    chunk::check_data_version(&root)?;

    let mut data = convert_player_data(&root, report)?;

    let stats_path = players_dir.join("stats").join(format!("{uuid}.json"));
    if let Some(stats) = read_json(&stats_path)? {
        data.stats = convert_stats(&stats, report)?;
    }
    let advancements_path = players_dir
        .join("advancements")
        .join(format!("{uuid}.json"));
    if let Some(advancements) = read_json(&advancements_path)? {
        data.advancements = convert_advancements(&advancements, report)?;
    }
    Ok(data)
}

/// Converts the `ServerPlayer.addAdditionalSaveData` compound.
fn convert_player_data(
    root: &NbtCompound,
    report: &mut PlayerReport,
) -> Result<PersistentPlayerData, String> {
    let pos = nbt::doubles::<3>(root, "Pos").ok_or("missing Pos")?;
    let world = nbt::string(root, "Dimension").unwrap_or_else(|| "minecraft:overworld".to_owned());

    let mut inventory = items(root.list("Inventory"), report);
    if let Some(equipment) = root.compound("equipment") {
        for (key, slot) in EQUIPMENT_SLOTS {
            if let Some(item) = equipment
                .compound(key)
                .and_then(|item| item_stack(item, report))
            {
                inventory.push(PersistentSlot { slot, item });
            }
        }
    }

    for effect in root
        .list("active_effects")
        .and_then(|list| list.compounds())
        .unwrap_or_default()
    {
        let id = nbt::string(effect, "id").unwrap_or_else(|| "<missing id>".to_owned());
        ImportReport::count(&mut report.dropped_effects, id);
    }
    if root.contains("recipeBook") {
        report.dropped_recipe_books += 1;
    }

    let mut entities = ImportReport::default();
    let root_vehicle = root.compound("RootVehicle").and_then(|vehicle| {
        let attach = entity_uuid(vehicle, "Attach")?;
        let entity = convert_entity(vehicle.compound("Entity")?, &mut entities)?;
        Some(PersistentRootVehicle { attach, entity })
    });
    let ender_pearls = root
        .list("ender_pearls")
        .and_then(|list| list.compounds())
        .unwrap_or_default()
        .iter()
        .filter_map(|pearl| {
            let world = nbt::string(pearl, "ender_pearl_dimension")?;
            let pearl = nbt::without_keys(pearl, &["ender_pearl_dimension"]);
            let entity = convert_entity(&pearl, &mut entities)?;
            Some(PersistentEnderPearl { world, entity })
        })
        .collect();
    report.merge_entities(entities);

    let abilities = root.compound("abilities");
    let ability = |key: &str| abilities.is_some_and(|abilities| nbt::boolean(abilities, key));
    let ability_float = |key: &str, default: f32| {
        abilities
            .and_then(|abilities| abilities.float(key))
            .unwrap_or(default)
    };

    Ok(PersistentPlayerData {
        pos,
        motion: nbt::doubles::<3>(root, "Motion").unwrap_or_default(),
        rotation: nbt::floats::<2>(root, "Rotation").unwrap_or_default(),
        on_ground: nbt::boolean(root, "OnGround"),
        fall_flying: nbt::boolean(root, "FallFlying"),
        remaining_fire_ticks: i32::from(root.short("Fire").unwrap_or_default()),
        ticks_frozen: root.int("TicksFrozen").unwrap_or_default(),
        // Powder snow contact is recomputed every tick and is not saved by vanilla.
        is_in_powder_snow: false,
        was_in_powder_snow: false,
        has_visual_fire: nbt::boolean(root, "HasVisualFire"),
        health: root.float("Health").unwrap_or(20.0),
        game_mode: root.int("playerGameType").unwrap_or_default(),
        prev_game_mode: root.int("previousPlayerGameType"),
        abilities: PersistentAbilities {
            invulnerable: ability("invulnerable"),
            flying: ability("flying"),
            may_fly: ability("mayfly"),
            instabuild: ability("instabuild"),
            may_build: abilities
                .is_none_or(|abilities| abilities.byte("mayBuild").is_none_or(|value| value != 0)),
            flying_speed: ability_float("flySpeed", 0.05),
            walking_speed: ability_float("walkSpeed", 0.1),
        },
        inventory,
        selected_slot: root.int("SelectedItemSlot").unwrap_or_default(),
        world,
        food_level: root.int("foodLevel").unwrap_or(20),
        food_saturation_level: root.float("foodSaturationLevel").unwrap_or(5.0),
        food_exhaustion_level: root.float("foodExhaustionLevel").unwrap_or_default(),
        food_tick_timer: root.int("foodTickTimer").unwrap_or_default(),
        data_version: PLAYER_DATA_VERSION,
        experience_level: root.int("XpLevel").unwrap_or_default(),
        experience_progress: root.float("XpP").unwrap_or_default(),
        experience_total: root.int("XpTotal").unwrap_or_default(),
        enchantment_seed: root.int("XpSeed").unwrap_or_default(),
        score: root.int("Score").unwrap_or_default(),
        seen_credits: nbt::boolean(root, "seenCredits"),
        root_vehicle,
        respawn_config: root.compound("respawn").and_then(respawn_config),
        ender_pearls,
        ender_items: items(root.list("EnderItems"), report),
        stats: Vec::new(),
        advancements: Vec::new(),
    })
}

/// Reads a vanilla `ItemStackWithSlot` list.
fn items(list: Option<&NbtList>, report: &mut PlayerReport) -> Vec<PersistentSlot> {
    list.and_then(|list| list.compounds())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let slot = entry.byte("Slot")?;
            let item = item_stack(entry, report)?;
            Some(PersistentSlot { slot, item })
        })
        .collect()
}

fn item_stack(entry: &NbtCompound, report: &mut PlayerReport) -> Option<ItemStack> {
    let item = nbt::without_keys(entry, &["Slot"]);
    let bytes = nbt::compound_bytes(&item);
    let parsed = borrow::read_compound(&mut io::Cursor::new(&bytes[..])).ok();
    let stack = parsed.and_then(|compound| {
        ItemStack::from_borrowed_compound(&borrow::NbtCompound::from(&compound))
    });
    if stack.is_none() {
        let id = nbt::string(entry, "id").unwrap_or_else(|| "<missing id>".to_owned());
        ImportReport::count(&mut report.unknown_items, id);
    }
    stack
}

/// `ServerPlayer.RespawnConfig`: `{dimension, pos, yaw, pitch, forced}`.
fn respawn_config(respawn: &NbtCompound) -> Option<PlayerRespawnConfig> {
    let dimension = nbt::identifier(&nbt::string(respawn, "dimension")?)?;
    let [x, y, z] = *respawn.int_array("pos")? else {
        return None;
    };
    Some(PlayerRespawnConfig {
        respawn_data: RespawnData::of(
            dimension,
            BlockPos::new(x, y, z),
            respawn.float("yaw").unwrap_or_default(),
            respawn.float("pitch").unwrap_or_default(),
        ),
        forced: nbt::boolean(respawn, "forced"),
    })
}

/// Vanilla `ServerStatsCounter`: `{"stats": {type: {value: count}}}`.
fn convert_stats(stats: &Value, report: &mut PlayerReport) -> Result<Vec<PersistentStat>, String> {
    let types = stats
        .get("stats")
        .and_then(Value::as_object)
        .ok_or("stats file has no stats object")?;
    let mut converted = Vec::new();
    for (type_key, values) in types {
        let values = values
            .as_object()
            .ok_or_else(|| format!("stat type {type_key} is not an object"))?;
        let stat_type = nbt::identifier(type_key).and_then(|key| REGISTRY.stat_types.by_key(&key));
        for (value_key, count) in values {
            let stat = stat_type.and_then(|stat_type| {
                let value = stat_type.value_from_key(&nbt::identifier(value_key)?)?;
                Some(Stat::from_erased(stat_type, value))
            });
            let count = count
                .as_i64()
                .and_then(|count| i32::try_from(count).ok())
                .ok_or_else(|| format!("stat {type_key} {value_key} is not an int"))?;
            match stat {
                Some(stat) => converted.push(PersistentStat { stat, count }),
                None => {
                    ImportReport::count(
                        &mut report.unknown_stats,
                        format!("{type_key} {value_key}"),
                    );
                }
            }
        }
    }
    Ok(converted)
}

/// Vanilla `PlayerAdvancements`: `{id: {criteria: {name: date}, done}, DataVersion}`.
fn convert_advancements(
    advancements: &Value,
    report: &mut PlayerReport,
) -> Result<Vec<(Identifier, AdvancementProgress)>, String> {
    let entries = advancements
        .as_object()
        .ok_or("advancements file is not an object")?;
    let mut converted = Vec::new();
    for (key, progress) in entries {
        if key == "DataVersion" {
            continue;
        }
        let Some(id) =
            nbt::identifier(key).filter(|id| REGISTRY.advancements.value_by_key(id).is_some())
        else {
            ImportReport::count(&mut report.unknown_advancements, key.clone());
            continue;
        };
        let mut criteria = FxHashMap::default();
        for (name, date) in progress
            .get("criteria")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let date = date
                .as_str()
                .ok_or_else(|| format!("criterion {key} {name} has no date"))?;
            let obtained = DateTime::parse_from_str(date, ADVANCEMENT_DATE_FORMAT)
                .map_err(|error| format!("criterion {key} {name}: {error}"))?;
            criteria.insert(
                Cow::Owned(name.clone()),
                CriterionProgress(Some(SystemTime::from(obtained))),
            );
        }
        converted.push((id, AdvancementProgress::new(criteria)));
    }
    Ok(converted)
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}
