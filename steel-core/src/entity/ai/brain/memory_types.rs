//! Vanilla memory types (`MemoryModuleType` constants) and the values they hold.
//!
//! Only memories with a Steel user are listed; add more as brains need them.

use glam::DVec3;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_utils::{BlockPos, DowncastTypeKey, GlobalPos, Identifier, UuidExt as _};
use uuid::Uuid;

use super::memory::{MemoryCodec, MemoryModuleType, MemoryValue};
use crate::entity::ai::path::Path;
use crate::entity::damage::RecentDamageSource;
use crate::entity::{LivingEntity, SharedEntity};

impl MemoryValue for () {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/unit");
}

impl MemoryValue for bool {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/bool");
}

impl MemoryValue for i32 {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/i32");
}

impl MemoryValue for i64 {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/i64");
}

impl MemoryValue for Uuid {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/uuid");
}

impl MemoryValue for BlockPos {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/block_pos");
}

impl MemoryValue for DVec3 {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/vec3");
}

impl MemoryValue for SharedEntity {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/entity");
}

impl MemoryValue for Vec<SharedEntity> {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/entity_list");

    fn is_empty_collection(&self) -> bool {
        self.is_empty()
    }
}

impl MemoryValue for Vec<GlobalPos> {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/global_pos_list");

    fn is_empty_collection(&self) -> bool {
        self.is_empty()
    }
}

impl MemoryValue for Vec<Uuid> {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/uuid_list");

    fn is_empty_collection(&self) -> bool {
        self.is_empty()
    }
}

impl MemoryValue for RecentDamageSource {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/damage_source");
}

/// A position a brain looks at or walks to (vanilla `PositionTracker`).
#[derive(Clone)]
pub enum PositionTracker {
    /// A fixed point (vanilla `BlockPosTracker`).
    Block { block_pos: BlockPos, center: DVec3 },
    /// An entity's current position (vanilla `EntityTracker`).
    Entity {
        entity: SharedEntity,
        track_eye_height: bool,
        target_eye_height: bool,
    },
}

impl PositionTracker {
    /// Tracks the center of `block_pos`.
    #[must_use]
    pub fn block(block_pos: BlockPos) -> Self {
        let (x, y, z) = block_pos.get_center();
        Self::Block {
            block_pos,
            center: DVec3::new(x, y, z),
        }
    }

    /// Tracks the exact point `position`.
    #[must_use]
    pub const fn point(position: DVec3) -> Self {
        Self::Block {
            block_pos: BlockPos::containing(position.x, position.y, position.z),
            center: position,
        }
    }

    /// Tracks `entity`, at its eyes when `track_eye_height`.
    #[must_use]
    pub const fn entity(entity: SharedEntity, track_eye_height: bool) -> Self {
        Self::Entity {
            entity,
            track_eye_height,
            target_eye_height: false,
        }
    }

    /// Vanilla `PositionTracker.currentPosition`.
    #[must_use]
    pub fn current_position(&self) -> DVec3 {
        match self {
            Self::Block { center, .. } => *center,
            Self::Entity {
                entity,
                track_eye_height,
                ..
            } => {
                let position = entity.position();
                if *track_eye_height {
                    position + DVec3::Y * entity.get_eye_height()
                } else {
                    position
                }
            }
        }
    }

    /// Vanilla `PositionTracker.currentBlockPosition`.
    #[must_use]
    pub fn current_block_position(&self) -> BlockPos {
        match self {
            Self::Block { block_pos, .. } => *block_pos,
            Self::Entity {
                entity,
                target_eye_height,
                ..
            } => {
                if *target_eye_height {
                    let position = entity.position();
                    BlockPos::containing(
                        position.x,
                        position.y + entity.get_eye_height(),
                        position.z,
                    )
                } else {
                    entity.block_position()
                }
            }
        }
    }

    /// The tracked entity, if this tracks one.
    #[must_use]
    pub const fn tracked_entity(&self) -> Option<&SharedEntity> {
        match self {
            Self::Block { .. } => None,
            Self::Entity { entity, .. } => Some(entity),
        }
    }
}

impl MemoryValue for PositionTracker {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/position_tracker");
}

/// Where a brain wants to walk (vanilla `WalkTarget`).
#[derive(Clone)]
pub struct WalkTarget {
    pub target: PositionTracker,
    pub speed_modifier: f32,
    pub close_enough_dist: i32,
}

impl WalkTarget {
    #[must_use]
    pub const fn new(target: PositionTracker, speed_modifier: f32, close_enough_dist: i32) -> Self {
        Self {
            target,
            speed_modifier,
            close_enough_dist,
        }
    }

    /// Walks to the block containing `position`.
    #[must_use]
    pub fn to_point(position: DVec3, speed_modifier: f32, close_enough_dist: i32) -> Self {
        Self::new(
            PositionTracker::block(BlockPos::containing(position.x, position.y, position.z)),
            speed_modifier,
            close_enough_dist,
        )
    }
}

impl MemoryValue for WalkTarget {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/walk_target");
}

const fn int_codec() -> MemoryCodec<i32> {
    MemoryCodec {
        save: |value| NbtTag::Int(*value),
        load: |tag| tag.int(),
    }
}

const fn long_codec() -> MemoryCodec<i64> {
    MemoryCodec {
        save: |value| NbtTag::Long(*value),
        load: |tag| tag.long(),
    }
}

const fn bool_codec() -> MemoryCodec<bool> {
    MemoryCodec {
        save: |value| NbtTag::Byte(i8::from(*value)),
        load: |tag| tag.byte().map(|value| value != 0),
    }
}

/// Vanilla `Unit.CODEC` writes an empty compound.
const fn unit_codec() -> MemoryCodec<()> {
    MemoryCodec {
        save: |()| NbtTag::Compound(NbtCompound::new()),
        load: |tag| tag.compound().map(|_| ()),
    }
}

/// Vanilla `Codec.list(GlobalPos.CODEC)`: compounds of `dimension` and an
/// `[x, y, z]` int array `pos`; entries that fail to decode are skipped.
const fn global_pos_list_codec() -> MemoryCodec<Vec<GlobalPos>> {
    MemoryCodec {
        save: |positions| {
            NbtTag::List(NbtList::Compound(
                positions
                    .iter()
                    .map(|global| {
                        let mut compound = NbtCompound::new();
                        compound.insert("dimension", global.dimension.to_string());
                        compound.insert(
                            "pos",
                            NbtTag::IntArray(vec![global.pos.x(), global.pos.y(), global.pos.z()]),
                        );
                        compound
                    })
                    .collect(),
            ))
        },
        load: |tag| {
            let compounds = tag.list()?.compounds()?;
            Some(
                compounds
                    .into_iter()
                    .filter_map(|compound| {
                        let dimension = compound
                            .string("dimension")?
                            .to_str()
                            .parse::<Identifier>()
                            .ok()?;
                        let pos = match compound.int_array("pos")?.as_slice() {
                            [x, y, z] => BlockPos::new(*x, *y, *z),
                            _ => return None,
                        };
                        Some(GlobalPos::new(dimension, pos))
                    })
                    .collect(),
            )
        },
    }
}

const fn uuid_codec() -> MemoryCodec<Uuid> {
    MemoryCodec {
        save: |value| NbtTag::IntArray(value.to_int_array().to_vec()),
        load: |tag| tag.int_array().and_then(|ints| Uuid::from_int_array(&ints)),
    }
}

pub static NEAREST_LIVING_ENTITIES: MemoryModuleType<Vec<SharedEntity>> =
    MemoryModuleType::new("mobs");
pub static NEAREST_PLAYERS: MemoryModuleType<Vec<SharedEntity>> =
    MemoryModuleType::new("nearest_players");
pub static NEAREST_VISIBLE_PLAYER: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_visible_player");
pub static NEAREST_VISIBLE_ATTACKABLE_PLAYER: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_visible_targetable_player");
pub static WALK_TARGET: MemoryModuleType<WalkTarget> = MemoryModuleType::new("walk_target");
pub static LOOK_TARGET: MemoryModuleType<PositionTracker> = MemoryModuleType::new("look_target");
pub static ATTACK_TARGET: MemoryModuleType<SharedEntity> = MemoryModuleType::new("attack_target");
pub static ATTACK_COOLING_DOWN: MemoryModuleType<bool> =
    MemoryModuleType::new("attack_cooling_down");
pub static INTERACTION_TARGET: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("interaction_target");
pub static BREED_TARGET: MemoryModuleType<SharedEntity> = MemoryModuleType::new("breed_target");
pub static RIDE_TARGET: MemoryModuleType<SharedEntity> = MemoryModuleType::new("ride_target");
pub static HURT_BY: MemoryModuleType<RecentDamageSource> = MemoryModuleType::new("hurt_by");
pub static HURT_BY_ENTITY: MemoryModuleType<SharedEntity> = MemoryModuleType::new("hurt_by_entity");
pub static AVOID_TARGET: MemoryModuleType<SharedEntity> = MemoryModuleType::new("avoid_target");
pub static NEAREST_HOSTILE: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_hostile");
pub static NEAREST_ATTACKABLE: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_attackable");
pub static CANT_REACH_WALK_TARGET_SINCE: MemoryModuleType<i64> =
    MemoryModuleType::new("cant_reach_walk_target_since");
pub static DANGER_DETECTED_RECENTLY: MemoryModuleType<bool> =
    MemoryModuleType::with_codec("danger_detected_recently", bool_codec());
pub static NEAREST_VISIBLE_ADULT: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_visible_adult");
pub static NEAREST_VISIBLE_WANTED_ITEM: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("nearest_visible_wanted_item");
pub static PLAY_DEAD_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("play_dead_ticks", int_codec());
pub static TEMPTING_PLAYER: MemoryModuleType<SharedEntity> =
    MemoryModuleType::new("tempting_player");
pub static TEMPTATION_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("temptation_cooldown_ticks", int_codec());
pub static GAZE_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("gaze_cooldown_ticks", int_codec());
pub static LONG_JUMP_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("long_jump_cooling_down", int_codec());
pub static LONG_JUMP_MID_JUMP: MemoryModuleType<bool> = MemoryModuleType::new("long_jump_mid_jump");
pub static HAS_HUNTING_COOLDOWN: MemoryModuleType<bool> =
    MemoryModuleType::with_codec("has_hunting_cooldown", bool_codec());
pub static RAM_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("ram_cooldown_ticks", int_codec());
pub static RAM_TARGET: MemoryModuleType<DVec3> = MemoryModuleType::new("ram_target");
pub static IS_IN_WATER: MemoryModuleType<()> =
    MemoryModuleType::with_codec("is_in_water", unit_codec());
pub static IS_PREGNANT: MemoryModuleType<()> =
    MemoryModuleType::with_codec("is_pregnant", unit_codec());
pub static IS_PANICKING: MemoryModuleType<bool> =
    MemoryModuleType::with_codec("is_panicking", bool_codec());
pub static ANGRY_AT: MemoryModuleType<Uuid> =
    MemoryModuleType::with_codec("angry_at", uuid_codec());
pub static UNIVERSAL_ANGER: MemoryModuleType<bool> =
    MemoryModuleType::with_codec("universal_anger", bool_codec());
pub static LAST_SLEPT: MemoryModuleType<i64> =
    MemoryModuleType::with_codec("last_slept", long_codec());
pub static LAST_WOKEN: MemoryModuleType<i64> =
    MemoryModuleType::with_codec("last_woken", long_codec());
pub static LAST_WORKED_AT_POI: MemoryModuleType<i64> =
    MemoryModuleType::with_codec("last_worked_at_poi", long_codec());
pub static HEARD_BELL_TIME: MemoryModuleType<i64> = MemoryModuleType::new("heard_bell_time");
pub static NEAREST_BED: MemoryModuleType<BlockPos> = MemoryModuleType::new("nearest_bed");
pub static ITEM_PICKUP_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("item_pickup_cooldown_ticks", int_codec());

/// Whether `entity` is the entity stored in `memory` (vanilla `Brain.isMemoryValue`
/// for entity memories, which compares identity).
#[must_use]
pub fn is_entity_memory(
    body: &(impl LivingEntity + ?Sized),
    memory: &MemoryModuleType<SharedEntity>,
    entity_id: i32,
) -> bool {
    body.brain_memories().is_some_and(|memories| {
        memories
            .lock()
            .get_ref(memory)
            .is_some_and(|stored| stored.id() == entity_id)
    })
}

pub static NEAREST_VISIBLE_LIVING_ENTITIES: MemoryModuleType<
    super::nearest_visible::NearestVisibleLivingEntities,
> = MemoryModuleType::new("visible_mobs");

impl MemoryValue for Path {
    const KEY: DowncastTypeKey = DowncastTypeKey::new("steel:brain_memory/path");
}

pub static PATH: MemoryModuleType<Path> = MemoryModuleType::new("path");

pub static NEAREST_VISIBLE_ATTACKABLE_PLAYERS: MemoryModuleType<Vec<SharedEntity>> =
    MemoryModuleType::new("nearest_visible_targetable_players");
pub static CHARGE_COOLDOWN_TICKS: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("charge_cooldown_ticks", int_codec());
pub static ATTACK_TARGET_COOLDOWN: MemoryModuleType<i32> =
    MemoryModuleType::with_codec("attack_target_cooldown", int_codec());
pub static VISIBLE_VILLAGER_BABIES: MemoryModuleType<Vec<SharedEntity>> =
    MemoryModuleType::new("visible_villager_babies");
pub static GOLEM_DETECTED_RECENTLY: MemoryModuleType<bool> =
    MemoryModuleType::with_codec("golem_detected_recently", bool_codec());
pub static SNIFF_COOLDOWN: MemoryModuleType<()> =
    MemoryModuleType::with_codec("sniff_cooldown", unit_codec());
pub static SNIFFER_EXPLORED_POSITIONS: MemoryModuleType<Vec<GlobalPos>> =
    MemoryModuleType::with_codec("sniffer_explored_positions", global_pos_list_codec());
pub static SNIFFER_SNIFFING_TARGET: MemoryModuleType<BlockPos> =
    MemoryModuleType::new("sniffer_sniffing_target");
pub static SNIFFER_DIGGING: MemoryModuleType<bool> = MemoryModuleType::new("sniffer_digging");
pub static SNIFFER_HAPPY: MemoryModuleType<bool> = MemoryModuleType::new("sniffer_happy");
pub static UNREACHABLE_TONGUE_TARGETS: MemoryModuleType<Vec<Uuid>> =
    MemoryModuleType::new("unreachable_tongue_targets");
