//! Vanilla `CreakingHeartBlockEntity`: spawns and guards the heart's creaking.

use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::RngExt as _;
use rand::seq::SliceRandom as _;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, CreakingHeartState};
use steel_registry::particle_type::{ParticleData, TrailParticleOption};
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{
    sound_events, vanilla_block_entity_types, vanilla_blocks, vanilla_entities,
    vanilla_game_events, vanilla_particle_types,
};
use steel_utils::types::{Difficulty, TraversalNodeStatus, UpdateFlags};
use steel_utils::{
    BlockPos, BlockStateId, Direction, Downcast as _, DowncastType, DowncastTypeKey, RgbColor,
    UuidExt as _, axis::Axis, locks::SyncMutex,
};
use uuid::Uuid;

use crate::behavior::blocks::multiface_face_property;
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::damage::DamageSource;
use crate::entity::entities::CreakingEntity;
use crate::entity::spawn_util::{on_top_of_collider_no_leaves, try_spawn_mob};
use crate::entity::{Entity, EntitySpawnReason, LivingEntity, Mob as _, SharedEntity};
use crate::world::game_event::GameEventContext;
use crate::world::{LevelReader, World};
use steel_utils::entity_events::EntityStatus;

/// Vanilla `PLAYER_DETECTION_RANGE`.
const PLAYER_DETECTION_RANGE: f64 = 32.0;
/// Vanilla `CREAKING_ROAMING_RADIUS`.
const CREAKING_ROAMING_RADIUS: f64 = 32.0;
/// Vanilla `DISTANCE_CREAKING_TOO_FAR`.
const DISTANCE_CREAKING_TOO_FAR: f64 = 34.0;
/// Vanilla `SPAWN_RANGE_XZ`, `SPAWN_RANGE_Y` and `ATTEMPTS_PER_SPAWN`.
const SPAWN_RANGE_XZ: i32 = 16;
const SPAWN_RANGE_Y: i32 = 8;
const ATTEMPTS_PER_SPAWN: i32 = 5;
/// Vanilla `UPDATE_TICKS` and `UPDATE_TICKS_VARIANCE`.
const UPDATE_TICKS: i32 = 20;
const UPDATE_TICKS_VARIANCE: i32 = 5;
/// Vanilla `HURT_CALL_TOTAL_TICKS`, `HURT_CALL_INTERVAL` and
/// `HURT_CALL_PARTICLE_TICKS`.
const HURT_CALL_TOTAL_TICKS: i32 = 100;
const HURT_CALL_INTERVAL: i32 = 10;
const HURT_CALL_PARTICLE_TICKS: i32 = 50;
/// Vanilla `MAX_DEPTH` and `MAX_COUNT` of the resin search.
const MAX_DEPTH: i32 = 2;
const MAX_COUNT: i32 = 64;
/// Vanilla `TICKS_GRACE_PERIOD`: how long a saved creaking may stay unloaded.
const TICKS_GRACE_PERIOD: i64 = 30;
/// Vanilla `Creaking.CREAKING_ORANGE` and `CREAKING_GRAY`.
const CREAKING_ORANGE: i32 = 16_545_810;
const CREAKING_GRAY: i32 = 6_250_335;

/// Vanilla `Either<Creaking, UUID>`: the live creaking, or the UUID of one
/// that has not been resolved since loading.
enum CreakingInfo {
    Entity(SharedEntity),
    Uuid(Uuid),
}

struct HeartData {
    creaking_info: Option<CreakingInfo>,
    ticks_existed: i64,
    ticker: i32,
    emitter: i32,
    emitter_target: Option<DVec3>,
    output_signal: i32,
}

/// Block entity for `creaking_heart` blocks.
pub struct CreakingHeartBlockEntity {
    base: BlockEntityBase,
    heart: SyncMutex<HeartData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `CreakingHeartBlockEntity`.
unsafe impl DowncastType for CreakingHeartBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/creaking_heart");
}

impl CreakingHeartBlockEntity {
    /// Creates a heart block entity with no creaking.
    #[must_use]
    pub fn new(world: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: BlockEntityBase::new(
                &vanilla_block_entity_types::CREAKING_HEART,
                world,
                pos,
                state,
            ),
            heart: SyncMutex::new(HeartData {
                creaking_info: None,
                ticks_existed: 0,
                ticker: 0,
                emitter: 0,
                emitter_target: None,
                output_signal: 0,
            }),
        }
    }

    /// Vanilla `serverTick`.
    fn server_tick(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        let emitter = {
            let mut heart = self.heart.lock();
            heart.ticks_existed += 1;
            heart.emitter
        };

        let computed_output_signal = self.compute_analog_output_signal(world);
        let output_changed = {
            let mut heart = self.heart.lock();
            let changed = heart.output_signal != computed_output_signal;
            heart.output_signal = computed_output_signal;
            changed
        };
        if output_changed {
            world.update_neighbor_for_output_signal(pos, &vanilla_blocks::CREAKING_HEART);
        }

        if emitter > 0 {
            self.tick_emitter(world, pos, emitter);
        }

        let ticker = {
            let mut heart = self.heart.lock();
            let ticker = heart.ticker;
            heart.ticker -= 1;
            ticker
        };
        if ticker >= 0 {
            return;
        }
        self.heart.lock().ticker =
            rand::rng().random_range(0..UPDATE_TICKS_VARIANCE) + UPDATE_TICKS;

        let updated_state = self.update_creaking_state(world, state, pos);
        if updated_state != state {
            world.set_block(pos, updated_state, UpdateFlags::UPDATE_ALL);
            if heart_state(updated_state) == CreakingHeartState::Uprooted {
                return;
            }
        }

        if self.heart.lock().creaking_info.is_none() {
            if heart_state(updated_state) == CreakingHeartState::Awake
                && world.is_spawning_monsters()
                && world.difficulty() != Difficulty::Peaceful
                && world
                    .nearest_player(
                        DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z())),
                        PLAYER_DETECTION_RANGE,
                        |player| !player.is_spectator(),
                    )
                    .is_some()
                && let Some(creaking) = self.spawn_protector(world)
            {
                self.set_creaking_info(CreakingInfo::Entity(Arc::clone(&creaking)));
                if let Some(living) = creaking.as_living_entity() {
                    living.make_sound(Some(&sound_events::ENTITY_CREAKING_SPAWN));
                }
                world.play_sound(
                    &sound_events::BLOCK_CREAKING_HEART_SPAWN,
                    SoundSource::Blocks,
                    pos,
                    1.0,
                    1.0,
                    None,
                );
            }
        } else if let Some(creaking) = self.creaking_protector(world)
            && let Some(creaking) = creaking.downcast_ref::<CreakingEntity>()
            && (!world.creaking_active() && !creaking.is_persistence_required()
                || self.distance_to_creaking(world) > DISTANCE_CREAKING_TOO_FAR
                || creaking.player_is_stuck_in_you())
        {
            self.remove_protector(world, None);
        }
    }

    fn tick_emitter(&self, world: &Arc<World>, pos: BlockPos, emitter: i32) {
        if emitter > HURT_CALL_PARTICLE_TICKS {
            self.emit_particles(world, 1, true);
            self.emit_particles(world, 1, false);
        }

        let emitter_target = self.heart.lock().emitter_target;
        if emitter % HURT_CALL_INTERVAL == 0
            && let Some(mut target) = emitter_target
        {
            if let Some(creaking) = self.creaking_protector(world) {
                target = creaking.bounding_box().center();
                self.heart.lock().emitter_target = Some(target);
            }
            let heart_position = center_of(pos);
            let progress = 0.2 + 0.8 * (HURT_CALL_TOTAL_TICKS - emitter) as f32 / 100.0;
            let sound_location = (heart_position - target) * f64::from(progress) + target;
            let volume = emitter as f32 / 2.0 / 100.0 + 0.5;
            world.play_sound(
                &sound_events::BLOCK_CREAKING_HEART_HURT,
                SoundSource::Blocks,
                BlockPos::from(sound_location),
                volume,
                1.0,
                None,
            );
        }

        self.heart.lock().emitter -= 1;
    }

    /// Vanilla `updateCreakingState`.
    fn update_creaking_state(
        &self,
        world: &World,
        state: BlockStateId,
        pos: BlockPos,
    ) -> BlockStateId {
        if !has_required_logs(state, world, pos) && self.heart.lock().creaking_info.is_none() {
            return state.set_value(
                &BlockStateProperties::CREAKING_HEART_STATE,
                CreakingHeartState::Uprooted,
            );
        }
        let heart_state = if world.creaking_active() {
            CreakingHeartState::Awake
        } else {
            CreakingHeartState::Dormant
        };
        state.set_value(&BlockStateProperties::CREAKING_HEART_STATE, heart_state)
    }

    fn distance_to_creaking(&self, world: &World) -> f64 {
        self.creaking_protector(world).map_or(0.0, |creaking| {
            let (x, y, z) = self.get_block_pos().get_bottom_center();
            creaking
                .position()
                .distance_squared(DVec3::new(x, y, z))
                .sqrt()
        })
    }

    fn set_creaking_info(&self, info: CreakingInfo) {
        let mut heart = self.heart.lock();
        if matches!(info, CreakingInfo::Uuid(_)) {
            heart.ticks_existed = 0;
        }
        heart.creaking_info = Some(info);
        drop(heart);
        self.set_changed();
    }

    fn clear_creaking_info(&self) {
        self.heart.lock().creaking_info = None;
        self.set_changed();
    }

    /// Vanilla `setCreakingInfo(Creaking)`: binds a freshly spawned creaking.
    pub fn set_creaking(&self, creaking: SharedEntity) {
        self.set_creaking_info(CreakingInfo::Entity(creaking));
    }

    /// Vanilla `getCreakingProtector`: the live creaking, resolving a saved
    /// UUID and giving up on it after the grace period.
    fn creaking_protector(&self, world: &World) -> Option<SharedEntity> {
        let uuid = {
            let heart = self.heart.lock();
            match heart.creaking_info.as_ref()? {
                CreakingInfo::Entity(creaking) if !creaking.is_removed() => {
                    return Some(Arc::clone(creaking));
                }
                CreakingInfo::Entity(creaking) => creaking.uuid(),
                CreakingInfo::Uuid(uuid) => *uuid,
            }
        };
        if matches!(
            self.heart.lock().creaking_info,
            Some(CreakingInfo::Entity(_))
        ) {
            self.set_creaking_info(CreakingInfo::Uuid(uuid));
        }

        if let Some(creaking) = world.get_entity_by_uuid(&uuid)
            && creaking.downcast_ref::<CreakingEntity>().is_some()
        {
            self.set_creaking_info(CreakingInfo::Entity(Arc::clone(&creaking)));
            return Some(creaking);
        }
        if self.heart.lock().ticks_existed >= TICKS_GRACE_PERIOD {
            self.clear_creaking_info();
        }
        None
    }

    /// Vanilla `spawnProtector`.
    fn spawn_protector(&self, world: &Arc<World>) -> Option<SharedEntity> {
        let pos = self.get_block_pos();
        let creaking = try_spawn_mob(
            world,
            &vanilla_entities::CREAKING,
            EntitySpawnReason::Spawner,
            pos,
            ATTEMPTS_PER_SPAWN,
            SPAWN_RANGE_XZ,
            SPAWN_RANGE_Y,
            on_top_of_collider_no_leaves,
            true,
        )?;
        world.game_event_at(
            &vanilla_game_events::ENTITY_PLACE,
            creaking.position(),
            &GameEventContext::new(Some(creaking.as_ref()), None),
        );
        creaking.broadcast_entity_event(EntityStatus::Poof);
        if let Some(creaking) = creaking.downcast_ref::<CreakingEntity>() {
            creaking.set_transient(pos);
        }
        Some(creaking)
    }

    /// Vanilla `creakingHurt`: calls out to the creaking and grows resin.
    pub fn creaking_hurt(&self, world: &Arc<World>) {
        let Some(creaking) = self.creaking_protector(world) else {
            return;
        };
        if self.heart.lock().emitter > 0 {
            return;
        }
        self.emit_particles(world, 20, false);
        if heart_state(world.get_block_state(self.get_block_pos())) == CreakingHeartState::Awake {
            let clumps = rand::rng().random_range(2..=3);
            for _ in 0..clumps {
                if let Some(resin_pos) = self.spread_resin(world) {
                    world.play_sound(
                        &sound_events::BLOCK_RESIN_PLACE,
                        SoundSource::Blocks,
                        resin_pos,
                        1.0,
                        1.0,
                        None,
                    );
                    world.game_event(
                        &vanilla_game_events::BLOCK_PLACE,
                        resin_pos,
                        &GameEventContext::new(None, Some(self.get_block_state())),
                    );
                }
            }
        }
        let mut heart = self.heart.lock();
        heart.emitter = HURT_CALL_TOTAL_TICKS;
        heart.emitter_target = Some(creaking.bounding_box().center());
    }

    /// Vanilla `spreadResin`: puts one resin face on a pale oak log within two
    /// logs of the heart.
    fn spread_resin(&self, world: &Arc<World>) -> Option<BlockPos> {
        let mut random = rand::rng();
        let mut placed = None;
        let _ = BlockPos::breadth_first_traversal(
            self.get_block_pos(),
            MAX_DEPTH,
            MAX_COUNT,
            |pos, accept| {
                for direction in shuffled_directions(&mut random) {
                    let neighbor = pos.relative(direction);
                    if is_pale_oak_log(world.get_block_state(neighbor)) {
                        accept(neighbor);
                    }
                }
            },
            |pos| {
                if !is_pale_oak_log(world.get_block_state(pos)) {
                    return TraversalNodeStatus::Accept;
                }
                for direction in shuffled_directions(&mut rand::rng()) {
                    let neighbor_pos = pos.relative(direction);
                    let mut neighbor_state = world.get_block_state(neighbor_pos);
                    let opposite = direction.opposite();
                    if neighbor_state.is_air() {
                        neighbor_state = vanilla_blocks::RESIN_CLUMP.default_state();
                    } else if neighbor_state.get_block() == &vanilla_blocks::WATER
                        && neighbor_state.get_fluid_state().is_source()
                    {
                        neighbor_state = vanilla_blocks::RESIN_CLUMP
                            .default_state()
                            .set_value(&BlockStateProperties::WATERLOGGED, true);
                    }
                    let face = multiface_face_property(opposite);
                    if neighbor_state.get_block() == &vanilla_blocks::RESIN_CLUMP
                        && !neighbor_state.try_get_value(face).unwrap_or(false)
                    {
                        world.set_block(
                            neighbor_pos,
                            neighbor_state.set_value(face, true),
                            UpdateFlags::UPDATE_ALL,
                        );
                        placed = Some(neighbor_pos);
                        return TraversalNodeStatus::Stop;
                    }
                }
                TraversalNodeStatus::Accept
            },
        );
        placed
    }

    /// Vanilla `emitParticles`: trails between the creaking and the heart.
    fn emit_particles(&self, world: &Arc<World>, count: i32, towards_creaking: bool) {
        let Some(creaking) = self.creaking_protector(world) else {
            return;
        };
        let color = RgbColor::new(if towards_creaking {
            CREAKING_ORANGE
        } else {
            CREAKING_GRAY
        });
        let mut random = rand::rng();
        let heart_pos = self.get_block_pos();
        for _ in 0..count {
            let bounding_box = creaking.bounding_box();
            let mut source = DVec3::new(
                bounding_box.min_x() + random.random::<f64>() * bounding_box.width(),
                bounding_box.min_y() + random.random::<f64>() * bounding_box.height(),
                bounding_box.min_z() + random.random::<f64>() * bounding_box.depth(),
            );
            let mut destination = DVec3::new(
                f64::from(heart_pos.x()) + random.random::<f64>(),
                f64::from(heart_pos.y()) + random.random::<f64>(),
                f64::from(heart_pos.z()) + random.random::<f64>(),
            );
            if towards_creaking {
                mem::swap(&mut source, &mut destination);
            }
            let duration = random.random_range(0..40) + 10;
            world.send_particles_with_options(
                ParticleData::new(
                    &vanilla_particle_types::TRAIL,
                    TrailParticleOption::new(destination, color, duration),
                ),
                true,
                true,
                source,
                1,
                DVec3::ZERO,
                0.0,
            );
        }
    }

    /// Vanilla `removeProtector`: crumbles the creaking, or kills it when
    /// `source` broke the heart.
    pub fn remove_protector(&self, world: &World, source: Option<&DamageSource>) {
        let Some(creaking) = self.creaking_protector(world) else {
            return;
        };
        if let Some(creaking) = creaking.downcast_ref::<CreakingEntity>() {
            match source {
                None => creaking.tear_down(),
                Some(source) => {
                    creaking.creaking_death_effects(source);
                    creaking.set_tearing_down();
                    creaking.set_health(0.0);
                }
            }
        }
        self.clear_creaking_info();
    }

    /// Vanilla `isProtector`: whether entity `creaking_id` is this heart's
    /// creaking.
    #[must_use]
    pub fn is_protector(&self, world: &World, creaking_id: i32) -> bool {
        self.creaking_protector(world)
            .is_some_and(|creaking| creaking.id() == creaking_id)
    }

    /// Vanilla `getAnalogOutputSignal`.
    #[must_use]
    pub fn analog_output_signal(&self) -> i32 {
        self.heart.lock().output_signal
    }

    /// Vanilla `computeAnalogOutputSignal`: 15 next to the heart, falling to 0
    /// at the roaming radius.
    fn compute_analog_output_signal(&self, world: &World) -> i32 {
        if self.heart.lock().creaking_info.is_none() || self.creaking_protector(world).is_none() {
            return 0;
        }
        let distance = self.distance_to_creaking(world);
        let scaled_distance =
            distance.clamp(0.0, CREAKING_ROAMING_RADIUS) / CREAKING_ROAMING_RADIUS;
        15 - (scaled_distance * 15.0).floor() as i32
    }
}

fn heart_state(state: BlockStateId) -> CreakingHeartState {
    state.get_value(&BlockStateProperties::CREAKING_HEART_STATE)
}

fn is_pale_oak_log(state: BlockStateId) -> bool {
    state.get_block().has_tag(&BlockTag::PALE_OAK_LOGS)
}

fn shuffled_directions(random: &mut impl rand::Rng) -> [Direction; 6] {
    let mut directions = Direction::ALL;
    directions.shuffle(random);
    directions
}

/// Vanilla `Vec3.atCenterOf`.
fn center_of(pos: BlockPos) -> DVec3 {
    DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()) + 0.5,
        f64::from(pos.z()) + 0.5,
    )
}

/// Vanilla `CreakingHeartBlock.hasRequiredLogs`: pale oak logs along the
/// heart's axis on both ends.
pub(crate) fn has_required_logs(
    state: BlockStateId,
    world: &dyn LevelReader,
    pos: BlockPos,
) -> bool {
    let axis = state.get_value(&BlockStateProperties::AXIS);
    axis_directions(axis).into_iter().all(|direction| {
        let neighbor = world.get_block_state(pos.relative(direction));
        is_pale_oak_log(neighbor)
            && neighbor.try_get_value(&BlockStateProperties::AXIS) == Some(axis)
    })
}

/// Vanilla `Direction.Axis.getDirections`.
pub(crate) const fn axis_directions(axis: Axis) -> [Direction; 2] {
    match axis {
        Axis::X => [Direction::West, Direction::East],
        Axis::Y => [Direction::Down, Direction::Up],
        Axis::Z => [Direction::North, Direction::South],
    }
}

impl BlockEntity for CreakingHeartBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn pre_remove_side_effects(&self, _pos: BlockPos, _state: BlockStateId) {
        if let Some(world) = self.get_level() {
            self.remove_protector(&world, None);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        match nbt
            .int_array("creaking")
            .and_then(|ints| Uuid::from_int_array(&ints))
        {
            Some(uuid) => self.set_creaking_info(CreakingInfo::Uuid(uuid)),
            None => self.clear_creaking_info(),
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let uuid = match self.heart.lock().creaking_info.as_ref() {
            Some(CreakingInfo::Entity(creaking)) => Some(creaking.uuid()),
            Some(CreakingInfo::Uuid(uuid)) => Some(*uuid),
            None => None,
        };
        if let Some(uuid) = uuid {
            nbt.insert("creaking", NbtTag::IntArray(uuid.to_int_array().to_vec()));
        }
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        Some(self.save_custom_only())
    }

    fn tick(&self, world: &Arc<World>) {
        let pos = self.get_block_pos();
        let state = world.get_block_state(pos);
        if state.get_block() != &vanilla_blocks::CREAKING_HEART {
            return;
        }
        self.server_tick(world, pos, state);
    }
}
