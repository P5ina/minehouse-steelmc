//! Conversion of one vanilla chunk (`SerializableChunkData`) into a Steel `PersistentChunk`.

use std::time::{SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashMap;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::blocks::BlockRef;
use steel_registry::vanilla_blocks::AIR;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::{BlockStateId, ChunkPos, Identifier, PackedChunkPos, version::DATA_VERSION};

use super::nbt::{self, ceil_log2, list_elements, unpack_bit_storage};
use super::report::ImportReport;
use super::{entities, poi};
use crate::chunk::heightmap::HeightmapType;
use crate::chunk::light::DATA_LAYER_SIZE;
use crate::chunk_saver::bit_pack::{bits_for_palette_len, pack_indices_from_iter};
use crate::chunk_saver::{
    PersistentBiomeData, PersistentBlockEntity, PersistentBlockState, PersistentChunk,
    PersistentHeightmap, PersistentLightData, PersistentLightSection, PersistentSection,
    PersistentStructureReference, PersistentTick,
};

const BLOCKS_PER_SECTION: usize = 4096;
const BIOMES_PER_SECTION: usize = 64;
/// Vanilla `PalettedContainer` strategy minimum for block states on disk.
const MIN_BLOCK_BITS: u32 = 4;
const FULL_STATUS: &str = "minecraft:full";

/// Vanilla heightmap names and the Steel heightmap they become.
const HEIGHTMAPS: [(&str, HeightmapType); 6] = [
    ("WORLD_SURFACE", HeightmapType::WorldSurface),
    ("MOTION_BLOCKING", HeightmapType::MotionBlocking),
    (
        "MOTION_BLOCKING_NO_LEAVES",
        HeightmapType::MotionBlockingNoLeaves,
    ),
    ("OCEAN_FLOOR", HeightmapType::OceanFloor),
    ("WORLD_SURFACE_WG", HeightmapType::WorldSurfaceWg),
    ("OCEAN_FLOOR_WG", HeightmapType::OceanFloorWg),
];

/// Block entity keys that `BlockEntity.saveWithFullMetadata` adds around the payload.
const BLOCK_ENTITY_METADATA_KEYS: [&str; 5] = ["id", "x", "y", "z", "keepPacked"];

/// Result of converting a chunk.
pub enum ChunkConversion {
    /// The chunk was fully generated and is ready to be written.
    Imported(Box<PersistentChunk<'static>>),
    /// The chunk has this vanilla status and will be generated again by Steel.
    NotFull(String),
}

/// Converts the NBT of a vanilla chunk, its entity chunk and its POI chunk.
///
/// Returns an error for data Steel cannot read faithfully, such as another data version or a
/// malformed palette. Replacements for unknown content are recorded in `report`.
pub fn convert_chunk(
    chunk_bytes: &[u8],
    entity_bytes: Option<&[u8]>,
    poi_bytes: Option<&[u8]>,
    pos: ChunkPos,
    min_y: i32,
    height: i32,
    report: &mut ImportReport,
) -> Result<ChunkConversion, String> {
    let root = nbt::read_root(chunk_bytes)?;
    check_data_version(&root)?;

    let status = nbt::string(&root, "Status").unwrap_or_default();
    if status != FULL_STATUS {
        return Ok(ChunkConversion::NotFull(status));
    }

    let x = root.int("xPos");
    let z = root.int("zPos");
    if x != Some(pos.0.x) || z != Some(pos.0.y) {
        return Err(format!(
            "chunk NBT says it is at [{x:?}, {z:?}], but it is stored at [{}, {}]",
            pos.0.x, pos.0.y
        ));
    }
    if root.int("yPos") != Some(min_y.div_euclid(16)) {
        return Err(format!(
            "chunk starts at section {:?}, but the dimension starts at section {}",
            root.int("yPos"),
            min_y.div_euclid(16)
        ));
    }

    let mut converter = ChunkConverter::new(pos, min_y, height);
    let (sections, light) = converter.sections(&root, report)?;
    if !nbt::boolean(&root, "isLightOn") {
        report.chunks_without_light += 1;
    }

    let block_entities = converter.block_entities(&root, report);
    let block_ticks = converter.ticks(&root, "block_ticks", report);
    let fluid_ticks = converter.ticks(&root, "fluid_ticks", report);
    let heightmaps = converter.heightmaps(&root)?;
    let postprocessing = converter.postprocessing(&root);
    let structure_references = structure_references(&root, report);

    let entities = match entity_bytes {
        Some(bytes) => entities::convert_entity_chunk(bytes, pos, report)?,
        None => Vec::new(),
    };
    let poi_records = match poi_bytes {
        Some(bytes) => poi::convert_poi_chunk(bytes, pos, min_y, height, report)?,
        None => Vec::new(),
    };

    Ok(ChunkConversion::Imported(Box::new(PersistentChunk {
        last_modified: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32),
        block_states: converter.block_states,
        biomes: converter.biomes,
        sections,
        block_entities,
        entities,
        block_ticks,
        fluid_ticks,
        heightmaps,
        light,
        postprocessing,
        structure_starts: Vec::new(),
        structure_references,
        pois: poi_records,
    })))
}

pub(super) fn check_data_version(root: &NbtCompound) -> Result<(), String> {
    match root.int("DataVersion") {
        Some(DATA_VERSION) => Ok(()),
        Some(other) => Err(format!(
            "saved with data version {other}, Steel reads {DATA_VERSION}; upgrade the world with \
             the matching vanilla server first (--forceUpgrade)"
        )),
        None => Err("missing DataVersion".to_owned()),
    }
}

/// Builds the chunk-level palettes while sections are converted.
struct ChunkConverter {
    pos: ChunkPos,
    min_y: i32,
    height: i32,
    block_states: Vec<PersistentBlockState<'static>>,
    block_indices: FxHashMap<BlockStateId, u16>,
    biomes: Vec<Identifier>,
    biome_indices: FxHashMap<Identifier, u16>,
}

impl ChunkConverter {
    fn new(pos: ChunkPos, min_y: i32, height: i32) -> Self {
        Self {
            pos,
            min_y,
            height,
            block_states: Vec::new(),
            block_indices: FxHashMap::default(),
            biomes: Vec::new(),
            biome_indices: FxHashMap::default(),
        }
    }

    const fn min_section(&self) -> i32 {
        self.min_y.div_euclid(16)
    }

    const fn section_count(&self) -> usize {
        (self.height / 16) as usize
    }

    fn sections(
        &mut self,
        root: &NbtCompound,
        report: &mut ImportReport,
    ) -> Result<(Vec<PersistentSection>, PersistentLightData), String> {
        let section_count = self.section_count();
        let mut by_index: Vec<Option<&NbtCompound>> = vec![None; section_count];
        let mut light = PersistentLightData::default();
        let section_list = root.list("sections").and_then(|list| list.compounds());

        for section in section_list.unwrap_or_default() {
            let Some(y) = section.byte("Y") else {
                return Err("section without Y".to_owned());
            };
            let y = i32::from(y);
            // Light sections extend one section below and above the build height.
            let light_index = y - (self.min_section() - 1);
            if (0..section_count as i32 + 2).contains(&light_index) {
                for (key, layer) in [
                    ("BlockLight", &mut light.block),
                    ("SkyLight", &mut light.sky),
                ] {
                    let Some(data) = section.byte_array(key) else {
                        continue;
                    };
                    if data.len() != DATA_LAYER_SIZE {
                        return Err(format!(
                            "section {y} {key} has {} bytes, expected {DATA_LAYER_SIZE}",
                            data.len()
                        ));
                    }
                    layer.push(PersistentLightSection::Initialized {
                        section_index: light_index as u32,
                        data: data.to_vec(),
                    });
                }
            }

            let index = y - self.min_section();
            if let Ok(index) = usize::try_from(index)
                && index < section_count
            {
                by_index[index] = Some(section);
            }
        }

        let sections = by_index
            .into_iter()
            .enumerate()
            .map(|(index, section)| self.section(section, index, report))
            .collect::<Result<_, _>>()?;
        Ok((sections, light))
    }

    fn section(
        &mut self,
        section: Option<&NbtCompound>,
        index: usize,
        report: &mut ImportReport,
    ) -> Result<PersistentSection, String> {
        let y = self.min_section() + index as i32;
        let biomes = match section.and_then(|section| section.compound("biomes")) {
            Some(biomes) => self
                .biome_container(biomes, report)
                .map_err(|error| format!("section {y} biomes: {error}"))?,
            // Vanilla fills a missing container with plains.
            None => PersistentBiomeData::Homogeneous {
                biome: self.biome_index(Identifier::vanilla_static("plains")),
            },
        };

        let Some(states) = section.and_then(|section| section.compound("block_states")) else {
            // Vanilla fills a missing container with air.
            let air = self.block_index(&AIR, REGISTRY.blocks.get_default_state_id(&AIR));
            return Ok(PersistentSection::Homogeneous {
                block_state: air,
                biomes,
            });
        };

        let palette = states.list("palette").map_or_default(list_elements);
        let palette = palette
            .iter()
            .map(|entry| self.block_palette_entry(entry, report))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("section {y} block states: {error}"))?;

        let values = Self::container_values(
            &palette,
            states.long_array("data"),
            BLOCKS_PER_SECTION,
            MIN_BLOCK_BITS,
        )
        .map_err(|error| format!("section {y} block states: {error}"))?;

        Ok(match local_palette(&values) {
            LocalPalette::Single(block_state) => PersistentSection::Homogeneous {
                block_state,
                biomes,
            },
            LocalPalette::Packed {
                palette,
                bits,
                data,
            } => PersistentSection::Heterogeneous {
                palette,
                bits_per_entry: bits,
                block_data: data,
                biomes,
            },
        })
    }

    fn biome_container(
        &mut self,
        biomes: &NbtCompound,
        report: &mut ImportReport,
    ) -> Result<PersistentBiomeData, String> {
        let palette = biomes
            .list("palette")
            .map_or_default(list_elements)
            .iter()
            .map(|entry| {
                let name = nbt::tag_string(entry).ok_or("palette entry is not a string")?;
                Ok(self.biome_palette_entry(&name, report))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let values =
            Self::container_values(&palette, biomes.long_array("data"), BIOMES_PER_SECTION, 0)?;
        Ok(match local_palette(&values) {
            LocalPalette::Single(biome) => PersistentBiomeData::Homogeneous { biome },
            LocalPalette::Packed {
                palette,
                bits,
                data,
            } => PersistentBiomeData::Heterogeneous {
                palette,
                bits_per_entry: bits,
                biome_data: data,
            },
        })
    }

    /// Expands a vanilla paletted container into chunk-level palette indices.
    fn container_values(
        palette: &[u16],
        data: Option<&[i64]>,
        count: usize,
        min_bits: u32,
    ) -> Result<Vec<u16>, String> {
        let Some(&first) = palette.first() else {
            return Err("empty palette".to_owned());
        };
        let Some(data) = data else {
            if palette.len() == 1 {
                return Ok(vec![first; count]);
            }
            return Err(format!("palette has {} entries but no data", palette.len()));
        };
        let bits = ceil_log2(palette.len()).max(min_bits).max(1);
        unpack_bit_storage(data, bits, count)?
            .into_iter()
            .map(|index| {
                palette.get(index as usize).copied().ok_or_else(|| {
                    format!("data references palette entry {index} of {}", palette.len())
                })
            })
            .collect()
    }

    fn block_palette_entry(
        &mut self,
        entry: &NbtTag,
        report: &mut ImportReport,
    ) -> Result<u16, String> {
        let (name, properties) = match entry {
            NbtTag::String(name) => (name.to_str().into_owned(), Vec::new()),
            NbtTag::Compound(compound) => {
                let name = nbt::string(compound, "id").ok_or("palette entry without id")?;
                let properties = compound
                    .compound("properties")
                    .map(|properties| {
                        properties
                            .iter()
                            .map(|(key, value)| {
                                let value = nbt::tag_string(value)
                                    .ok_or("property value is not a string")?;
                                Ok((key.to_str().into_owned(), value))
                            })
                            .collect::<Result<Vec<_>, &str>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                (name, properties)
            }
            _ => return Err("palette entry is neither a string nor a compound".to_owned()),
        };

        let Some(block) = nbt::identifier(&name).and_then(|key| REGISTRY.blocks.by_key(&key))
        else {
            ImportReport::count(&mut report.unknown_blocks, name);
            return Ok(self.block_index(&AIR, REGISTRY.blocks.get_default_state_id(&AIR)));
        };
        let state = REGISTRY.blocks.state_id_from_block_defaulted_properties(
            block,
            properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        let state = state.unwrap_or_else(|| {
            ImportReport::count(&mut report.invalid_block_states, name);
            REGISTRY.blocks.get_default_state_id(block)
        });
        Ok(self.block_index(block, state))
    }

    fn block_index(&mut self, block: BlockRef, state: BlockStateId) -> u16 {
        if let Some(&index) = self.block_indices.get(&state) {
            return index;
        }
        let index = self.block_states.len() as u16;
        self.block_states.push(PersistentBlockState {
            name: block.key.clone(),
            properties: REGISTRY.blocks.properties(state).collect(),
        });
        self.block_indices.insert(state, index);
        index
    }

    fn biome_palette_entry(&mut self, name: &str, report: &mut ImportReport) -> u16 {
        let key = nbt::identifier(name).filter(|key| REGISTRY.biomes.id_from_key(key).is_some());
        let key = key.unwrap_or_else(|| {
            ImportReport::count(&mut report.unknown_biomes, name.to_owned());
            Identifier::vanilla_static("plains")
        });
        self.biome_index(key)
    }

    fn biome_index(&mut self, key: Identifier) -> u16 {
        if let Some(&index) = self.biome_indices.get(&key) {
            return index;
        }
        let index = self.biomes.len() as u16;
        self.biomes.push(key.clone());
        self.biome_indices.insert(key, index);
        index
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        x.div_euclid(16) == self.pos.0.x
            && z.div_euclid(16) == self.pos.0.y
            && (self.min_y..self.min_y + self.height).contains(&y)
    }

    fn block_entities(
        &self,
        root: &NbtCompound,
        report: &mut ImportReport,
    ) -> Vec<PersistentBlockEntity> {
        let list = root
            .list("block_entities")
            .and_then(|list| list.compounds());
        let mut block_entities = Vec::new();
        for compound in list.unwrap_or_default() {
            let (Some(id), Some(x), Some(y), Some(z)) = (
                nbt::string(compound, "id"),
                compound.int("x"),
                compound.int("y"),
                compound.int("z"),
            ) else {
                ImportReport::count(
                    &mut report.dropped_records,
                    "block entity without id or position",
                );
                continue;
            };
            if !self.contains(x, y, z) {
                ImportReport::count(
                    &mut report.dropped_records,
                    "block entity outside its chunk",
                );
                continue;
            }
            // `DUMMY` marks a block entity that must be created from the block when loaded.
            let entity_type = if id == "DUMMY" {
                None
            } else {
                let Some(key) = nbt::identifier(&id)
                    .filter(|key| REGISTRY.block_entity_types.by_key(key).is_some())
                else {
                    ImportReport::count(&mut report.unknown_block_entities, id);
                    continue;
                };
                Some(key)
            };
            let payload = if entity_type.is_some() {
                nbt::compound_bytes(&nbt::without_keys(compound, &BLOCK_ENTITY_METADATA_KEYS))
            } else {
                Vec::new()
            };
            report.block_entities += 1;
            block_entities.push(PersistentBlockEntity {
                x: x.rem_euclid(16) as u8,
                y: y as i16,
                z: z.rem_euclid(16) as u8,
                entity_type,
                nbt_data: payload,
            });
        }
        block_entities
    }

    fn ticks(
        &self,
        root: &NbtCompound,
        key: &str,
        report: &mut ImportReport,
    ) -> Vec<PersistentTick> {
        let list = root.list(key).and_then(|list| list.compounds());
        let mut ticks = Vec::new();
        for tick in list.unwrap_or_default() {
            let (Some(id), Some(x), Some(y), Some(z), Some(delay), Some(priority)) = (
                nbt::string(tick, "i").and_then(|id| nbt::identifier(&id)),
                tick.int("x"),
                tick.int("y"),
                tick.int("z"),
                tick.int("t"),
                tick.int("p"),
            ) else {
                ImportReport::count(&mut report.dropped_records, "malformed scheduled tick");
                continue;
            };
            if !self.contains(x, y, z) {
                ImportReport::count(
                    &mut report.dropped_records,
                    "scheduled tick outside its chunk",
                );
                continue;
            }
            ticks.push(PersistentTick {
                x: x.rem_euclid(16) as u8,
                y: y as i16,
                z: z.rem_euclid(16) as u8,
                delay,
                priority: priority as i8,
                tick_type: id,
            });
        }
        ticks
    }

    fn heightmaps(&self, root: &NbtCompound) -> Result<Vec<PersistentHeightmap>, String> {
        let Some(heightmaps) = root.compound("Heightmaps") else {
            return Ok(Vec::new());
        };
        let bits = ceil_log2(self.height as usize + 1);
        let mut converted = Vec::new();
        for (name, heightmap_type) in HEIGHTMAPS {
            let Some(data) = heightmaps.long_array(name) else {
                continue;
            };
            let values = unpack_bit_storage(data, bits, 256)
                .map_err(|error| format!("heightmap {name}: {error}"))?;
            if let Some(value) = values.iter().find(|&&value| value > self.height as u32) {
                return Err(format!("heightmap {name} has out-of-range value {value}"));
            }
            converted.push(PersistentHeightmap {
                heightmap_type: heightmap_type.persistence_id(),
                data: values.into_iter().map(|value| value as u16).collect(),
            });
        }
        Ok(converted)
    }

    fn postprocessing(&self, root: &NbtCompound) -> Vec<Vec<u16>> {
        let Some(lists) = root.list("PostProcessing").and_then(|list| list.lists()) else {
            return Vec::new();
        };
        lists
            .iter()
            .take(self.section_count())
            .map(|list| {
                list.shorts()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|offset| offset as u16)
                    .collect()
            })
            .collect()
    }
}

fn structure_references(
    root: &NbtCompound,
    report: &mut ImportReport,
) -> Vec<PersistentStructureReference> {
    let Some(structures) = root.compound("structures") else {
        return Vec::new();
    };
    if let Some(starts) = structures.compound("starts") {
        for (structure, start) in starts.iter() {
            let is_start = start
                .compound()
                .and_then(|start| nbt::string(start, "id"))
                .is_some_and(|id| id != "INVALID");
            if is_start {
                ImportReport::count(
                    &mut report.structure_starts_dropped,
                    structure.to_str().into_owned(),
                );
            }
        }
    }
    let Some(references) = structures.compound("References") else {
        return Vec::new();
    };
    references
        .iter()
        .filter_map(|(structure, positions)| {
            let structure = nbt::identifier(&structure.to_str())?;
            let references = positions
                .long_array()?
                .iter()
                .map(|&packed| PackedChunkPos::from_raw(packed))
                .collect();
            Some(PersistentStructureReference {
                structure,
                references,
            })
        })
        .collect()
}

enum LocalPalette {
    Single(u16),
    Packed {
        palette: Vec<u16>,
        bits: u8,
        data: Box<[u64]>,
    },
}

/// Builds a section-local palette over chunk-level indices in first-use order.
fn local_palette(values: &[u16]) -> LocalPalette {
    let mut palette = Vec::new();
    let mut lookup = FxHashMap::default();
    let local: Vec<u32> = values
        .iter()
        .map(|&value| {
            *lookup.entry(value).or_insert_with(|| {
                palette.push(value);
                (palette.len() - 1) as u32
            })
        })
        .collect();
    let Some(bits) = bits_for_palette_len(palette.len()) else {
        return LocalPalette::Single(palette[0]);
    };
    LocalPalette::Packed {
        palette,
        bits,
        data: pack_indices_from_iter(local.into_iter(), bits),
    }
}
