//! Weighted natural spawn lists: biome spawners, structure overrides and the fortress list.

use std::ptr;

use rand::{Rng, RngExt as _};
use steel_registry::biome::SpawnerData;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityTypeRef, MobCategory};
use steel_registry::structure::{StructureSpawnBoundingBox, StructureSpawnerData};
use steel_registry::{REGISTRY, RegistryExt, vanilla_blocks, vanilla_entities};
use steel_utils::{BlockPos, ChunkPos, Identifier, SectionPos};

use crate::chunk::status::ChunkStatus;
use crate::world::World;

/// Vanilla `MobSpawnSettings.SpawnerData` with its list weight.
///
/// Equality follows the vanilla record (type and count), not the weight, which
/// belongs to the surrounding `Weighted` entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpawnerEntry {
    pub(crate) entity_type: EntityTypeRef,
    pub(crate) weight: i32,
    pub(crate) min_count: i32,
    pub(crate) max_count: i32,
}

impl PartialEq for SpawnerEntry {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.entity_type, other.entity_type)
            && self.min_count == other.min_count
            && self.max_count == other.max_count
    }
}

impl SpawnerEntry {
    /// Builds an entry the way vanilla's `SpawnerData` constructor does: types in
    /// the `MISC` category spawn as pigs. Unknown entity ids yield `None`.
    fn new(entity_type: &Identifier, weight: i32, min_count: i32, max_count: i32) -> Option<Self> {
        let entity_type = REGISTRY.entity_types.by_key(entity_type)?;
        let entity_type = if entity_type.mob_category == MobCategory::Misc {
            &vanilla_entities::PIG
        } else {
            entity_type
        };
        Some(Self {
            entity_type,
            weight,
            min_count,
            max_count,
        })
    }

    fn from_biome(data: &SpawnerData) -> Option<Self> {
        Self::new(
            &data.entity_type,
            data.weight,
            data.min_count,
            data.max_count,
        )
    }

    fn from_structure(data: &StructureSpawnerData) -> Option<Self> {
        Self::new(
            &data.entity_type,
            data.weight,
            data.min_count,
            data.max_count,
        )
    }

    /// Vanilla `count().sample(random)` for the uniform count provider.
    pub(crate) fn sample_count(&self, random: &mut dyn Rng) -> i32 {
        if self.max_count <= self.min_count {
            self.min_count
        } else {
            random.random_range(self.min_count..=self.max_count)
        }
    }
}

/// Vanilla `WeightedList<SpawnerData>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SpawnList {
    entries: Vec<SpawnerEntry>,
}

impl SpawnList {
    pub(crate) const fn new(entries: Vec<SpawnerEntry>) -> Self {
        Self { entries }
    }

    fn total_weight(&self) -> i32 {
        self.entries.iter().map(|entry| entry.weight.max(0)).sum()
    }

    /// Vanilla `WeightedList.getRandom`: one `nextInt(totalWeight)` roll, then
    /// the entry whose cumulative weight covers it.
    pub(crate) fn get_random(&self, random: &mut dyn Rng) -> Option<SpawnerEntry> {
        let total_weight = self.total_weight();
        if total_weight <= 0 {
            return None;
        }
        let mut selection = random.random_range(0..total_weight);
        for entry in &self.entries {
            let weight = entry.weight.max(0);
            if selection < weight {
                return Some(*entry);
            }
            selection -= weight;
        }
        None
    }

    /// Vanilla `WeightedList.contains`.
    pub(crate) fn contains(&self, entry: &SpawnerEntry) -> bool {
        self.entries.contains(entry)
    }
}

/// Vanilla `NetherFortressStructure.FORTRESS_ENEMIES`.
pub(crate) fn fortress_enemies() -> SpawnList {
    let entry = |entity_type: EntityTypeRef, weight, min_count, max_count| SpawnerEntry {
        entity_type,
        weight,
        min_count,
        max_count,
    };
    SpawnList::new(vec![
        entry(&vanilla_entities::BLAZE, 10, 2, 3),
        entry(&vanilla_entities::ZOMBIFIED_PIGLIN, 5, 4, 4),
        entry(&vanilla_entities::WITHER_SKELETON, 8, 5, 5),
        entry(&vanilla_entities::SKELETON, 2, 5, 5),
        entry(&vanilla_entities::MAGMA_CUBE, 3, 4, 4),
    ])
}

/// Vanilla `BuiltinStructures.FORTRESS`.
const FORTRESS: Identifier = Identifier::vanilla_static("fortress");

impl World {
    /// Vanilla `NaturalSpawner.mobsAt`.
    pub(crate) fn mobs_at(&self, category: MobCategory, pos: BlockPos) -> SpawnList {
        if self.is_in_nether_fortress_bounds(pos, category) {
            return fortress_enemies();
        }
        self.generator_mobs_at(category, pos)
    }

    /// Vanilla `NaturalSpawner.isInNetherFortressBounds`.
    pub(crate) fn is_in_nether_fortress_bounds(
        &self,
        pos: BlockPos,
        category: MobCategory,
    ) -> bool {
        category == MobCategory::Monster
            && self.get_block_state(pos.below()).get_block() == &vanilla_blocks::NETHER_BRICKS
            && self.structure_start_contains(&FORTRESS, pos, |start_box, _| start_box)
    }

    /// Vanilla `ChunkGenerator.getMobsAt`: the first structure override whose
    /// bounds contain `pos`, otherwise the biome's spawners for the category.
    fn generator_mobs_at(&self, category: MobCategory, pos: BlockPos) -> SpawnList {
        let references = self.structure_references_at(pos);
        for structure_key in references {
            let Some(structure) = REGISTRY.structures.by_key(&structure_key) else {
                continue;
            };
            let Some(spawn_override) = structure
                .spawn_overrides
                .iter()
                .find(|spawn_override| spawn_override.category == category.serialized_name())
            else {
                continue;
            };
            let piece_only = spawn_override.bounding_box == StructureSpawnBoundingBox::Piece;
            if self.structure_start_contains(&structure_key, pos, |start_box, piece_box| {
                if piece_only { piece_box } else { start_box }
            }) {
                return SpawnList::new(
                    spawn_override
                        .spawns
                        .iter()
                        .filter_map(SpawnerEntry::from_structure)
                        .collect(),
                );
            }
        }

        self.biome_at(pos)
            .and_then(|biome| biome.spawners.get(category.serialized_name()))
            .map_or_else(SpawnList::default, |spawners| {
                SpawnList::new(
                    spawners
                        .iter()
                        .filter_map(SpawnerEntry::from_biome)
                        .collect(),
                )
            })
    }

    /// Structure ids referenced by the chunk containing `pos`.
    fn structure_references_at(&self, pos: BlockPos) -> Vec<Identifier> {
        let chunk_pos = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        self.chunk_map
            .with_chunk_at_status(chunk_pos, ChunkStatus::StructureReferences, |chunk| {
                chunk
                    .structure_references()
                    .iter()
                    .filter(|(_, sources)| !sources.is_empty())
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Checks the valid starts of `structure` referenced by the chunk at `pos`.
    ///
    /// `pick` receives whether the start box and whether any piece box contain
    /// `pos`, and returns the containment the caller cares about. Vanilla loads
    /// each referenced start chunk to `STRUCTURE_STARTS`; Steel cannot load
    /// synchronously here, so starts in chunks that are not resident are skipped.
    fn structure_start_contains(
        &self,
        structure: &Identifier,
        pos: BlockPos,
        pick: impl Fn(bool, bool) -> bool,
    ) -> bool {
        let chunk_pos = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        let sources: Vec<ChunkPos> = self
            .chunk_map
            .with_chunk_at_status(chunk_pos, ChunkStatus::StructureReferences, |chunk| {
                chunk
                    .structure_references()
                    .get(structure)
                    .map_or_else(Vec::new, |sources| sources.iter().copied().collect())
            })
            .unwrap_or_default();

        sources.into_iter().any(|source| {
            self.chunk_map
                .with_chunk_at_status(source, ChunkStatus::StructureStarts, |chunk| {
                    chunk
                        .structure_starts()
                        .get(structure)
                        .is_some_and(|start| {
                            let start_box = start
                                .bounding_box
                                .is_some_and(|bounding_box| bounding_box.contains_blockpos(pos));
                            let piece_box = start
                                .pieces
                                .iter()
                                .any(|piece| piece.bounding_box.contains_blockpos(pos));
                            !start.pieces.is_empty() && pick(start_box, piece_box)
                        })
                })
                .unwrap_or(false)
        })
    }
}
