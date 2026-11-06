//! Import of vanilla Anvil worlds into Steel's region format.
//!
//! The importer reads a world saved by vanilla Minecraft of the same version Steel targets
//! (`steel_utils::version::DATA_VERSION`) and writes Steel `.srg` region files. Steel has no
//! `DataFixer`, so worlds from older versions must be upgraded by vanilla first
//! (`java -jar server.jar --forceUpgrade`).
//!
//! What is converted per dimension:
//! - `region/`: block states, biomes, heightmaps, light, block entities, scheduled block and
//!   fluid ticks, postprocessing marks and structure references of `minecraft:full` chunks.
//! - `entities/`: entities with their passengers.
//! - `poi/`: free tickets of points of interest.
//!
//! Chunks that are not `minecraft:full` are skipped; Steel generates them again from the seed.
//! Structure starts are converted when Steel can represent every piece of them; the others are
//! dropped and counted in the report. Only jigsaw structures are converted so far.
//!
//! Content Steel does not know (blocks, biomes, block entities or entities from mods) cannot be
//! represented in Steel's typed format. Unknown blocks become air, unknown biomes become
//! `minecraft:plains`, and unknown block entities and entities are dropped. Every replacement is
//! counted by id in the [`ImportReport`], so an import can be checked before it is used.

mod chunk;
mod entities;
mod level;
mod nbt;
mod players;
mod poi;
pub mod region;
mod report;
mod structures;

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::spawn_blocking;

use futures::future::try_join_all;
use steel_utils::{ChunkPos, Identifier};

use crate::chunk::status::ChunkStatus;
use crate::chunk_saver::{PersistentChunk, PreparedChunkSave, RegionManager};

pub use chunk::{ChunkConversion, convert_chunk};
pub use level::{LevelReport, import_level_data};
pub use players::{PlayerReport, import_players};
pub use region::AnvilRegion;
pub use report::ImportReport;

/// Errors that stop an import.
#[derive(Debug, thiserror::Error)]
pub enum AnvilImportError {
    /// Reading or writing a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// A region or `.dat` file is malformed.
    #[error("{}: {reason}", path.display())]
    InvalidFile {
        /// File that failed to parse.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// The Steel target already contains region files.
    #[error("{} already contains Steel region files; import into an empty world", path.display())]
    TargetNotEmpty {
        /// Target region directory.
        path: PathBuf,
    },
    /// The Steel world already has level data.
    #[error("{} already exists; import into an empty world", path.display())]
    LevelDataExists {
        /// Existing `level.toml`.
        path: PathBuf,
    },
    /// A player already has Steel data in the target domain.
    #[error("player {uuid} already has data in domain {domain}; import into an empty world")]
    PlayerDataExists {
        /// Player UUID.
        uuid: uuid::Uuid,
        /// Target domain.
        domain: String,
    },
}

/// One vanilla dimension to import into one Steel world.
#[derive(Debug, Clone)]
pub struct DimensionImport {
    /// Dimension id, for example `minecraft:overworld`.
    pub key: Identifier,
    /// Vanilla dimension directory holding `region/`, `entities/` and `poi/`,
    /// for example `world/dimensions/minecraft/overworld`.
    pub source: PathBuf,
    /// Steel region directory, for example `saves/minecraft/worlds/overworld/region`.
    pub target: PathBuf,
    /// Minimum build height of the dimension type.
    pub min_y: i32,
    /// Build height of the dimension type.
    pub height: i32,
}

/// Imports one dimension.
///
/// Chunk conversion runs on `thread_pool` because a whole world is gigabytes of NBT.
pub async fn import_dimension(
    dimension: &DimensionImport,
    thread_pool: Arc<rayon::ThreadPool>,
) -> Result<ImportReport, AnvilImportError> {
    ensure_empty_target(&dimension.target)?;

    let regions = region::list_region_files(&dimension.source.join("region"))?;
    let manager = RegionManager::new(&dimension.target);
    let mut report = ImportReport::default();

    for region_path in regions {
        let task_dimension = dimension.clone();
        let task_path = region_path.clone();
        let pool = Arc::clone(&thread_pool);
        let (chunks, region_report) =
            spawn_blocking(move || convert_region(&task_dimension, &task_path, &pool))
                .await
                .map_err(|error| AnvilImportError::Io {
                    path: region_path,
                    source: io::Error::other(error.to_string()),
                })??;
        report.merge(region_report);

        let saves = chunks.into_iter().map(|(pos, persistent)| {
            let prepared = PreparedChunkSave {
                pos,
                status: ChunkStatus::Full,
                persistent,
                handled_runtime_entity_ids: Vec::new(),
            };
            manager.save_chunk_data(prepared, &thread_pool)
        });
        try_join_all(saves)
            .await
            .map_err(|source| AnvilImportError::Io {
                path: dimension.target.clone(),
                source,
            })?;
        report.regions += 1;
    }

    manager
        .close_all()
        .await
        .map_err(|source| AnvilImportError::Io {
            path: dimension.target.clone(),
            source,
        })?;
    Ok(report)
}

fn ensure_empty_target(target: &Path) -> Result<(), AnvilImportError> {
    let entries = match fs::read_dir(target) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AnvilImportError::Io {
                path: target.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "srg") {
            return Err(AnvilImportError::TargetNotEmpty {
                path: target.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Converts every chunk of one region file.
fn convert_region(
    dimension: &DimensionImport,
    region_path: &Path,
    thread_pool: &rayon::ThreadPool,
) -> Result<(Vec<(ChunkPos, PersistentChunk<'static>)>, ImportReport), AnvilImportError> {
    use rayon::prelude::*;

    let region = AnvilRegion::open(region_path)?;
    let file_name = region_path.file_name().map_or_default(PathBuf::from);
    let entities = open_optional_region(&dimension.source.join("entities").join(&file_name))?;
    let pois = open_optional_region(&dimension.source.join("poi").join(&file_name))?;

    let results: Vec<_> = thread_pool.install(|| {
        region
            .chunk_positions()
            .into_par_iter()
            .map(|pos| {
                let mut report = ImportReport::default();
                let chunk = convert_one(
                    &region,
                    entities.as_ref(),
                    pois.as_ref(),
                    pos,
                    dimension,
                    &mut report,
                );
                (pos, chunk, report)
            })
            .collect()
    });

    let mut chunks = Vec::new();
    let mut report = ImportReport::default();
    for (pos, chunk, chunk_report) in results {
        report.merge(chunk_report);
        match chunk {
            Ok(Some(persistent)) => chunks.push((pos, persistent)),
            Ok(None) => {}
            Err(reason) => report.failed_chunks.push((pos, reason)),
        }
    }
    Ok((chunks, report))
}

fn convert_one(
    region: &AnvilRegion,
    entities: Option<&AnvilRegion>,
    poi_region: Option<&AnvilRegion>,
    pos: ChunkPos,
    dimension: &DimensionImport,
    report: &mut ImportReport,
) -> Result<Option<PersistentChunk<'static>>, String> {
    let Some(chunk_bytes) = region.read_chunk(pos).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let entity_bytes = match entities {
        Some(entities) => entities
            .read_chunk(pos)
            .map_err(|error| error.to_string())?,
        None => None,
    };
    let poi_bytes = match poi_region {
        Some(region) => region.read_chunk(pos).map_err(|error| error.to_string())?,
        None => None,
    };
    match convert_chunk(
        &chunk_bytes,
        entity_bytes.as_deref(),
        poi_bytes.as_deref(),
        pos,
        dimension.min_y,
        dimension.height,
        report,
    )? {
        ChunkConversion::Imported(persistent) => {
            report.chunks_imported += 1;
            Ok(Some(*persistent))
        }
        ChunkConversion::NotFull(status) => {
            *report.skipped_statuses.entry(status).or_default() += 1;
            Ok(None)
        }
    }
}

fn open_optional_region(path: &Path) -> Result<Option<AnvilRegion>, AnvilImportError> {
    if path.exists() {
        AnvilRegion::open(path).map(Some)
    } else {
        Ok(None)
    }
}

/// The vanilla dimensions of a 26.x world save and the Steel worlds they import into.
///
/// Vanilla keeps each dimension in `dimensions/<namespace>/<name>`; Steel's default world
/// layout is `<save_path>/<domain>/worlds/<name>/region`.
#[must_use]
pub fn vanilla_dimensions(world: &Path, save_path: &Path, domain: &str) -> Vec<DimensionImport> {
    use steel_registry::vanilla_dimension_types::{OVERWORLD, THE_END, THE_NETHER};

    [&OVERWORLD, &THE_NETHER, &THE_END]
        .into_iter()
        .map(|dimension_type| {
            let name = dimension_type.key.path.as_ref();
            DimensionImport {
                key: dimension_type.key.clone(),
                source: world.join("dimensions").join("minecraft").join(name),
                target: save_path
                    .join(domain)
                    .join("worlds")
                    .join(name)
                    .join("region"),
                min_y: dimension_type.min_y,
                height: dimension_type.height,
            }
        })
        .collect()
}

/// Reads the world seed from `data/minecraft/world_gen_settings.dat`.
pub fn read_world_seed(world: &Path) -> Result<i64, AnvilImportError> {
    let path = world
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    let invalid = |reason: String| AnvilImportError::InvalidFile {
        path: path.clone(),
        reason,
    };
    let root = level::read_gzip_root(&path)?;
    chunk::check_data_version(&root).map_err(invalid)?;
    root.compound("data")
        .and_then(|data| data.long("seed"))
        .ok_or_else(|| invalid("missing data.seed".to_owned()))
}
