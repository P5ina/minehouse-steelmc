//! Summary of an Anvil import.

use std::collections::BTreeMap;
use std::fmt;

use steel_utils::ChunkPos;

/// What an import converted, skipped and replaced.
///
/// Maps are keyed by vanilla id so replacements can be reviewed before the world is used.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Region files written.
    pub regions: usize,
    /// Chunks written to Steel region files.
    pub chunks_imported: usize,
    /// Chunks skipped because they were not fully generated, by vanilla status.
    pub skipped_statuses: BTreeMap<String, usize>,
    /// Chunks that could not be read or converted, with the reason.
    pub failed_chunks: Vec<(ChunkPos, String)>,
    /// Block entities imported.
    pub block_entities: usize,
    /// Entities imported, passengers included.
    pub entities: usize,
    /// Points of interest whose tickets were imported.
    pub pois: usize,
    /// Unknown block ids replaced with air, with the number of palette entries.
    pub unknown_blocks: BTreeMap<String, usize>,
    /// Block states with invalid properties replaced with the block's default state.
    pub invalid_block_states: BTreeMap<String, usize>,
    /// Unknown biome ids replaced with `minecraft:plains`.
    pub unknown_biomes: BTreeMap<String, usize>,
    /// Block entities dropped because Steel does not know their type.
    pub unknown_block_entities: BTreeMap<String, usize>,
    /// Entities dropped because Steel does not know their type, passengers included.
    pub unknown_entities: BTreeMap<String, usize>,
    /// Structure starts that were not converted, by structure id.
    pub structure_starts_dropped: BTreeMap<String, usize>,
    /// Records that were malformed and dropped (ticks, block entities, entities outside the
    /// chunk or with missing fields), by kind.
    pub dropped_records: BTreeMap<&'static str, usize>,
    /// Full chunks whose light was not marked correct by vanilla (`isLightOn` false).
    pub chunks_without_light: usize,
}

impl ImportReport {
    /// Adds another report's counts to this one.
    pub fn merge(&mut self, other: Self) {
        self.regions += other.regions;
        self.chunks_imported += other.chunks_imported;
        merge_counts(&mut self.skipped_statuses, other.skipped_statuses);
        self.failed_chunks.extend(other.failed_chunks);
        self.block_entities += other.block_entities;
        self.entities += other.entities;
        self.pois += other.pois;
        merge_counts(&mut self.unknown_blocks, other.unknown_blocks);
        merge_counts(&mut self.invalid_block_states, other.invalid_block_states);
        merge_counts(&mut self.unknown_biomes, other.unknown_biomes);
        merge_counts(
            &mut self.unknown_block_entities,
            other.unknown_block_entities,
        );
        merge_counts(&mut self.unknown_entities, other.unknown_entities);
        merge_counts(
            &mut self.structure_starts_dropped,
            other.structure_starts_dropped,
        );
        merge_counts(&mut self.dropped_records, other.dropped_records);
        self.chunks_without_light += other.chunks_without_light;
    }

    pub(super) fn count<K: Ord>(map: &mut BTreeMap<K, usize>, key: K) {
        *map.entry(key).or_default() += 1;
    }
}

fn merge_counts<K: Ord>(into: &mut BTreeMap<K, usize>, from: BTreeMap<K, usize>) {
    for (key, count) in from {
        *into.entry(key).or_default() += count;
    }
}

fn write_counts<K: fmt::Display>(
    f: &mut fmt::Formatter<'_>,
    title: &str,
    map: &BTreeMap<K, usize>,
) -> fmt::Result {
    if map.is_empty() {
        return Ok(());
    }
    writeln!(f, "{title}:")?;
    for (key, count) in map {
        writeln!(f, "  {key}: {count}")?;
    }
    Ok(())
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "regions: {}, chunks: {}, block entities: {}, entities: {}, POIs: {}",
            self.regions, self.chunks_imported, self.block_entities, self.entities, self.pois
        )?;
        write_counts(
            f,
            "Skipped chunks that are not fully generated",
            &self.skipped_statuses,
        )?;
        write_counts(f, "Unknown blocks replaced with air", &self.unknown_blocks)?;
        write_counts(
            f,
            "Block states with invalid properties reset to the default state",
            &self.invalid_block_states,
        )?;
        write_counts(
            f,
            "Unknown biomes replaced with plains",
            &self.unknown_biomes,
        )?;
        write_counts(
            f,
            "Unknown block entities dropped",
            &self.unknown_block_entities,
        )?;
        write_counts(f, "Unknown entities dropped", &self.unknown_entities)?;
        write_counts(
            f,
            "Structure starts not converted",
            &self.structure_starts_dropped,
        )?;
        write_counts(f, "Malformed records dropped", &self.dropped_records)?;
        if self.chunks_without_light > 0 {
            writeln!(
                f,
                "Chunks without correct light: {}",
                self.chunks_without_light
            )?;
        }
        if !self.failed_chunks.is_empty() {
            writeln!(f, "Failed chunks:")?;
            for (pos, reason) in &self.failed_chunks {
                writeln!(f, "  [{}, {}]: {reason}", pos.0.x, pos.0.y)?;
            }
        }
        Ok(())
    }
}
