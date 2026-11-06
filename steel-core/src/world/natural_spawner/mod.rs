//! Vanilla `NaturalSpawner`: periodic mob spawning around players.

mod chunk_generation;
mod spawn_list;
mod spawn_state;
#[cfg(test)]
mod tests;

pub(crate) use chunk_generation::{set_decoration_seed, spawn_mobs_for_chunk_generation};

use std::sync::Arc;

use glam::DVec3;
use rand::seq::SliceRandom as _;
use rand::{Rng, RngExt as _};
use steel_math::DEGREE_360;
use steel_registry::entity_type::MobCategory;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_game_rules::{SPAWN_MOBS, SPAWN_MONSTERS};
use steel_utils::{BlockPos, ChunkPos, SectionPos};

use crate::chunk::heightmap::HeightmapType;
use crate::entity::{
    EntitySpawnReason, Mob, SpawnGroupData, SpawnPlacements, create_entity_instance, spawn_aabb,
};
use crate::level_data::RespawnData;
use crate::physics::{WorldCollisionProvider, has_collision};
use crate::world::{World, is_redstone_conductor};

pub(crate) use spawn_list::SpawnerEntry;
pub(crate) use spawn_state::SpawnState;

/// Vanilla `NaturalSpawner.MIN_SPAWN_DISTANCE`, squared.
const MIN_SPAWN_DISTANCE_SQR: f64 = 24.0 * 24.0;

/// Vanilla `NaturalSpawner.SPAWNING_CATEGORIES`: every category but `MISC`.
pub(crate) const SPAWNING_CATEGORIES: [MobCategory; 7] = [
    MobCategory::Monster,
    MobCategory::Creature,
    MobCategory::Ambient,
    MobCategory::Axolotls,
    MobCategory::UndergroundWaterCreature,
    MobCategory::WaterCreature,
    MobCategory::WaterAmbient,
];

/// Vanilla `NaturalSpawner.getFilteredSpawningCategories`.
#[must_use]
pub(crate) fn filtered_spawning_categories(
    state: &SpawnState,
    spawn_enemies: bool,
    spawn_persistent: bool,
) -> Vec<MobCategory> {
    SPAWNING_CATEGORIES
        .into_iter()
        .filter(|category| {
            (spawn_enemies || category.is_friendly())
                && (spawn_persistent || !category.is_persistent())
                && state.can_spawn_for_category_global(*category)
        })
        .collect()
}

impl World {
    /// Vanilla `ServerLevel.isSpawningMonsters`.
    #[must_use]
    pub(crate) fn is_spawning_monsters(&self) -> bool {
        self.get_game_rule(&SPAWN_MOBS) && self.get_game_rule(&SPAWN_MONSTERS)
    }

    /// Vanilla `ServerLevel.canSpawnEntitiesInChunk`.
    #[must_use]
    pub(crate) fn can_spawn_entities_in_chunk(&self, pos: ChunkPos) -> bool {
        let border = self.world_border_snapshot();
        let min_x = f64::from(pos.0.x << 4);
        let min_z = f64::from(pos.0.y << 4);
        self.chunk_map.is_entity_ticking_full_chunk_loaded(pos)
            && border.is_within_bounds_with_margin(min_x, min_z, 0.0)
            && border.is_within_bounds_with_margin(min_x + 15.0, min_z + 15.0, 0.0)
    }

    /// Vanilla `ServerLevel.getRespawnData`: the domain's shared respawn point,
    /// owned by the world that owns the domain's game time.
    #[must_use]
    pub(crate) fn domain_respawn_data(self: &Arc<Self>) -> RespawnData {
        let owner = self
            .domain_entity_directory
            .read()
            .as_ref()
            .and_then(|directory| directory.game_time_owner())
            .unwrap_or_else(|| Arc::clone(self));
        let level_data = owner.level_data.read();
        level_data.data().respawn_data_or_local(&owner.key)
    }

    /// Vanilla `ServerChunkCache.tickChunks` spawning half: builds the spawn
    /// state, then ticks each spawning chunk in shuffled order. `on_chunk` runs
    /// first for every chunk, matching `tickSpawningChunk`'s inhabited time step.
    pub(crate) fn tick_spawning_chunks(
        self: &Arc<Self>,
        spawning_chunks: &mut [ChunkPos],
        spawn_chunk_count: usize,
        mut on_chunk: impl FnMut(ChunkPos),
    ) {
        let mut random = rand::rng();
        let mut state = self
            .get_game_rule(&SPAWN_MOBS)
            .then(|| SpawnState::create(self, spawn_chunk_count));
        let spawning_categories = state.as_ref().map_or_else(Vec::new, |state| {
            let spawn_persistent = self.game_time() % 400 == 0;
            filtered_spawning_categories(state, self.is_spawning_monsters(), spawn_persistent)
        });

        spawning_chunks.shuffle(&mut random);
        for &chunk_pos in spawning_chunks.iter() {
            on_chunk(chunk_pos);
            if let Some(state) = state.as_mut()
                && !spawning_categories.is_empty()
                && self.can_spawn_entities_in_chunk(chunk_pos)
            {
                self.spawn_for_chunk(chunk_pos, state, &spawning_categories, &mut random);
            }
        }
    }

    /// Vanilla `NaturalSpawner.spawnForChunk`.
    pub(crate) fn spawn_for_chunk(
        self: &Arc<Self>,
        chunk_pos: ChunkPos,
        state: &mut SpawnState,
        spawning_categories: &[MobCategory],
        random: &mut dyn Rng,
    ) {
        for &category in spawning_categories {
            if state.can_spawn_for_category_local(self, category, chunk_pos) {
                self.spawn_category_for_chunk(category, chunk_pos, state, random);
            }
        }
    }

    /// Vanilla `NaturalSpawner.spawnCategoryForChunk`.
    fn spawn_category_for_chunk(
        self: &Arc<Self>,
        category: MobCategory,
        chunk_pos: ChunkPos,
        state: &mut SpawnState,
        random: &mut dyn Rng,
    ) {
        let Some(start) = self.random_pos_within(chunk_pos, random) else {
            return;
        };
        if start.y() > self.get_min_y() {
            self.spawn_category_for_position(category, chunk_pos, start, state, random);
        }
    }

    /// Vanilla `NaturalSpawner.getRandomPosWithin`. `None` when the chunk is not
    /// resident anymore.
    fn random_pos_within(&self, chunk_pos: ChunkPos, random: &mut dyn Rng) -> Option<BlockPos> {
        let x = (chunk_pos.0.x << 4) + random.random_range(0..16);
        let z = (chunk_pos.0.y << 4) + random.random_range(0..16);
        // Steel heights are the first free Y, which is vanilla's `getHeight + 1`.
        let top_empty_y = self.height_at(HeightmapType::WorldSurface, x, z)?;
        let y = random.random_range(self.get_min_y()..=top_empty_y.max(self.get_min_y()));
        Some(BlockPos::new(x, y, z))
    }

    /// Vanilla `NaturalSpawner.spawnCategoryForPosition`.
    pub(crate) fn spawn_category_for_position(
        self: &Arc<Self>,
        category: MobCategory,
        chunk_pos: ChunkPos,
        start: BlockPos,
        state: &mut SpawnState,
        random: &mut dyn Rng,
    ) {
        let y_start = start.y();
        if is_redstone_conductor(self, self.get_block_state(start), start) {
            return;
        }

        let respawn = self.domain_respawn_data();
        let mut cluster_size = 0;
        for _ in 0..3 {
            let mut x = start.x();
            let mut z = start.z();
            let mut current_spawn_data: Option<SpawnerEntry> = None;
            let mut group_data: Option<SpawnGroupData> = None;
            let mut max = (random.random::<f32>() * 4.0).ceil() as i32;
            let mut group_size = 0;
            let mut ll = 0;

            while ll < max {
                ll += 1;
                x += random.random_range(0..6) - random.random_range(0..6);
                z += random.random_range(0..6) - random.random_range(0..6);
                let pos = BlockPos::new(x, y_start, z);
                let xx = f64::from(x) + 0.5;
                let zz = f64::from(z) + 0.5;
                let Some(nearest_player_distance_sqr) =
                    self.nearest_player_distance_sqr(DVec3::new(xx, f64::from(y_start), zz))
                else {
                    continue;
                };
                if !self.is_right_distance_to_player_and_spawn_point(
                    chunk_pos,
                    pos,
                    nearest_player_distance_sqr,
                    &respawn,
                ) {
                    continue;
                }

                let spawn_data = if let Some(spawn_data) = current_spawn_data {
                    spawn_data
                } else {
                    let Some(next_spawn_data) = self.random_spawn_mob_at(category, pos, random)
                    else {
                        break;
                    };
                    current_spawn_data = Some(next_spawn_data);
                    max = next_spawn_data.sample_count(random);
                    next_spawn_data
                };

                if !self.is_valid_spawn_position_for_type(
                    category,
                    &spawn_data,
                    pos,
                    nearest_player_distance_sqr,
                    random,
                ) || !state.can_spawn(self, spawn_data.entity_type, pos)
                {
                    continue;
                }

                let Ok(entity) = create_entity_instance(
                    self,
                    spawn_data.entity_type,
                    DVec3::new(xx, f64::from(y_start), zz),
                ) else {
                    return;
                };
                let Some(mob) = entity.as_mob() else {
                    log::warn!("Can't spawn entity of type: {}", spawn_data.entity_type.key);
                    return;
                };

                entity.snap_to(
                    DVec3::new(xx, f64::from(y_start), zz),
                    random.random::<f32>() * DEGREE_360,
                    0.0,
                );
                if !self.is_valid_position_for_mob(mob, nearest_player_distance_sqr) {
                    continue;
                }

                group_data = mob.finalize_spawn(self, EntitySpawnReason::Natural, group_data);
                cluster_size += 1;
                group_size += 1;
                if let Err(error) = self.try_add_fresh_entity_with_passengers(Arc::clone(&entity)) {
                    log::debug!(
                        "Natural spawn of {} failed: {error:?}",
                        spawn_data.entity_type.key
                    );
                    return;
                }
                state.after_spawn(self, spawn_data.entity_type, entity.block_position());
                if cluster_size >= mob.max_spawn_cluster_size() {
                    return;
                }
                if mob.is_max_group_size_reached(group_size) {
                    break;
                }
            }
        }
    }

    /// Vanilla `NaturalSpawner.isRightDistanceToPlayerAndSpawnPoint`.
    fn is_right_distance_to_player_and_spawn_point(
        &self,
        chunk_pos: ChunkPos,
        pos: BlockPos,
        nearest_player_distance_sqr: f64,
        respawn: &RespawnData,
    ) -> bool {
        if nearest_player_distance_sqr <= MIN_SPAWN_DISTANCE_SQR {
            return false;
        }
        if respawn.dimension() == &self.key {
            let spawn = respawn.pos();
            let dx = f64::from(spawn.x()) + 0.5 - (f64::from(pos.x()) + 0.5);
            let dy = f64::from(spawn.y()) + 0.5 - f64::from(pos.y());
            let dz = f64::from(spawn.z()) + 0.5 - (f64::from(pos.z()) + 0.5);
            if dx * dx + dy * dy + dz * dz < MIN_SPAWN_DISTANCE_SQR {
                return false;
            }
        }
        let pos_chunk = ChunkPos::new(
            SectionPos::block_to_section_coord(pos.x()),
            SectionPos::block_to_section_coord(pos.z()),
        );
        pos_chunk == chunk_pos || self.can_spawn_entities_in_chunk(pos_chunk)
    }

    /// Vanilla `NaturalSpawner.isValidSpawnPositionForType`.
    fn is_valid_spawn_position_for_type(
        self: &Arc<Self>,
        category: MobCategory,
        spawn_data: &SpawnerEntry,
        pos: BlockPos,
        nearest_player_distance_sqr: f64,
        random: &mut dyn Rng,
    ) -> bool {
        let entity_type = spawn_data.entity_type;
        if entity_type.mob_category == MobCategory::Misc {
            return false;
        }
        let despawn_distance = f64::from(entity_type.mob_category.despawn_distance());
        if !entity_type.can_spawn_far_from_player
            && nearest_player_distance_sqr > despawn_distance * despawn_distance
        {
            return false;
        }
        if !entity_type.summonable || !self.mobs_at(category, pos).contains(spawn_data) {
            return false;
        }
        if !SpawnPlacements::is_spawn_position_ok(entity_type, self, pos) {
            return false;
        }
        if !SpawnPlacements::check_spawn_rules(
            entity_type,
            self,
            EntitySpawnReason::Natural,
            pos,
            random,
        ) {
            return false;
        }
        !has_collision(
            &WorldCollisionProvider::new(self),
            spawn_aabb(
                entity_type,
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()),
                f64::from(pos.z()) + 0.5,
            ),
        )
    }

    /// Vanilla `NaturalSpawner.isValidPositionForMob`.
    fn is_valid_position_for_mob(
        self: &Arc<Self>,
        mob: &dyn Mob,
        nearest_player_distance_sqr: f64,
    ) -> bool {
        let despawn_distance = f64::from(mob.entity_type().mob_category.despawn_distance());
        if nearest_player_distance_sqr > despawn_distance * despawn_distance
            && mob.remove_when_far_away(nearest_player_distance_sqr)
        {
            return false;
        }
        mob.check_spawn_rules(self, EntitySpawnReason::Natural) && mob.check_spawn_obstruction(self)
    }

    /// Vanilla `NaturalSpawner.getRandomSpawnMobAt`.
    fn random_spawn_mob_at(
        &self,
        category: MobCategory,
        pos: BlockPos,
        random: &mut dyn Rng,
    ) -> Option<SpawnerEntry> {
        if category == MobCategory::WaterAmbient
            && self
                .biome_at(pos)
                .is_some_and(|biome| biome.has_tag(&BiomeTag::REDUCE_WATER_AMBIENT_SPAWNS))
            && random.random::<f32>() < 0.98
        {
            return None;
        }
        self.mobs_at(category, pos).get_random(random)
    }
}
