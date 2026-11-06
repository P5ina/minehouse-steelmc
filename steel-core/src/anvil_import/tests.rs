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
use simdnbt::owned::{BaseNbt, NbtCompound, NbtList};
use steel_utils::text::DisplayResolutor;

use glam::IVec3;
use steel_registry::template_pool::Projection;
use steel_registry::vanilla_dimension_types::OVERWORLD;
use steel_registry::{REGISTRY, vanilla_blocks};
use steel_utils::{BlockPos, ChunkPos, Identifier};
use steel_worldgen::structure::{StructurePiecePayload, TemplateMarkerHandling};

use super::*;
use crate::bootstrap::init_globals;
use crate::chunk::full_chunk::FullChunkRef;
use crate::chunk_saver::{
    ChunkStorage, PersistentBoundingBox, PersistentJigsawPieceData, PersistentPoolElement,
    PersistentProcessorList, PersistentStructurePiece, PersistentStructurePiecePayload,
    PersistentStructureStart, PersistentTemplatePieceData, PersistentTemplatePlacementAdjustment,
    PersistentTemplateProcessorList,
};
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

/// Reads a file of `fixtures/structures`: structure starts cut unchanged from the
/// `structures.starts` of their origin chunks in a seed 777 world saved by the vanilla 26.3
/// server, merged into one `starts` compound.
///
/// - `jigsaw.nbt`: a plains village, an ancient city and trial chambers.
/// - `templates.nbt`: a shipwreck, an igloo with a basement, a cold ocean ruin, a ruined portal,
///   a nether fossil, an end city and a woodland mansion.
fn structure_fixture(name: &str) -> NbtCompound {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/anvil_import/fixtures/structures")
        .join(name);
    level::read_gzip_root(&path).expect("structure fixture should read")
}

fn jigsaw_fixture() -> NbtCompound {
    structure_fixture("jigsaw.nbt")
}

fn template_data(piece: &PersistentStructurePiece) -> &PersistentTemplatePieceData {
    let PersistentStructurePiecePayload::Template(data) = &piece.payload else {
        panic!("{} is a template piece", piece.piece_type);
    };
    data
}

#[test]
fn vanilla_template_starts_are_converted() {
    init_globals();
    let mut report = ImportReport::default();
    let starts = structures::structure_starts(&structure_fixture("templates.nbt"), &mut report);
    assert!(report.structure_starts_dropped.is_empty(), "{report}");
    let piece_counts: Vec<_> = starts
        .iter()
        .map(|start| (start.structure.path.as_ref().to_owned(), start.pieces.len()))
        .collect();
    let expected = [
        ("shipwreck", 1),
        ("igloo", 12),
        ("ocean_ruin_cold", 15),
        ("ruined_portal", 1),
        ("nether_fossil", 1),
        ("end_city", 94),
        ("mansion", 559),
    ];
    assert_eq!(
        piece_counts,
        expected.map(|(structure, pieces)| (structure.to_owned(), pieces))
    );
    for piece in starts.iter().flat_map(|start| &start.pieces) {
        assert_eq!((piece.orientation, piece.projection), (2, -1));
        assert!(piece.junctions.is_empty());
    }

    assert_small_template_pieces(&starts);

    let end_city = template_data(&starts[5].pieces[0]);
    assert_eq!(
        end_city.template_id,
        Identifier::vanilla_static("end_city/base_floor")
    );
    assert_eq!((end_city.block_ignore, end_city.marker_handling), (1, 6));

    let mansion = &starts[6].pieces;
    let entrance = template_data(&mansion[0]);
    assert_eq!(
        entrance.template_id,
        Identifier::vanilla_static("woodland_mansion/entrance")
    );
    assert_eq!((entrance.rotation, entrance.marker_handling), (3, 7));
    assert!(
        mansion.iter().any(|piece| template_data(piece).mirror == 2),
        "mansion rooms keep their LEFT_RIGHT mirror"
    );
}

/// Shipwreck, igloo, ocean ruin, ruined portal and nether fossil of `templates.nbt`.
fn assert_small_template_pieces(starts: &[PersistentStructureStart]) {
    let shipwreck = &starts[0].pieces[0];
    assert_eq!(
        shipwreck.piece_type,
        Identifier::vanilla_static("shipwreck")
    );
    let data = template_data(shipwreck);
    assert_eq!(
        data.template_id,
        Identifier::vanilla_static("shipwreck/upsidedown_fronthalf_degraded")
    );
    assert_eq!(data.template_position, [12864, 90, 2208]);
    assert_eq!((data.rotation, data.rotation_pivot), (2, [4, 0, 15]));
    assert_eq!((data.block_ignore, data.marker_handling), (2, 2));
    assert!(matches!(
        data.placement_adjustment,
        PersistentTemplatePlacementAdjustment::Shipwreck {
            is_beached: false,
            height_adjusted: false
        }
    ));

    let igloo = &starts[1].pieces;
    // Vanilla saved the basement's box moved to the terrain by the placement, at y 33; loading
    // computes it again from the template position, as vanilla does.
    assert_eq!(
        (igloo[0].bounding_box.min_y, igloo[0].bounding_box.max_y),
        (54, 59)
    );
    let bottom = template_data(&igloo[0]);
    assert_eq!(
        bottom.template_id,
        Identifier::vanilla_static("igloo/bottom")
    );
    assert_eq!((bottom.rotation_pivot, bottom.post_process), ([3, 6, 7], 0));
    assert!(matches!(
        bottom.placement_adjustment,
        PersistentTemplatePlacementAdjustment::Igloo {
            template_offset: [0, -3, -2]
        }
    ));
    let top = template_data(igloo.last().expect("igloo has pieces"));
    assert_eq!(top.template_id, Identifier::vanilla_static("igloo/top"));
    assert_eq!(
        (top.post_process, top.liquid_settings, top.block_ignore),
        (2, 1, 1)
    );

    let ruin = template_data(&starts[2].pieces[0]);
    assert_eq!((ruin.late_block_ignore, ruin.marker_handling), (2, 5));
    assert!(matches!(
        ruin.processors,
        PersistentTemplateProcessorList::OceanRuin { biome_temp: 1, integrity } if (integrity - 0.9).abs() < f32::EPSILON
    ));

    let portal = template_data(&starts[3].pieces[0]);
    assert_eq!(
        portal.template_id,
        Identifier::vanilla_static("ruined_portal/portal_7")
    );
    assert_eq!((portal.mirror, portal.block_ignore), (0, 2));
    assert_eq!((portal.placement_clip, portal.post_process), (2, 3));
    assert!(matches!(
        portal.processors,
        PersistentTemplateProcessorList::RuinedPortal {
            vertical_placement: 0,
            cold: false,
            air_pocket: false,
            ..
        }
    ));

    let fossil = template_data(&starts[4].pieces[0]);
    assert_eq!((fossil.placement_clip, fossil.post_process), (1, 1));
}

#[test]
fn vanilla_jigsaw_starts_are_converted() {
    init_globals();
    let mut report = ImportReport::default();
    let starts = structures::structure_starts(&jigsaw_fixture(), &mut report);
    assert!(report.structure_starts_dropped.is_empty(), "{report}");
    let piece_counts: Vec<_> = starts
        .iter()
        .map(|start| (start.structure.to_string(), start.pieces.len()))
        .collect();
    assert_eq!(
        piece_counts,
        [
            ("minecraft:village_plains".to_owned(), 70),
            ("minecraft:ancient_city".to_owned(), 90),
            ("minecraft:trial_chambers".to_owned(), 220),
        ]
    );

    let village = &starts[0];
    assert_eq!((village.chunk_x, village.chunk_z), (636, 110));
    let center = &village.pieces[0];
    assert_eq!(
        center.bounding_box,
        PersistentBoundingBox {
            min_x: 10176,
            min_y: 67,
            min_z: 1760,
            max_x: 10186,
            max_y: 75,
            max_z: 1770,
        }
    );
    assert_eq!((center.gen_depth, center.orientation), (0, -1));
    assert_eq!((center.ground_level_delta, center.projection), (1, 0));
    assert_eq!(center.junctions.len(), 7);
    let junction = &center.junctions[2];
    assert_eq!(
        (
            junction.source_x,
            junction.source_ground_y,
            junction.source_z,
            junction.delta_y,
            junction.dest_projection
        ),
        (10175, 68, 1765, 0, 1)
    );
    let PersistentStructurePiecePayload::Jigsaw(data) = &center.payload else {
        panic!("village pieces are jigsaw pieces");
    };
    assert_eq!(data.position, [10176, 67, 1760]);
    assert_eq!((data.rotation, data.liquid_settings), (0, 0));
    assert!(matches!(
        &data.pool_element,
        PersistentPoolElement::LegacySingle {
            location,
            processors: PersistentProcessorList::Registry(processors),
            projection: 0,
        } if *location == Identifier::vanilla_static("village/plains/town_centers/plains_meeting_point_3")
            && *processors == Identifier::vanilla_static("mossify_70_percent")
    ));
    assert!(village.pieces.iter().any(|piece| matches!(
        &piece.payload,
        PersistentStructurePiecePayload::Jigsaw(PersistentJigsawPieceData {
            pool_element: PersistentPoolElement::Feature { feature, .. },
            ..
        }) if *feature == Identifier::vanilla_static("flower_plain")
    )));

    let PersistentStructurePiecePayload::Jigsaw(ice_box) = &starts[1].pieces[30].payload else {
        panic!("ancient city pieces are jigsaw pieces");
    };
    assert_eq!(
        (ice_box.position, ice_box.rotation),
        ([13206, -51, 1892], 1)
    );
    assert!(matches!(
        &ice_box.pool_element,
        PersistentPoolElement::List { elements, projection: 0 } if matches!(
            elements.as_slice(),
            [PersistentPoolElement::Single { processors: PersistentProcessorList::Empty, .. }]
        )
    ));

    let PersistentStructurePiecePayload::Jigsaw(chamber) = &starts[2].pieces[0].payload else {
        panic!("trial chamber pieces are jigsaw pieces");
    };
    assert_eq!((chamber.rotation, chamber.liquid_settings), (2, 1));
}

#[test]
fn starts_with_pieces_steel_cannot_place_are_dropped_whole() {
    init_globals();
    let mut fixture = jigsaw_fixture();
    let starts = fixture.compound_mut("starts").expect("fixture has starts");
    let village = starts
        .compound_mut("minecraft:village_plains")
        .expect("fixture has a village");
    let element = village
        .list_mut("Children")
        .and_then(|children| match children {
            NbtList::Compound(children) => children.last_mut(),
            _ => None,
        })
        .and_then(|piece| piece.compound_mut("pool_element"))
        .expect("village pieces have pool elements");
    let _ = element.remove("location");
    element.insert("location", "mod:houses/hut");
    let chambers = starts
        .compound_mut("minecraft:trial_chambers")
        .expect("fixture has trial chambers");
    let _ = chambers.remove("id");
    chambers.insert("id", "INVALID");

    let mut report = ImportReport::default();
    let converted = structures::structure_starts(&fixture, &mut report);
    let kept: Vec<_> = converted
        .iter()
        .map(|start| start.structure.to_string())
        .collect();
    assert_eq!(kept, ["minecraft:ancient_city"]);
    assert_eq!(
        report
            .structure_starts_dropped
            .get("minecraft:village_plains"),
        Some(&1)
    );
    assert_eq!(
        report.structure_starts_dropped.len(),
        1,
        "invalid starts are not counted"
    );
}

#[tokio::test]
async fn imported_structure_starts_load_in_steel() {
    init_globals();
    let target = test_directory("structures");
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("test thread pool should build"),
    );
    let mut report = ImportReport::default();
    let pos = ChunkPos::new(0, 0);
    let ChunkConversion::Imported(mut persistent) = convert_fixture(pos, &mut report) else {
        panic!("fixture chunk (0, 0) is full");
    };
    persistent.structure_starts = structures::structure_starts(&jigsaw_fixture(), &mut report);
    persistent
        .structure_starts
        .extend(structures::structure_starts(
            &structure_fixture("templates.nbt"),
            &mut report,
        ));

    let manager = RegionManager::new(&target);
    manager
        .save_chunk_data(
            PreparedChunkSave {
                pos,
                status: ChunkStatus::Full,
                persistent: *persistent,
                handled_runtime_entity_ids: Vec::new(),
            },
            &pool,
        )
        .await
        .expect("chunk should save");
    manager.close_all().await.expect("region should close");

    let manager = RegionManager::new(&target);
    assert!(
        manager
            .acquire_chunk(pos)
            .await
            .expect("region should open")
    );
    let loaded = manager
        .load_chunk(pos, OVERWORLD_MIN_Y, OVERWORLD_HEIGHT, Weak::new(), &pool)
        .await
        .expect("chunk should load")
        .expect("chunk should exist");
    {
        let starts = loaded.chunk.structure_starts();
        let village = starts
            .get(&Identifier::vanilla_static("village_plains"))
            .expect("village start should load");
        assert_eq!(village.pieces.len(), 70);
        assert_eq!(village.chunk_pos, ChunkPos::new(636, 110));
        let StructurePiecePayload::Jigsaw(data) = &village.pieces[0].payload else {
            panic!("village pieces are jigsaw pieces");
        };
        assert_eq!(data.position, IVec3::new(10176, 67, 1760));
        assert_eq!(village.pieces[0].projection, Some(Projection::Rigid));
        let mansion = starts
            .get(&Identifier::vanilla_static("mansion"))
            .expect("mansion start should load");
        assert_eq!(mansion.pieces.len(), 559);
        assert!(matches!(
            &mansion.pieces[0].payload,
            StructurePiecePayload::Template(data)
                if data.marker_handling == TemplateMarkerHandling::WoodlandMansion
        ));
        assert_eq!(starts.len(), 10);
    }
    manager
        .release_chunk(pos)
        .await
        .expect("region should release");
    let _ = fs::remove_dir_all(&target);
}
