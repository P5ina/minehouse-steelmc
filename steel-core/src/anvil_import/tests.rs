//! Tests against a world saved by the vanilla 26.3 server.
//!
//! `fixtures/overworld` holds chunks (0, 0) and (0, 3) of a seed 12345 world. Chunk (0, 0) was
//! edited with these commands before saving:
//!
//! ```text
//! setblock 0 100 0 chest[facing=north]{Items:[{Slot:0b,id:diamond,count:5},
//!     {Slot:3b,id:iron_sword,count:1,components:{damage:7}}],CustomName:"Сундук"}
//! setblock 1 100 0 oak_sign{front_text:{messages:["\"hello\"","\"мир\"","\"\"","\"\""]}}
//! setblock 2 100 0 furnace[facing=east,lit=true]{Items:[{Slot:0b,id:raw_iron,count:3}]}
//! setblock 3 100 0 oak_stairs[facing=south,half=top,waterlogged=true]
//! setblock 6 100 0 red_bed[facing=east,part=head]
//! setblock 5 100 1 bell[attachment=floor,facing=north]
//! setblock 7 100 1 blast_furnace[facing=north]
//! setblock 0 -60 0 deepslate_diamond_ore
//! setblock 0 300 0 glowstone
//! summon cow 0.5 101 2.5 {CustomName:"Бурёнка",NoAI:1b,Tags:["imported"]}
//! summon armor_stand 3.5 101 3.5
//! summon pig 6.5 101 6.5 {Passengers:[{id:chicken}]}
//! ```
//!
//! Chunk (0, 3) is still at `minecraft:initialize_light`.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs, process, slice};

use simdnbt::borrow::read_compound;
use simdnbt::owned::{BaseNbt, NbtCompound};
use steel_utils::text::DisplayResolutor;

use steel_registry::vanilla_dimension_types::OVERWORLD;
use steel_registry::{REGISTRY, vanilla_blocks};
use steel_utils::{BlockPos, ChunkPos, Identifier};

use super::*;
use crate::bootstrap::init_globals;
use crate::chunk::full_chunk::FullChunkRef;
use crate::chunk_saver::ChunkStorage;
use crate::player::player_data::{PersistentPlayerData, PersistentSlot};

const OVERWORLD_MIN_Y: i32 = -64;
const OVERWORLD_HEIGHT: i32 = 384;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/anvil_import/fixtures/overworld")
}

fn read_fixture_chunk(kind: &str, pos: ChunkPos) -> Option<Vec<u8>> {
    let region = AnvilRegion::open(&fixture_dir().join(kind).join("r.0.0.mca"))
        .expect("fixture region should open");
    region.read_chunk(pos).expect("fixture chunk should read")
}

fn convert_fixture(pos: ChunkPos, report: &mut ImportReport) -> ChunkConversion {
    let chunk = read_fixture_chunk("region", pos).expect("fixture chunk should exist");
    let entities = read_fixture_chunk("entities", pos);
    let poi_bytes = read_fixture_chunk("poi", pos);
    convert_chunk(
        &chunk,
        entities.as_deref(),
        poi_bytes.as_deref(),
        pos,
        OVERWORLD_MIN_Y,
        OVERWORLD_HEIGHT,
        report,
    )
    .expect("fixture chunk should convert")
}

fn state(name: &'static str, properties: &[(&str, &str)]) -> steel_utils::BlockStateId {
    REGISTRY
        .blocks
        .state_id_from_properties(&Identifier::vanilla_static(name), properties)
        .expect("test block state should exist")
}

#[test]
fn vanilla_full_chunk_loads_in_steel() {
    init_globals();
    let pos = ChunkPos::new(0, 0);
    let mut report = ImportReport::default();
    let ChunkConversion::Imported(persistent) = convert_fixture(pos, &mut report) else {
        panic!("chunk (0, 0) is fully generated");
    };
    assert!(report.unknown_blocks.is_empty(), "{report}");
    assert!(report.unknown_biomes.is_empty(), "{report}");
    assert!(report.invalid_block_states.is_empty(), "{report}");
    assert_eq!(report.chunks_without_light, 0);
    assert_eq!(persistent.inhabited_time, 1234, "fixture InhabitedTime");

    let loaded = ChunkStorage::try_persistent_to_chunk(
        &persistent,
        pos,
        ChunkStatus::Full,
        OVERWORLD_MIN_Y,
        OVERWORLD_HEIGHT,
        Weak::new(),
    )
    .expect("converted chunk should pass Steel's validation");
    let chunk = &loaded.chunk;

    let get = |x, y, z| chunk.get_block_state(BlockPos::new(x, y, z));
    assert_eq!(
        get(0, 100, 0),
        state(
            "chest",
            &[
                ("facing", "north"),
                ("type", "single"),
                ("waterlogged", "false")
            ]
        )
    );
    assert_eq!(
        get(2, 100, 0),
        state("furnace", &[("facing", "east"), ("lit", "false")]),
        "the furnace burned out before the save"
    );
    assert_eq!(
        get(3, 100, 0),
        state(
            "oak_stairs",
            &[
                ("facing", "south"),
                ("half", "top"),
                ("shape", "straight"),
                ("waterlogged", "true"),
            ]
        )
    );
    assert_eq!(
        get(0, -60, 0),
        REGISTRY
            .blocks
            .get_default_state_id(&vanilla_blocks::DEEPSLATE_DIAMOND_ORE)
    );
    assert_eq!(
        get(0, 300, 0),
        REGISTRY
            .blocks
            .get_default_state_id(&vanilla_blocks::GLOWSTONE)
    );
    assert_eq!(
        get(0, -64, 0),
        REGISTRY
            .blocks
            .get_default_state_id(&vanilla_blocks::BEDROCK)
    );

    let full = FullChunkRef::from_full_context(chunk);
    let chest = full
        .get_block_entity(BlockPos::new(0, 100, 0))
        .expect("chest block entity should load");
    let chest_nbt = chest.save_without_metadata();
    let items = chest_nbt
        .list("Items")
        .and_then(|items| items.compounds())
        .expect("chest should save its items");
    assert_eq!(items.len(), 2);
    assert!(full.get_block_entity(BlockPos::new(1, 100, 0)).is_some());
    assert!(full.get_block_entity(BlockPos::new(2, 100, 0)).is_some());

    // Glowstone at y=300 lights its section; vanilla saved that light.
    let glowstone_light_section = (300i32.div_euclid(16) - (OVERWORLD_MIN_Y / 16 - 1)) as u32;
    assert!(
        persistent
            .light
            .block
            .iter()
            .any(|section| section.section_index() == glowstone_light_section)
    );

    assert!(
        persistent
            .biomes
            .contains(&Identifier::vanilla_static("lush_caves"))
    );
    assert!(
        persistent
            .biomes
            .contains(&Identifier::vanilla_static("ocean"))
    );
}

#[test]
fn vanilla_entities_keep_their_base_data() {
    init_globals();
    let mut report = ImportReport::default();
    let ChunkConversion::Imported(persistent) = convert_fixture(ChunkPos::new(0, 0), &mut report)
    else {
        panic!("chunk (0, 0) is fully generated");
    };
    assert!(report.unknown_entities.is_empty(), "{report}");
    assert_eq!(report.entities, persistent.entities.len());

    let cow = persistent
        .entities
        .iter()
        .find(|entity| entity.entity_type == Identifier::vanilla_static("cow"))
        .expect("the cow should be imported");
    assert_eq!(cow.pos, [0.5, 101.0, 2.5]);
    assert_eq!(cow.tags, vec!["imported".to_owned()]);
    let name = ChunkStorage::custom_name_from_persistent(
        &cow.custom_name_nbt,
        uuid::Uuid::from_bytes(cow.uuid),
    )
    .expect("the cow keeps its name");
    assert_eq!(name.to_plain(&DisplayResolutor), "Бурёнка");

    let payload =
        read_compound(&mut Cursor::new(&cow.nbt_data[..])).expect("entity payload should be NBT");
    let payload = simdnbt::borrow::NbtCompound::from(&payload);
    assert_eq!(payload.byte("NoAI"), Some(1));
    assert!(payload.get("Pos").is_none(), "base fields are typed fields");
}

#[test]
fn vanilla_poi_tickets_are_imported() {
    init_globals();
    let mut report = ImportReport::default();
    let ChunkConversion::Imported(persistent) = convert_fixture(ChunkPos::new(0, 0), &mut report)
    else {
        panic!("chunk (0, 0) is fully generated");
    };
    let mut pois: Vec<_> = persistent
        .pois
        .iter()
        .map(|poi| (poi.x, poi.y, poi.z, poi.free_tickets))
        .collect();
    pois.sort_unstable();
    assert_eq!(pois, vec![(5, 100, 1, 32), (6, 100, 0, 1), (7, 100, 1, 1)]);
}

#[test]
fn chunks_that_are_not_full_are_skipped() {
    init_globals();
    let mut report = ImportReport::default();
    let ChunkConversion::NotFull(status) = convert_fixture(ChunkPos::new(0, 3), &mut report) else {
        panic!("chunk (0, 3) is a proto chunk");
    };
    assert_eq!(status, "minecraft:initialize_light");
}

#[test]
fn chunks_from_another_data_version_are_rejected() {
    init_globals();
    let mut root = NbtCompound::new();
    root.insert("DataVersion", 3955);
    root.insert("Status", "minecraft:full");
    let mut bytes = Vec::new();
    BaseNbt::new("", root).write(&mut bytes);

    let mut report = ImportReport::default();
    let Err(error) = convert_chunk(
        &bytes,
        None,
        None,
        ChunkPos::new(0, 0),
        OVERWORLD_MIN_Y,
        OVERWORLD_HEIGHT,
        &mut report,
    ) else {
        panic!("an old chunk must not be imported without a DataFixer");
    };
    assert!(error.contains("3955"), "{error}");
}

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

fn test_directory(name: &str) -> PathBuf {
    let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "steel-anvil-import-{name}-{}-{sequence}",
        process::id()
    ))
}

#[tokio::test]
async fn imported_dimension_loads_from_steel_region_files() {
    init_globals();
    let target = test_directory("dimension");
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("test thread pool should build"),
    );
    let dimension = DimensionImport {
        key: OVERWORLD.key.clone(),
        source: fixture_dir(),
        target: target.clone(),
        min_y: OVERWORLD_MIN_Y,
        height: OVERWORLD_HEIGHT,
    };

    let report = import_dimension(&dimension, Arc::clone(&pool))
        .await
        .expect("fixture dimension should import");
    assert_eq!(report.regions, 1);
    assert_eq!(report.chunks_imported, 1);
    assert_eq!(
        report.skipped_statuses.get("minecraft:initialize_light"),
        Some(&1)
    );
    assert!(report.failed_chunks.is_empty(), "{report}");

    let manager = RegionManager::new(&target);
    let pos = ChunkPos::new(0, 0);
    assert!(
        manager
            .acquire_chunk(pos)
            .await
            .expect("region should open")
    );
    let loaded = manager
        .load_chunk(pos, OVERWORLD_MIN_Y, OVERWORLD_HEIGHT, Weak::new(), &pool)
        .await
        .expect("imported chunk should load")
        .expect("imported chunk should exist");
    assert_eq!(loaded.status, ChunkStatus::Full);
    assert_eq!(
        loaded.chunk.get_block_state(BlockPos::new(0, 300, 0)),
        REGISTRY
            .blocks
            .get_default_state_id(&vanilla_blocks::GLOWSTONE)
    );
    assert!(
        !manager
            .chunk_exists(ChunkPos::new(0, 3))
            .await
            .expect("header should read"),
        "proto chunks are left for Steel to generate"
    );
    manager
        .release_chunk(pos)
        .await
        .expect("region should release");

    let again = import_dimension(&dimension, pool).await;
    assert!(
        matches!(again, Err(AnvilImportError::TargetNotEmpty { .. })),
        "importing twice must not mix worlds"
    );
    let _ = fs::remove_dir_all(&target);
}

#[tokio::test]
async fn vanilla_level_data_loads_as_steel_level_toml() {
    use steel_registry::{RegistryExt, vanilla_game_rules};
    use steel_utils::types::Difficulty;

    use crate::level_data::{GameTimeSource, LevelDataManager, WorldGenerationSettings};

    init_globals();
    let world_dir = test_directory("level");
    let dimension = DimensionImport {
        key: OVERWORLD.key.clone(),
        source: fixture_dir(),
        target: world_dir.join("region"),
        min_y: OVERWORLD_MIN_Y,
        height: OVERWORLD_HEIGHT,
    };
    let vanilla_world =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/anvil_import/fixtures/level");

    let report = import_level_data(&vanilla_world, slice::from_ref(&dimension))
        .expect("fixture level data should import");
    assert_eq!(report.seed, 12345);
    assert!(report.unknown_game_rules.is_empty(), "{report}");
    assert!(report.invalid_game_rules.is_empty(), "{report}");
    assert!(report.unknown_clocks.is_empty(), "{report}");

    let generation = WorldGenerationSettings::from_generator_config(
        Identifier::vanilla_static("overworld"),
        &toml::Value::Table(toml::Table::new()),
        OVERWORLD.key.clone(),
        OVERWORLD_MIN_Y,
        OVERWORLD_HEIGHT,
    );
    let manager = LevelDataManager::new(
        Some(&world_dir),
        0,
        Difficulty::Hard,
        generation,
        GameTimeSource::Primary,
    )
    .await
    .expect("Steel should load the imported level.toml");
    let data = manager.data();
    assert_eq!(manager.seed(), 12345, "the saved seed wins over the config");
    assert_eq!(manager.game_time_handle().ticks(), 864);
    assert_eq!(data.spawn_pos(), BlockPos::new(96, 145, -32));
    assert_eq!(
        data.respawn_data_or_local(&OVERWORLD.key).pos(),
        BlockPos::new(96, 145, -32)
    );
    assert_eq!(data.difficulty, Difficulty::Easy);
    assert!(data.initialized);
    assert_eq!(data.weather.rain_time, 87024);
    assert_eq!(data.weather.thunder_time, 155_929);
    assert!((data.world_border.size - 59_999_968.0).abs() < f64::EPSILON);
    assert_eq!(
        data.game_rules_values
            .get(&vanilla_game_rules::RANDOM_TICK_SPEED, &REGISTRY.game_rules),
        3
    );
    assert!(
        data.game_rules_values
            .get(&vanilla_game_rules::SPAWN_MOBS, &REGISTRY.game_rules)
    );
    let overworld_clock = REGISTRY
        .world_clocks
        .by_key(&Identifier::vanilla_static("overworld"))
        .expect("overworld clock is registered");
    assert_eq!(data.world_clocks.total_ticks(overworld_clock), Some(864));

    let again = import_level_data(&vanilla_world, slice::from_ref(&dimension));
    assert!(
        matches!(again, Err(AnvilImportError::LevelDataExists { .. })),
        "an existing level.toml is never overwritten"
    );
    let _ = fs::remove_dir_all(&world_dir);
}

#[tokio::test]
async fn vanilla_players_load_from_steel_player_storage() {
    use steel_registry::stat::vanilla_stat_types;

    use crate::player::player_data_storage::PlayerDataStorage;

    init_globals();
    let save_path = test_directory("players");
    let vanilla_world =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/anvil_import/fixtures/level");
    let uuid = uuid::Uuid::parse_str("36532b5e-c442-3dbb-a24c-c7e55d0f979a")
        .expect("fixture player UUID is valid");

    let report = import_players(&vanilla_world, &save_path, "minecraft")
        .await
        .expect("fixture players should import");
    assert_eq!(report.players, 1, "{report}");
    assert!(report.failed_players.is_empty(), "{report}");
    assert!(report.unknown_items.is_empty(), "{report}");
    assert!(report.unknown_stats.is_empty(), "{report}");
    assert!(report.unknown_advancements.is_empty(), "{report}");
    assert_eq!(report.dropped_effects.get("minecraft:speed"), Some(&1));

    let storage = PlayerDataStorage::on_disk(save_path.clone())
        .await
        .expect("storage should open");
    let data = storage
        .load_domain("minecraft", uuid)
        .await
        .expect("player data should load")
        .expect("imported player should exist");
    assert_eq!(data.pos, [8.5, 120.0, 8.5]);
    assert_eq!(data.rotation, [90.0, 10.0]);
    assert_eq!(data.world, "minecraft:overworld");
    assert!((data.health - 19.0).abs() < f32::EPSILON);
    assert_eq!(data.experience_level, 5);
    assert_eq!(data.experience_total, 7);

    assert_imported_items(&data);

    let respawn = data.respawn_config.as_ref().expect("respawn point is set");
    assert_eq!(respawn.respawn_data.pos(), BlockPos::new(10, 100, 10));
    assert!(respawn.forced);

    let play_time = data
        .stats
        .iter()
        .find(|stat| {
            stat.stat.stat_type_key() == &vanilla_stat_types::CUSTOM.key
                && stat.stat.stat_value_key() == &Identifier::vanilla_static("play_time")
        })
        .map(|stat| stat.count);
    assert_eq!(play_time, Some(218));
    assert!(
        data.advancements.iter().any(|(id, progress)| id
            == &Identifier::vanilla_static("story/root")
            && progress.has_progress()),
        "granted advancements stay done"
    );
    assert_eq!(
        storage
            .load_global(uuid)
            .await
            .expect("global data should load")
            .map(|global| global.last_active_domain),
        Some("minecraft".to_owned())
    );

    let again = import_players(&vanilla_world, &save_path, "minecraft").await;
    assert!(
        matches!(again, Err(AnvilImportError::PlayerDataExists { .. })),
        "existing Steel players are never overwritten"
    );
    let _ = fs::remove_dir_all(&save_path);
}

fn assert_imported_items(data: &PersistentPlayerData) {
    use steel_registry::vanilla_items;

    let slot = |slots: &[PersistentSlot], index: i8| {
        slots
            .iter()
            .find(|slot| slot.slot == index)
            .map(|slot| (slot.item.item.key.clone(), slot.item.count))
    };
    assert_eq!(
        slot(&data.inventory, 0),
        Some((vanilla_items::DIAMOND_SWORD.key.clone(), 1))
    );
    assert_eq!(
        slot(&data.inventory, 1),
        Some((vanilla_items::COBBLESTONE.key.clone(), 40))
    );
    assert_eq!(
        slot(&data.inventory, 39),
        Some((vanilla_items::IRON_HELMET.key.clone(), 1)),
        "the helmet moves from equipment to the head slot"
    );
    assert_eq!(
        slot(&data.inventory, 40),
        Some((vanilla_items::SHIELD.key.clone(), 1)),
        "the offhand item moves to the offhand slot"
    );
    assert_eq!(
        slot(&data.ender_items, 5),
        Some((vanilla_items::EMERALD.key.clone(), 12))
    );
    let sword = data
        .inventory
        .iter()
        .find(|slot| slot.slot == 0)
        .expect("sword slot");
    assert!(
        !sword.item.patch().is_empty(),
        "the sword keeps its components"
    );
}
