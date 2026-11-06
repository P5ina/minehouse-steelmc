//! Reader for vanilla Anvil region files (`r.<x>.<z>.mca`).
//!
//! A region file starts with a 4 KiB table of 1024 big-endian location entries
//! (3-byte sector offset + 1-byte sector count) followed by a 4 KiB timestamp table.
//! Each chunk payload is `length: u32 BE`, `compression: u8`, then `length - 1` bytes.
//! When bit 7 of the compression byte is set, the payload lives in an external
//! `c.<x>.<z>.mcc` file next to the region file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::{fs, io};

use flate2::read::{GzDecoder, ZlibDecoder};
use steel_utils::ChunkPos;

use super::AnvilImportError;

const SECTOR_SIZE: usize = 4096;
const HEADER_SIZE: usize = SECTOR_SIZE * 2;
const CHUNKS_PER_REGION: usize = 1024;
const EXTERNAL_FLAG: u8 = 0x80;

const COMPRESSION_GZIP: u8 = 1;
const COMPRESSION_ZLIB: u8 = 2;
const COMPRESSION_NONE: u8 = 3;
const COMPRESSION_LZ4: u8 = 4;
const COMPRESSION_CUSTOM: u8 = 127;

/// One region file loaded fully into memory.
pub struct AnvilRegion {
    path: PathBuf,
    region_x: i32,
    region_z: i32,
    data: Vec<u8>,
}

impl AnvilRegion {
    /// Reads a region file. The region coordinates come from the file name.
    pub fn open(path: &Path) -> Result<Self, AnvilImportError> {
        let (region_x, region_z) =
            parse_region_file_name(path).ok_or_else(|| AnvilImportError::InvalidFile {
                path: path.to_path_buf(),
                reason: "file name is not r.<x>.<z>.mca".to_owned(),
            })?;
        let data = fs::read(path).map_err(|source| AnvilImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            region_x,
            region_z,
            data,
        })
    }

    /// Region X coordinate.
    #[must_use]
    pub const fn region_x(&self) -> i32 {
        self.region_x
    }

    /// Region Z coordinate.
    #[must_use]
    pub const fn region_z(&self) -> i32 {
        self.region_z
    }

    /// Chunk positions that have a payload in this region.
    #[must_use]
    pub fn chunk_positions(&self) -> Vec<ChunkPos> {
        if self.data.len() < HEADER_SIZE {
            return Vec::new();
        }
        (0..CHUNKS_PER_REGION)
            .filter(|&index| self.location(index) != (0, 0))
            .map(|index| {
                ChunkPos::new(
                    self.region_x * 32 + (index % 32) as i32,
                    self.region_z * 32 + (index / 32) as i32,
                )
            })
            .collect()
    }

    fn location(&self, index: usize) -> (usize, usize) {
        let entry = &self.data[index * 4..index * 4 + 4];
        let offset =
            (usize::from(entry[0]) << 16) | (usize::from(entry[1]) << 8) | usize::from(entry[2]);
        (offset, usize::from(entry[3]))
    }

    /// Returns the decompressed NBT bytes of a chunk, or `None` if it has no payload.
    pub fn read_chunk(&self, pos: ChunkPos) -> Result<Option<Vec<u8>>, AnvilImportError> {
        if self.data.len() < HEADER_SIZE {
            return Ok(None);
        }
        let local_x = pos.0.x.rem_euclid(32) as usize;
        let local_z = pos.0.y.rem_euclid(32) as usize;
        let (sector, sector_count) = self.location(local_x + local_z * 32);
        if sector == 0 && sector_count == 0 {
            return Ok(None);
        }

        let start = sector * SECTOR_SIZE;
        let Some(header) = self.data.get(start..start + 5) else {
            return Err(self.invalid(pos, "chunk offset points past the end of the file"));
        };
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if length == 0 {
            return Err(self.invalid(pos, "chunk payload length is zero"));
        }
        let compression = header[4];

        if compression & EXTERNAL_FLAG != 0 {
            let external = self
                .path
                .with_file_name(format!("c.{}.{}.mcc", pos.0.x, pos.0.y));
            let compressed = fs::read(&external).map_err(|source| AnvilImportError::Io {
                path: external,
                source,
            })?;
            return self
                .decompress(pos, compression & !EXTERNAL_FLAG, &compressed)
                .map(Some);
        }

        let Some(payload) = self.data.get(start + 5..start + 4 + length) else {
            return Err(self.invalid(pos, "chunk payload runs past the end of the file"));
        };
        self.decompress(pos, compression, payload).map(Some)
    }

    fn decompress(
        &self,
        pos: ChunkPos,
        compression: u8,
        payload: &[u8],
    ) -> Result<Vec<u8>, AnvilImportError> {
        let mut out = Vec::new();
        let result = match compression {
            COMPRESSION_GZIP => GzDecoder::new(payload).read_to_end(&mut out),
            COMPRESSION_ZLIB => ZlibDecoder::new(payload).read_to_end(&mut out),
            COMPRESSION_NONE => {
                out.extend_from_slice(payload);
                Ok(out.len())
            }
            COMPRESSION_LZ4 => {
                return Err(self.invalid(
                    pos,
                    "LZ4 region compression is not supported; set region-file-compression=deflate \
                     in server.properties, then load and save the world with vanilla once",
                ));
            }
            COMPRESSION_CUSTOM => {
                return Err(self.invalid(pos, "custom region compression is not supported"));
            }
            other => {
                return Err(self.invalid(pos, &format!("unknown compression type {other}")));
            }
        };
        result.map_err(|source: io::Error| AnvilImportError::InvalidFile {
            path: self.path.clone(),
            reason: format!("chunk {pos:?} failed to decompress: {source}"),
        })?;
        Ok(out)
    }

    fn invalid(&self, pos: ChunkPos, reason: &str) -> AnvilImportError {
        AnvilImportError::InvalidFile {
            path: self.path.clone(),
            reason: format!("chunk {pos:?}: {reason}"),
        }
    }
}

/// Parses `r.<x>.<z>.mca` into region coordinates.
#[must_use]
pub fn parse_region_file_name(path: &Path) -> Option<(i32, i32)> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("r.")?.strip_suffix(".mca")?;
    let (x, z) = rest.split_once('.')?;
    Some((x.parse().ok()?, z.parse().ok()?))
}

/// Lists region files in a directory, sorted by coordinates. A missing directory is empty.
pub fn list_region_files(dir: &Path) -> Result<Vec<PathBuf>, AnvilImportError> {
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
        let entry = entry.map_err(|source| AnvilImportError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if let Some(coords) = parse_region_file_name(&path) {
            files.push((coords, path));
        }
    }
    files.sort_by_key(|(coords, _)| *coords);
    Ok(files.into_iter().map(|(_, path)| path).collect())
}
