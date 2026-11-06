//! Conversion of vanilla world data into Steel's per-world `level.toml`.
//!
//! Vanilla 26.x keeps world data in several files:
//! - `level.dat`: game time, spawn, difficulty, default game mode.
//! - `data/minecraft/world_gen_settings.dat`: the seed.
//! - `data/minecraft/game_rules.dat`, `weather.dat` and `world_clocks.dat`: server-wide state.
//! - `dimensions/<namespace>/<name>/data/minecraft/world_border.dat`: one border per dimension.
//!
//! Steel stores this in one `level.toml` per world, so server-wide vanilla state (game rules,
//! weather, clocks, game time) is written into every imported world.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde_json::{Map, Value};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::game_rules::GameRuleValues;
use steel_registry::vanilla_dimension_types::OVERWORLD;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::BlockPos;
use steel_utils::types::Difficulty;

use super::{AnvilImportError, DimensionImport, chunk, nbt, read_world_seed};
use crate::level_data::{LevelData, RespawnData, WeatherState, WorldBorderData};
use crate::world::clock::WorldClockManager;

/// What the level data import wrote and what it could not carry over.
#[derive(Debug, Default)]
pub struct LevelReport {
    /// `level.toml` files written.
    pub written: Vec<PathBuf>,
    /// World seed.
    pub seed: i64,
    /// Vanilla default game mode id (`GameType` in `level.dat`).
    pub game_type: i32,
    /// Whether the vanilla world is hardcore.
    pub hardcore: bool,
    /// Game rules Steel does not know, with their vanilla value.
    pub unknown_game_rules: Vec<String>,
    /// Game rules whose saved value Steel rejects; they keep Steel's default.
    pub invalid_game_rules: Vec<String>,
    /// World clocks Steel does not know.
    pub unknown_clocks: Vec<String>,
}

impl fmt::Display for LevelReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for path in &self.written {
            writeln!(f, "Wrote {}", path.display())?;
        }
        for (title, entries) in [
            ("Unknown game rules dropped", &self.unknown_game_rules),
            (
                "Game rules with invalid values reset to the default",
                &self.invalid_game_rules,
            ),
            ("Unknown world clocks dropped", &self.unknown_clocks),
        ] {
            if !entries.is_empty() {
                writeln!(f, "{title}:")?;
                for entry in entries {
                    writeln!(f, "  {entry}")?;
                }
            }
        }
        Ok(())
    }
}

/// Server-wide vanilla state shared by every imported world.
struct SharedLevelState {
    seed: i64,
    game_time: i64,
    difficulty: Difficulty,
    difficulty_locked: bool,
    initialized: bool,
    respawn: RespawnData,
    game_rules: GameRuleValues,
    weather: WeatherState,
    world_clocks: WorldClockManager,
}

/// Writes `level.toml` for every imported dimension.
///
/// Fails without writing anything if one of the worlds already has a `level.toml`.
pub fn import_level_data(
    world: &Path,
    dimensions: &[DimensionImport],
) -> Result<LevelReport, AnvilImportError> {
    let targets = dimensions
        .iter()
        .map(|dimension| {
            let world_dir = dimension
                .target
                .parent()
                .unwrap_or(&dimension.target)
                .to_path_buf();
            let path = world_dir.join("level.toml");
            if path.exists() {
                return Err(AnvilImportError::LevelDataExists { path });
            }
            Ok(path)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut report = LevelReport::default();
    let shared = read_shared_state(world, &mut report)?;

    for (dimension, path) in dimensions.iter().zip(targets) {
        let border_path = dimension
            .source
            .join("data")
            .join("minecraft")
            .join("world_border.dat");
        let world_border = match read_saved_data(&border_path)? {
            Some(data) => world_border(&data),
            None => WorldBorderData::default(),
        };

        let mut level = LevelData::new_with_seed_and_difficulty(shared.seed, shared.difficulty);
        level.set_saved_game_time(shared.game_time);
        level.world_clocks = shared.world_clocks.clone();
        level.weather = shared.weather.clone();
        level.world_border = world_border;
        level.difficulty_locked = shared.difficulty_locked;
        level.game_rules_values = shared.game_rules.clone();
        level.initialized = shared.initialized;
        if *shared.respawn.dimension() == dimension.key {
            level.set_spawn_pos(shared.respawn.pos());
            level.spawn.angle = shared.respawn.yaw;
        }
        // Steel keeps the global respawn on the domain default world.
        if dimension.key == OVERWORLD.key {
            level.set_respawn_data(shared.respawn.clone());
        }
        level.save_game_rules();

        let content = toml::to_string_pretty(&level).map_err(|error| AnvilImportError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| AnvilImportError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&path, content).map_err(|source| AnvilImportError::Io {
            path: path.clone(),
            source,
        })?;
        report.written.push(path);
    }
    Ok(report)
}

fn read_shared_state(
    world: &Path,
    report: &mut LevelReport,
) -> Result<SharedLevelState, AnvilImportError> {
    let level_path = world.join("level.dat");
    let invalid = |reason: String| AnvilImportError::InvalidFile {
        path: level_path.clone(),
        reason,
    };
    let root = read_gzip_root(&level_path)?;
    let data = root
        .compound("Data")
        .ok_or_else(|| invalid("missing Data".to_owned()))?;
    chunk::check_data_version(data).map_err(invalid)?;

    report.seed = read_world_seed(world)?;
    report.game_type = data.int("GameType").unwrap_or(0);

    let (difficulty, difficulty_locked) = match data.compound("difficulty_settings") {
        Some(settings) => {
            report.hardcore = nbt::boolean(settings, "hardcore");
            let name = nbt::string(settings, "difficulty").unwrap_or_default();
            let difficulty = difficulty_by_name(&name)
                .ok_or_else(|| invalid(format!("unknown difficulty {name:?}")))?;
            (difficulty, nbt::boolean(settings, "locked"))
        }
        None => (Difficulty::default(), false),
    };

    let data_dir = world.join("data").join("minecraft");
    let game_rules = match read_saved_data(&data_dir.join("game_rules.dat"))? {
        Some(rules) => game_rules(&rules, report),
        None => GameRuleValues::new(&REGISTRY.game_rules),
    };
    let weather = match read_saved_data(&data_dir.join("weather.dat"))? {
        Some(weather) => weather_state(&weather),
        None => WeatherState::default(),
    };
    let clocks_path = data_dir.join("world_clocks.dat");
    let world_clocks = match read_saved_data(&clocks_path)? {
        Some(clocks) => {
            world_clocks(&clocks, report).map_err(|reason| AnvilImportError::InvalidFile {
                path: clocks_path.clone(),
                reason,
            })?
        }
        None => WorldClockManager::new(),
    };

    Ok(SharedLevelState {
        seed: report.seed,
        game_time: data.long("Time").unwrap_or(0),
        difficulty,
        difficulty_locked,
        initialized: nbt::boolean(data, "initialized"),
        respawn: respawn(data.compound("spawn")),
        game_rules,
        weather,
        world_clocks,
    })
}

/// Vanilla `Difficulty` is saved by its serialized name.
fn difficulty_by_name(name: &str) -> Option<Difficulty> {
    match name {
        "peaceful" => Some(Difficulty::Peaceful),
        "easy" => Some(Difficulty::Easy),
        "normal" => Some(Difficulty::Normal),
        "hard" => Some(Difficulty::Hard),
        _ => None,
    }
}

/// `LevelData.RespawnData`: `{dimension, pos: [x, y, z], yaw, pitch}`. Vanilla falls back to the
/// overworld origin when it is missing.
fn respawn(spawn: Option<&NbtCompound>) -> RespawnData {
    let default = || RespawnData::of(OVERWORLD.key.clone(), BlockPos::new(0, 0, 0), 0.0, 0.0);
    let Some(spawn) = spawn else {
        return default();
    };
    let dimension = nbt::string(spawn, "dimension")
        .as_deref()
        .and_then(nbt::identifier);
    let pos = spawn.int_array("pos").and_then(|pos| match *pos {
        [x, y, z] => Some(BlockPos::new(x, y, z)),
        _ => None,
    });
    match (dimension, pos) {
        (Some(dimension), Some(pos)) => RespawnData::of(
            dimension,
            pos,
            spawn.float("yaw").unwrap_or(0.0),
            spawn.float("pitch").unwrap_or(0.0),
        ),
        _ => default(),
    }
}

/// `game_rules.dat` stores every rule by id: booleans as bytes, integers as ints.
fn game_rules(rules: &NbtCompound, report: &mut LevelReport) -> GameRuleValues {
    let mut values = GameRuleValues::new(&REGISTRY.game_rules);
    for (name, tag) in rules.iter() {
        let name = name.to_str();
        let rule = nbt::identifier(&name).and_then(|key| REGISTRY.game_rules.by_key(&key));
        let Some(rule) = rule else {
            report
                .unknown_game_rules
                .push(format!("{name} = {}", tag_value(tag)));
            continue;
        };
        let default = rule.serialize_erased_value(&rule.default_erased_value());
        let value = match (default, tag) {
            (Value::Bool(_), NbtTag::Byte(value)) => Some(Value::Bool(*value != 0)),
            (Value::Number(_), NbtTag::Int(value)) => Some(Value::from(*value)),
            _ => None,
        };
        let applied = value.is_some_and(|value| {
            values.set_serialized_by_name(&name, &value, &REGISTRY.game_rules)
        });
        if !applied {
            report
                .invalid_game_rules
                .push(format!("{name} = {}", tag_value(tag)));
        }
    }
    values
}

fn tag_value(tag: &NbtTag) -> String {
    match tag {
        NbtTag::Byte(value) => value.to_string(),
        NbtTag::Int(value) => value.to_string(),
        NbtTag::String(value) => format!("{:?}", value.to_str()),
        _ => "<unsupported tag>".to_owned(),
    }
}

/// `WeatherData`: the same fields as Steel's weather state.
fn weather_state(weather: &NbtCompound) -> WeatherState {
    WeatherState {
        raining: nbt::boolean(weather, "raining"),
        rain_time: weather.int("rain_time").unwrap_or(0),
        thundering: nbt::boolean(weather, "thundering"),
        thunder_time: weather.int("thunder_time").unwrap_or(0),
        clear_weather_time: weather.int("clear_weather_time").unwrap_or(0),
    }
}

/// `WorldBorder.Settings`: the same fields as Steel's border data; missing fields keep the
/// vanilla defaults.
fn world_border(border: &NbtCompound) -> WorldBorderData {
    let default = WorldBorderData::default();
    WorldBorderData {
        center_x: border.double("center_x").unwrap_or(default.center_x),
        center_z: border.double("center_z").unwrap_or(default.center_z),
        damage_per_block: border
            .double("damage_per_block")
            .unwrap_or(default.damage_per_block),
        safe_zone: border.double("safe_zone").unwrap_or(default.safe_zone),
        warning_blocks: border
            .int("warning_blocks")
            .unwrap_or(default.warning_blocks),
        warning_time: border.int("warning_time").unwrap_or(default.warning_time),
        size: border.double("size").unwrap_or(default.size),
        lerp_time: border.long("lerp_time").unwrap_or(default.lerp_time),
        lerp_target: border.double("lerp_target").unwrap_or(default.lerp_target),
    }
}

/// `world_clocks.dat` maps clock ids to `ClockState {total_ticks, partial_tick, rate, paused}`,
/// the same fields Steel persists, with the same defaults for missing ones.
fn world_clocks(
    clocks: &NbtCompound,
    report: &mut LevelReport,
) -> Result<WorldClockManager, String> {
    let mut states = Map::new();
    for (key, tag) in clocks.iter() {
        let key = key.to_str();
        let known =
            nbt::identifier(&key).is_some_and(|id| REGISTRY.world_clocks.by_key(&id).is_some());
        let NbtTag::Compound(state) = tag else {
            return Err(format!("clock {key} is not a compound"));
        };
        if !known {
            report.unknown_clocks.push(key.into_owned());
            continue;
        }
        let mut fields = Map::new();
        if let Some(total_ticks) = state.long("total_ticks") {
            fields.insert("total_ticks".to_owned(), Value::from(total_ticks));
        }
        if let Some(partial_tick) = state.float("partial_tick") {
            fields.insert("partial_tick".to_owned(), Value::from(partial_tick));
        }
        if let Some(rate) = state.float("rate") {
            fields.insert("rate".to_owned(), Value::from(rate));
        }
        if state.contains("paused") {
            fields.insert(
                "paused".to_owned(),
                Value::Bool(nbt::boolean(state, "paused")),
            );
        }
        states.insert(key.into_owned(), Value::Object(fields));
    }
    let mut manager: WorldClockManager =
        serde_json::from_value(Value::Object(states)).map_err(|error| error.to_string())?;
    manager
        .initialize_registered_clocks()
        .map_err(|error| error.to_string())?;
    Ok(manager)
}

/// Reads the `data` compound of a vanilla `SavedData` file, or `None` if the file is missing.
fn read_saved_data(path: &Path) -> Result<Option<NbtCompound>, AnvilImportError> {
    if !path.exists() {
        return Ok(None);
    }
    let invalid = |reason: String| AnvilImportError::InvalidFile {
        path: path.to_path_buf(),
        reason,
    };
    let root = read_gzip_root(path)?;
    chunk::check_data_version(&root).map_err(invalid)?;
    root.compound("data")
        .cloned()
        .map(Some)
        .ok_or_else(|| invalid("missing data".to_owned()))
}

/// Reads a gzip-compressed NBT file.
pub(super) fn read_gzip_root(path: &Path) -> Result<NbtCompound, AnvilImportError> {
    let invalid = |reason: String| AnvilImportError::InvalidFile {
        path: path.to_path_buf(),
        reason,
    };
    let compressed = fs::read(path).map_err(|source| AnvilImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    GzDecoder::new(&compressed[..])
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(error.to_string()))?;
    nbt::read_root(&bytes).map_err(invalid)
}
