//! Lightning bolt: strikes entities, lights fire, powers lightning rods and scrapes copper.

use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::LightningBoltEntityData;
use steel_registry::{REGISTRY, level_events, vanilla_game_events};
use steel_utils::locks::SyncMutex;
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, WorldAabb};

use crate::behavior::blocks::{FireBlock, LightningRodBlock};
use crate::behavior::waxables::get_normal_from_waxed_variant;
use crate::behavior::weathering::{get_weather_state, previous_copper_stage};
use crate::entity::damage::DamageSource;
use crate::entity::{Entity, EntityBase, EntityBaseLoad, EntitySyncedData, RemovalReason};
use crate::world::World;

/// Ticks a flash stays active, counted down from this value.
const START_LIFE: i32 = 2;
/// Horizontal and downward reach of the strike, in blocks.
const DAMAGE_RADIUS: f64 = 3.0;
/// The strike also reaches this far above the bolt, on top of [`DAMAGE_RADIUS`].
const DAMAGE_HEIGHT: f64 = 6.0;
/// How many random positions each copper-cleaning step tries.
const COPPER_STEP_CANDIDATES: usize = 10;

/// Mutable bolt state, read and written once per tick.
struct LightningBoltState {
    life: i32,
    flashes: i32,
    visual_only: bool,
    blocks_set_on_fire: i32,
}

/// A lightning bolt, as summoned by thunderstorms, channeling tridents or commands.
#[entity_behavior(class = "LightningBolt")]
pub struct LightningBoltEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<LightningBoltEntityData>,
    state: SyncMutex<LightningBoltState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `LightningBoltEntity`.
unsafe impl DowncastType for LightningBoltEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/lightning_bolt");
}

impl LightningBoltEntity {
    /// Creates a new lightning bolt with a random number of extra flashes.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(LightningBoltEntityData::new()),
            state: SyncMutex::new(Self::initial_state()),
        }
    }

    /// Creates a lightning bolt from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(LightningBoltEntityData::new()),
            state: SyncMutex::new(Self::initial_state()),
        }
    }

    fn initial_state() -> LightningBoltState {
        LightningBoltState {
            life: START_LIFE,
            flashes: rand::random_range(0..3) + 1,
            visual_only: false,
            blocks_set_on_fire: 0,
        }
    }

    /// Makes the bolt purely cosmetic: it neither hurts entities nor lights fire.
    pub fn set_visual_only(&self, visual_only: bool) {
        self.state.lock().visual_only = visual_only;
    }

    /// Returns whether the bolt is purely cosmetic.
    #[must_use]
    pub fn is_visual_only(&self) -> bool {
        self.state.lock().visual_only
    }

    /// Returns how many fire blocks this bolt has placed.
    #[must_use]
    pub fn blocks_set_on_fire(&self) -> i32 {
        self.state.lock().blocks_set_on_fire
    }

    /// The block the bolt landed on, just below its feet.
    fn strike_position(&self) -> BlockPos {
        let position = self.position();
        BlockPos::containing(position.x, position.y - 1.0E-6, position.z)
    }

    fn server_tick(&self, world: &Arc<World>) {
        if self.state.lock().life == START_LIFE {
            let difficulty = world.difficulty();
            if matches!(difficulty, Difficulty::Normal | Difficulty::Hard) {
                self.spawn_fire(world, 4);
            }
            self.power_lightning_rod(world);
            clear_copper_on_lightning_strike(world, self.strike_position());
            self.game_event(&vanilla_game_events::LIGHTNING_STRIKE);
        }

        let (life, visual_only) = {
            let mut state = self.state.lock();
            state.life -= 1;
            (state.life, state.visual_only)
        };
        if life < 0 {
            let flashes = self.state.lock().flashes;
            if flashes == 0 {
                self.set_removed(RemovalReason::Discarded);
            } else if life < -rand::random_range(0..10) {
                {
                    let mut state = self.state.lock();
                    state.flashes -= 1;
                    state.life = 1;
                }
                self.spawn_fire(world, 0);
            }
        }

        if self.state.lock().life >= 0 && !visual_only {
            self.strike_entities(world);
        }
    }

    fn strike_entities(&self, world: &Arc<World>) {
        let position = self.position();
        let bounds = WorldAabb::from_min_max(
            DVec3::new(
                position.x - DAMAGE_RADIUS,
                position.y - DAMAGE_RADIUS,
                position.z - DAMAGE_RADIUS,
            ),
            DVec3::new(
                position.x + DAMAGE_RADIUS,
                position.y + DAMAGE_HEIGHT + DAMAGE_RADIUS,
                position.z + DAMAGE_RADIUS,
            ),
        );
        let own_id = self.id();
        let entities = world.get_entities_in_aabb_matching(&bounds, |entity| {
            entity.id() != own_id && entity.is_alive()
        });
        for entity in entities {
            entity.thunder_hit(world, self);
        }
    }

    fn power_lightning_rod(&self, world: &Arc<World>) {
        let pos = self.strike_position();
        let state = world.get_block_state(pos);
        let block = state.get_block();
        if block.has_tag(&BlockTag::LIGHTNING_RODS) {
            LightningRodBlock::on_lightning_strike(block, state, world, pos);
        }
    }

    fn spawn_fire(&self, world: &Arc<World>, additional_sources: i32) {
        if self.state.lock().visual_only {
            return;
        }
        let pos = self.block_position();
        if !world.can_spread_fire_around(pos) {
            return;
        }

        let mut placed = i32::from(place_fire(world, pos));
        for _ in 0..additional_sources {
            let nearby = pos.offset(
                rand::random_range(0..3) - 1,
                rand::random_range(0..3) - 1,
                rand::random_range(0..3) - 1,
            );
            placed += i32::from(place_fire(world, nearby));
        }
        self.state.lock().blocks_set_on_fire += placed;
    }
}

/// Places fire (or soul fire) at `pos` when it is air and the fire could stay there.
fn place_fire(world: &Arc<World>, pos: BlockPos) -> bool {
    let fire = FireBlock::get_state(world.as_ref(), pos);
    if world.get_block_state(pos).is_air()
        && FireBlock::fire_state_can_survive(world.as_ref(), fire, pos)
    {
        world.set_block(pos, fire, UpdateFlags::UPDATE_ALL);
        return true;
    }
    false
}

/// Vanilla `WeatheringCopper.getPrevious`: the next less oxidized stage, keeping properties.
fn previous_copper_state(state: BlockStateId) -> Option<BlockStateId> {
    previous_copper_stage(state.get_block())
        .map(|previous| REGISTRY.blocks.copy_matching_properties(state, previous))
}

/// Vanilla `WeatheringCopper.getFirst`: the unoxidized stage, keeping properties.
fn first_copper_state(state: BlockStateId) -> BlockStateId {
    let mut block = state.get_block();
    while let Some(previous) = previous_copper_stage(block) {
        block = previous;
    }
    REGISTRY.blocks.copy_matching_properties(state, block)
}

/// Scrapes oxidation off a struck copper block and some copper around it.
fn clear_copper_on_lightning_strike(world: &Arc<World>, struck_pos: BlockPos) {
    let struck_state = world.get_block_state(struck_pos);
    let block = struck_state.get_block();
    let is_waxed = get_normal_from_waxed_variant(block).is_some();
    let is_weathering_copper = get_weather_state(block).is_some();
    if !is_weathering_copper && !is_waxed {
        return;
    }

    if is_weathering_copper {
        world.set_block(
            struck_pos,
            first_copper_state(world.get_block_state(struck_pos)),
            UpdateFlags::UPDATE_ALL,
        );
    }

    let strikes = rand::random_range(0..3) + 3;
    for _ in 0..strikes {
        let steps = rand::random_range(0..8) + 1;
        random_walk_cleaning_copper(world, struck_pos, steps);
    }
}

fn random_walk_cleaning_copper(world: &Arc<World>, origin: BlockPos, steps: i32) {
    let mut pos = origin;
    for _ in 0..steps {
        let Some(next) = random_step_cleaning_copper(world, pos) else {
            break;
        };
        pos = next;
    }
}

/// Tries up to ten random neighbors (vanilla `BlockPos.randomInCube(random, 10, pos, 1)`)
/// and scrapes the first weathering copper block found.
fn random_step_cleaning_copper(world: &Arc<World>, pos: BlockPos) -> Option<BlockPos> {
    for _ in 0..COPPER_STEP_CANDIDATES {
        let candidate = pos.offset(
            rand::random_range(0..3) - 1,
            rand::random_range(0..3) - 1,
            rand::random_range(0..3) - 1,
        );
        let state = world.get_block_state(candidate);
        if get_weather_state(state.get_block()).is_some() {
            if let Some(previous) = previous_copper_state(state) {
                world.set_block(candidate, previous, UpdateFlags::UPDATE_ALL);
            }
            world.level_event(level_events::PARTICLES_ELECTRIC_SPARK, candidate, -1, None);
            return Some(candidate);
        }
    }
    None
}

impl Entity for LightningBoltEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.default_tick();
        if let Some(world) = self.level() {
            self.server_tick(&world);
        }
    }

    fn hurt(&self, _world: &World, _source: &DamageSource, _amount: f32) -> bool {
        false
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::blocks::properties::BlockStateProperties;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos, Downcast as _};

    use super::LightningBoltEntity;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::CreeperEntity;
    use crate::entity::{Entity, LivingEntity as _, SharedEntity, next_entity_id};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
    use crate::world::World;

    const STRIKE: DVec3 = DVec3::new(8.5, 65.0, 8.5);
    const STRUCK_BLOCK: BlockPos = BlockPos::new(8, 64, 8);

    fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> SharedEntity {
        let entity: SharedEntity = Arc::new(entity);
        world
            .try_add_entity(Arc::clone(&entity))
            .expect("test entity should attach to the loaded chunk");
        entity
    }

    fn bolt(world: &Arc<World>) -> SharedEntity {
        add(
            world,
            LightningBoltEntity::new(
                &vanilla_entities::LIGHTNING_BOLT,
                next_entity_id(),
                STRIKE,
                Arc::downgrade(world),
            ),
        )
    }

    #[test]
    fn lightning_charges_and_burns_a_creeper() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("lightning_creeper");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let creeper = add(
            world,
            CreeperEntity::new(
                &vanilla_entities::CREEPER,
                next_entity_id(),
                STRIKE,
                Arc::downgrade(world),
            ),
        );
        let bolt = bolt(world);

        Arc::clone(&bolt).tick();

        let creeper_ref = creeper
            .downcast_ref::<CreeperEntity>()
            .expect("creeper is concrete");
        assert!(creeper_ref.is_powered());
        assert!(creeper_ref.remaining_fire_ticks() > 0);
        assert!(creeper_ref.get_health() < creeper_ref.get_max_health());
    }

    #[test]
    fn visual_only_lightning_leaves_entities_alone() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("lightning_visual");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let creeper = add(
            world,
            CreeperEntity::new(
                &vanilla_entities::CREEPER,
                next_entity_id(),
                STRIKE,
                Arc::downgrade(world),
            ),
        );
        let bolt = bolt(world);
        bolt.downcast_ref::<LightningBoltEntity>()
            .expect("bolt is concrete")
            .set_visual_only(true);

        Arc::clone(&bolt).tick();

        let creeper_ref = creeper
            .downcast_ref::<CreeperEntity>()
            .expect("creeper is concrete");
        assert!(!creeper_ref.is_powered());
        assert_eq!(creeper_ref.remaining_fire_ticks(), 0);
    }

    #[test]
    fn lightning_disappears_after_its_flashes() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("lightning_life");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let bolt = bolt(world);

        // Up to 3 extra flashes, each re-arming within 10 ticks.
        for _ in 0..60 {
            if bolt.is_removed() {
                break;
            }
            Arc::clone(&bolt).tick();
        }
        assert!(bolt.is_removed());
    }

    #[test]
    fn lightning_scrapes_struck_copper_back_to_unaffected() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("lightning_copper");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        world.set_block(
            STRUCK_BLOCK,
            vanilla_blocks::OXIDIZED_COPPER.default_state(),
            UpdateFlags::UPDATE_ALL,
        );

        Arc::clone(&bolt(world)).tick();

        assert_eq!(
            world.get_block_state(STRUCK_BLOCK).get_block(),
            &vanilla_blocks::COPPER_BLOCK
        );
    }

    #[test]
    fn lightning_powers_the_rod_it_strikes() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("lightning_rod");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        world.set_block(
            STRUCK_BLOCK,
            vanilla_blocks::WAXED_LIGHTNING_ROD.default_state(),
            UpdateFlags::UPDATE_ALL,
        );

        Arc::clone(&bolt(world)).tick();

        let state = world.get_block_state(STRUCK_BLOCK);
        assert_eq!(state.get_block(), &vanilla_blocks::WAXED_LIGHTNING_ROD);
        assert!(state.get_value(&BlockStateProperties::POWERED));
    }
}
