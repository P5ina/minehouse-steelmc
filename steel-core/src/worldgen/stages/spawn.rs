use std::sync::Arc;

use crate::chunk::{
    chunk_generation_task::StaticCache2D, chunk_holder::ChunkHolder, chunk_pyramid::ChunkStep,
};
use crate::worldgen::generator::ChunkGenerator;
use crate::worldgen::generator::context::WorldGenContext;
use crate::worldgen::region::WorldGenRegion;

/// Vanilla `ChunkStatusTasks.generateSpawn`.
pub(crate) fn generate(
    context: Arc<WorldGenContext>,
    step: &ChunkStep,
    cache: &Arc<StaticCache2D<Arc<ChunkHolder>>>,
    holder: Arc<ChunkHolder>,
) {
    let center = holder.get_pos();
    let region_random = context
        .generator
        .create_worldgen_region_random(context.world().seed(), center);
    let region = WorldGenRegion::new(context.as_ref(), step, cache, center, region_random);
    context.generator.spawn_original_mobs(&region);
}
