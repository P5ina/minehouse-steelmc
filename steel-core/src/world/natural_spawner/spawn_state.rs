//! Vanilla `NaturalSpawner.SpawnState`, `LocalMobCapCalculator` and `PotentialCalculator`.

use std::ptr;

use rustc_hash::FxHashMap;
use steel_registry::entity_type::{EntityTypeRef, MobCategory};
use steel_utils::{BlockPos, ChunkPos, SectionPos};
use uuid::Uuid;

use rayon::iter::{
    IndexedParallelIterator as _, IntoParallelRefIterator as _, ParallelIterator as _,
};

use crate::chunk::gameplay_chunk_lookup_cache::GameplayChunkLookupCacheScope;
use crate::entity::{Entity as _, SharedEntity};
use crate::world::World;
use crate::world::tick_pool::{TICK_POOL_MIN_SPLIT, install_on_tick_pool};

/// Vanilla `NaturalSpawner.MAGIC_NUMBER`: the 17×17 chunk area a cap is defined for.
const MAGIC_NUMBER: i32 = 17 * 17;

/// Vanilla `NaturalSpawner.SPAWN_DISTANCE_BLOCK`, squared.
const SPAWN_DISTANCE_SQR: f64 = 128.0 * 128.0;

/// Vanilla `PotentialCalculator`: point charges of already spawned mobs.
#[derive(Debug, Default, Clone)]
pub(crate) struct PotentialCalculator {
    charges: Vec<(BlockPos, f64)>,
}

impl PotentialCalculator {
    pub(crate) fn add_charge(&mut self, pos: BlockPos, charge: f64) {
        if charge != 0.0 {
            self.charges.push((pos, charge));
        }
    }

    /// Vanilla `getPotentialEnergyChange`: `charge * Σ qᵢ / dᵢ`.
    #[must_use]
    pub(crate) fn potential_energy_change(&self, pos: BlockPos, charge: f64) -> f64 {
        if charge == 0.0 {
            return 0.0;
        }
        let potential_change: f64 = self
            .charges
            .iter()
            .map(|(point, point_charge)| {
                let dist_sqr = dist_sqr(*point, pos);
                if dist_sqr == 0.0 {
                    f64::INFINITY
                } else {
                    point_charge / dist_sqr.sqrt()
                }
            })
            .sum();
        potential_change * charge
    }
}

/// Vanilla `Vec3i.distSqr`.
fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let dx = f64::from(a.x() - b.x());
    let dy = f64::from(a.y() - b.y());
    let dz = f64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}

/// Vanilla `LocalMobCapCalculator`: per-player mob counts for the chunks each
/// player is close enough to spawn in.
#[derive(Debug, Default, Clone)]
pub(crate) struct LocalMobCapCalculator {
    players_near_chunk: FxHashMap<ChunkPos, Vec<Uuid>>,
    player_mob_counts: FxHashMap<Uuid, FxHashMap<MobCategory, i32>>,
}

impl LocalMobCapCalculator {
    /// Vanilla `ChunkMap.getPlayersCloseForSpawning`: non-spectators within
    /// 128 blocks (horizontal) of the chunk center.
    fn players_near<'a>(
        players_near_chunk: &'a mut FxHashMap<ChunkPos, Vec<Uuid>>,
        world: &World,
        pos: ChunkPos,
    ) -> &'a [Uuid] {
        players_near_chunk.entry(pos).or_insert_with(|| {
            let center_x = f64::from(pos.0.x * 16 + 8);
            let center_z = f64::from(pos.0.y * 16 + 8);
            let mut players = Vec::new();
            world.players.iter_players(|uuid, player| {
                if !player.is_spectator() {
                    // Other islands' players come from the pre-phase snapshot: their live
                    // positions change under their own island's tick.
                    let position = world.player_position_in_tick(player);
                    let dx = center_x - position.x;
                    let dz = center_z - position.z;
                    if dx * dx + dz * dz < SPAWN_DISTANCE_SQR {
                        players.push(*uuid);
                    }
                }
                true
            });
            players
        })
    }

    pub(crate) fn add_mob(&mut self, world: &World, pos: ChunkPos, category: MobCategory) {
        let players = Self::players_near(&mut self.players_near_chunk, world, pos);
        for player in players {
            *self
                .player_mob_counts
                .entry(*player)
                .or_default()
                .entry(category)
                .or_insert(0) += 1;
        }
    }

    /// True when any nearby player is still under its local cap.
    pub(crate) fn can_spawn(
        &mut self,
        world: &World,
        category: MobCategory,
        pos: ChunkPos,
    ) -> bool {
        let players = Self::players_near(&mut self.players_near_chunk, world, pos);
        players.iter().any(|player| {
            self.player_mob_counts.get(player).is_none_or(|counts| {
                counts.get(&category).copied().unwrap_or(0) < category.max_instances_per_chunk()
            })
        })
    }
}

/// What one entity adds to `createState`, read without touching the shared state.
struct MobContribution {
    pos: BlockPos,
    chunk_pos: ChunkPos,
    category: MobCategory,
    charge: Option<f64>,
    is_mob: bool,
}

impl MobContribution {
    /// `None` when `createState` skips the entity: persistent mobs, `Misc`
    /// entities, and entities outside a loaded full chunk.
    fn of(world: &World, entity: &SharedEntity) -> Option<Self> {
        let mob = entity.as_mob();
        if mob.is_some_and(|mob| mob.is_persistence_required() || mob.requires_custom_persistence())
        {
            return None;
        }
        let entity_type = entity.entity_type();
        let category = entity_type.mob_category;
        if category == MobCategory::Misc {
            return None;
        }
        let pos = entity.block_position();
        let chunk_pos = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        world.chunk_map.with_full_chunk(chunk_pos, |_| ())?;
        Some(Self {
            pos,
            chunk_pos,
            category,
            charge: spawn_charge(world, entity_type, pos),
            is_mob: mob.is_some(),
        })
    }
}

/// Vanilla `NaturalSpawner.SpawnState`.
#[derive(Debug, Clone)]
pub(crate) struct SpawnState {
    spawnable_chunk_count: i32,
    pub(super) mob_category_counts: FxHashMap<MobCategory, i32>,
    pub(super) spawn_potential: PotentialCalculator,
    pub(super) local_mob_cap_calculator: LocalMobCapCalculator,
    last_checked: Option<(BlockPos, EntityTypeRef, f64)>,
}

impl SpawnState {
    /// Vanilla `NaturalSpawner.createState`: counts every non-persistent mob in
    /// a loaded full chunk, feeding the global caps, local caps and charges.
    pub(crate) fn create(world: &World, spawnable_chunk_count: usize) -> Self {
        let entities = world.entity_manager.get_accessible_entities();
        // The per-entity lookups (owning chunk, biome charge) dominate on busy worlds and only
        // read the world, so they run on the tick pool. Rayon's `collect` keeps entity order,
        // so charges are pushed, and later summed, in the same order as a sequential pass.
        let contributions: Vec<MobContribution> = install_on_tick_pool(|| {
            entities
                .par_iter()
                .with_min_len(TICK_POOL_MIN_SPLIT)
                .map_init(
                    || GameplayChunkLookupCacheScope::enter(&world.chunk_map),
                    |_, entity| MobContribution::of(world, entity),
                )
                .flatten_iter()
                .collect()
        });

        let mut state = Self::empty(i32::try_from(spawnable_chunk_count).unwrap_or(i32::MAX));
        for contribution in contributions {
            if let Some(charge) = contribution.charge {
                state.spawn_potential.add_charge(contribution.pos, charge);
            }
            if contribution.is_mob {
                state.local_mob_cap_calculator.add_mob(
                    world,
                    contribution.chunk_pos,
                    contribution.category,
                );
            }
            *state
                .mob_category_counts
                .entry(contribution.category)
                .or_insert(0) += 1;
        }
        state
    }

    pub(crate) fn empty(spawnable_chunk_count: i32) -> Self {
        Self {
            spawnable_chunk_count,
            mob_category_counts: FxHashMap::default(),
            spawn_potential: PotentialCalculator::default(),
            local_mob_cap_calculator: LocalMobCapCalculator::default(),
            last_checked: None,
        }
    }

    #[must_use]
    pub(crate) fn mob_count(&self, category: MobCategory) -> i32 {
        self.mob_category_counts
            .get(&category)
            .copied()
            .unwrap_or(0)
    }

    /// Vanilla `canSpawnForCategoryGlobal`.
    #[must_use]
    pub(crate) fn can_spawn_for_category_global(&self, category: MobCategory) -> bool {
        let max_mob_count =
            category.max_instances_per_chunk() * self.spawnable_chunk_count / MAGIC_NUMBER;
        self.mob_count(category) < max_mob_count
    }

    /// Vanilla `canSpawnForCategoryLocal`.
    pub(crate) fn can_spawn_for_category_local(
        &mut self,
        world: &World,
        category: MobCategory,
        chunk_pos: ChunkPos,
    ) -> bool {
        self.local_mob_cap_calculator
            .can_spawn(world, category, chunk_pos)
    }

    /// Vanilla `SpawnState.canSpawn`: the biome's energy budget for this type.
    pub(crate) fn can_spawn(
        &mut self,
        world: &World,
        entity_type: EntityTypeRef,
        pos: BlockPos,
    ) -> bool {
        let cost = world
            .biome_at(pos)
            .and_then(|biome| biome.spawn_costs.get(&entity_type.key))
            .map(|cost| (cost.charge, cost.energy_budget));
        let Some((charge, energy_budget)) = cost else {
            self.last_checked = Some((pos, entity_type, 0.0));
            return true;
        };
        self.last_checked = Some((pos, entity_type, charge));
        self.spawn_potential.potential_energy_change(pos, charge) <= energy_budget
    }

    /// Vanilla `SpawnState.afterSpawn`.
    pub(crate) fn after_spawn(&mut self, world: &World, entity_type: EntityTypeRef, pos: BlockPos) {
        let charge = match self.last_checked {
            Some((checked_pos, checked_type, charge))
                if checked_pos == pos && ptr::eq(checked_type, entity_type) =>
            {
                charge
            }
            _ => spawn_charge(world, entity_type, pos).unwrap_or(0.0),
        };
        self.spawn_potential.add_charge(pos, charge);
        let category = entity_type.mob_category;
        *self.mob_category_counts.entry(category).or_insert(0) += 1;
        let chunk_pos = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        self.local_mob_cap_calculator
            .add_mob(world, chunk_pos, category);
    }
}

/// The biome `spawn_costs` charge of `entity_type` at `pos`, if it has one.
fn spawn_charge(world: &World, entity_type: EntityTypeRef, pos: BlockPos) -> Option<f64> {
    world
        .biome_at(pos)
        .and_then(|biome| biome.spawn_costs.get(&entity_type.key))
        .map(|cost| cost.charge)
}
