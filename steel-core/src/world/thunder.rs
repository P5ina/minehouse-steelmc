//! Vanilla `ServerLevel.tickThunder`: lightning strikes during thunderstorms,
//! which sometimes leave a skeleton horse trap.

use std::sync::Arc;

use glam::DVec3;
use rand::seq::IndexedRandom as _;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_rules::SPAWN_MOBS;
use steel_registry::{RegistryEntry as _, vanilla_entities, vanilla_poi_types};
use steel_utils::{BlockPos, ChunkPos, Downcast as _, WorldAabb};

use super::World;
use crate::chunk::full_chunk::BlockRandomPositionGenerator;
use crate::chunk::heightmap::HeightmapType;
use crate::entity::entities::{LightningBoltEntity, SkeletonHorseEntity};
use crate::entity::{AgeableMob as _, create_entity_instance};
use crate::poi::OccupationStatus;

/// Vanilla `random.nextInt(100000) == 0` strike chance per chunk tick.
const THUNDER_CHANCE: u32 = 100_000;
/// Vanilla `findLightningRod` search radius.
const LIGHTNING_ROD_RANGE: i32 = 128;
/// Vanilla `findLightningTargetAround` horizontal inflate of the search box.
const LIVING_TARGET_INFLATE: f64 = 3.0;
/// Vanilla `difficulty.getEffectiveDifficulty() * 0.01` trap chance factor.
const TRAP_CHANCE_PER_DIFFICULTY: f64 = 0.01;

impl World {
    /// Vanilla `ServerLevel.tickThunder`, run for each spawning chunk in
    /// entity-ticking range.
    pub(crate) fn tick_thunder(self: &Arc<Self>, chunk_pos: ChunkPos) {
        if !self.is_raining() || !self.is_thundering() || rand::random_range(0..THUNDER_CHANCE) != 0
        {
            return;
        }
        let mut random_positions = BlockRandomPositionGenerator::from_runtime_rng();
        let origin = random_positions.next_pos(chunk_pos.0.x << 4, 0, chunk_pos.0.y << 4);
        let Some(pos) = self.find_lightning_target_around(origin) else {
            return;
        };
        if !self.is_raining_at(pos) {
            return;
        }

        let difficulty = self.current_difficulty_at(pos);
        let is_trap = self.get_game_rule(&SPAWN_MOBS)
            && rand::random::<f64>()
                < f64::from(difficulty.effective_difficulty()) * TRAP_CHANCE_PER_DIFFICULTY
            && !self
                .get_block_state(pos.below())
                .get_block()
                .has_tag(&BlockTag::LIGHTNING_RODS);
        if is_trap {
            self.spawn_skeleton_horse_trap(pos);
        }

        let strike = DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()),
            f64::from(pos.z()) + 0.5,
        );
        let Ok(bolt) = create_entity_instance(self, &vanilla_entities::LIGHTNING_BOLT, strike)
        else {
            return;
        };
        bolt.snap_to(strike, 0.0, 0.0);
        if let Some(bolt) = bolt.downcast_ref::<LightningBoltEntity>() {
            bolt.set_visual_only(is_trap);
        }
        if let Err(error) = self.try_add_entity(bolt) {
            log::debug!("world rejected a storm lightning bolt: {error}");
        }
    }

    /// The trap half of vanilla `tickThunder`. Vanilla does not run
    /// `finalizeSpawn` here and places the horse at the block corner.
    fn spawn_skeleton_horse_trap(self: &Arc<Self>, pos: BlockPos) {
        let corner = DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()));
        let Ok(horse) = create_entity_instance(self, &vanilla_entities::SKELETON_HORSE, corner)
        else {
            return;
        };
        if let Some(skeleton_horse) = horse.downcast_ref::<SkeletonHorseEntity>() {
            skeleton_horse.set_trap(true);
            skeleton_horse.set_age(0);
        }
        if let Err(error) = self.try_add_entity(horse) {
            log::debug!("world rejected a skeleton horse trap: {error}");
        }
    }

    /// Vanilla `ServerLevel.findLightningTargetAround`. `None` when the
    /// column is not loaded.
    fn find_lightning_target_around(&self, pos: BlockPos) -> Option<BlockPos> {
        let height = self.height_at(HeightmapType::MotionBlocking, pos.x(), pos.z())?;
        let mut center = pos.at_y(height);
        if let Some(rod) = self.find_lightning_rod(center) {
            return Some(rod);
        }

        let search = WorldAabb::new(
            f64::from(center.x()),
            f64::from(center.y()),
            f64::from(center.z()),
            f64::from(center.x()) + 1.0,
            f64::from(self.get_max_y() + 1) + 1.0,
            f64::from(center.z()) + 1.0,
        )
        .inflate(LIVING_TARGET_INFLATE);
        let entities = self.get_entities_in_aabb_matching(&search, |entity| {
            entity.as_living_entity().is_some()
                && entity.is_alive()
                && self.can_see_sky(entity.block_position())
        });
        if let Some(target) = entities.choose(&mut rand::rng()) {
            return Some(target.block_position());
        }

        if center.y() == self.get_min_y() - 1 {
            center = center.above_n(2);
        }
        Some(center)
    }

    /// Vanilla `ServerLevel.findLightningRod`: the closest lightning rod on
    /// the world surface within 128 blocks, struck from the block above.
    fn find_lightning_rod(&self, center: BlockPos) -> Option<BlockPos> {
        let lightning_rod_type = vanilla_poi_types::LIGHTNING_ROD.try_id()?;
        let radius_sqr = i64::from(LIGHTNING_ROD_RANGE) * i64::from(LIGHTNING_ROD_RANGE);
        let candidates = self.poi_storage.lock().get_in_horizontal_square(
            &|type_id| type_id == lightning_rod_type,
            center,
            LIGHTNING_ROD_RANGE,
            OccupationStatus::Any,
        );
        candidates
            .into_iter()
            .map(|(pos, _)| pos)
            .filter(|pos| dist_sqr(*pos, center) <= radius_sqr)
            .filter(|pos| {
                self.height_at(HeightmapType::WorldSurface, pos.x(), pos.z())
                    .is_some_and(|height| pos.y() == height - 1)
            })
            .min_by_key(|pos| dist_sqr(*pos, center))
            .map(|pos| pos.above())
    }
}

/// Vanilla `Vec3i.distSqr` between two block positions.
fn dist_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.x() - b.x());
    let dy = i64::from(a.y() - b.y());
    let dz = i64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos};

    use crate::behavior::init_behaviors;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn storm_lightning_targets_the_surface_rod() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("thunder_rod");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let rod = BlockPos::new(3, 70, 12);
        world.set_block(
            rod,
            vanilla_blocks::LIGHTNING_ROD.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        // Ground under the strike column keeps the rod within 128 blocks.
        world.set_block(
            BlockPos::new(10, 69, 4),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        );

        assert_eq!(
            world.find_lightning_target_around(BlockPos::new(10, 0, 4)),
            Some(rod.above())
        );
    }

    #[test]
    fn buried_rod_is_ignored() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("thunder_buried_rod");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let rod = BlockPos::new(3, 70, 12);
        world.set_block(
            rod,
            vanilla_blocks::LIGHTNING_ROD.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        world.set_block(
            rod.above(),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        );

        assert_eq!(world.find_lightning_rod(BlockPos::new(10, 70, 4)), None);
    }
}
