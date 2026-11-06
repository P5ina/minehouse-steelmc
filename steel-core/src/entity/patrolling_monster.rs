//! Monsters that roam in patrols (Vanilla `PatrollingMonster`).
//!
//! Patrol fields live in [`PatrollingMonsterState`]; the long-distance patrol goal is
//! [`LongDistancePatrolGoal`](crate::entity::ai::goal::LongDistancePatrolGoal).

use std::sync::atomic::{AtomicBool, Ordering};

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::entity_type::EntityTypeRef;
use steel_utils::BlockPos;
use steel_utils::locks::SyncMutex;

use crate::chunk::light::LightLayer;
use crate::entity::{EntitySpawnReason, Monster, ominous_banner_instance};
use crate::inventory::prelude::EquipmentSlot;
use crate::world::ServerLevelAccessor;

const TAG_PATROL_TARGET: &str = "patrol_target";
const TAG_PATROL_LEADER: &str = "PatrolLeader";
const TAG_PATROLLING: &str = "Patrolling";
/// Vanilla `finalizeSpawn`: chance for a naturally spawned patroller to lead.
const PATROL_LEADER_CHANCE: f32 = 0.06;
/// Vanilla `checkPatrollingMonsterSpawnRules` block light limit.
const PATROL_SPAWN_MAX_BLOCK_LIGHT: u8 = 8;
/// Vanilla `removeWhenFarAway`: a patrolling monster only despawns beyond 128 blocks.
const PATROLLING_DESPAWN_DISTANCE_SQR: f64 = 16384.0;
/// Vanilla `findPatrolTarget` picks a point up to 500 blocks away on each axis.
const PATROL_TARGET_SPREAD: i32 = 500;
/// Vanilla `setDropChance(EquipmentSlot.HEAD, 2.0F)` for the leader's banner.
pub(crate) const GUARANTEED_BANNER_DROP_CHANCE: f32 = 2.0;

/// Patrol fields of a patrolling monster.
pub struct PatrollingMonsterState {
    patrol_target: SyncMutex<Option<BlockPos>>,
    patrol_leader: AtomicBool,
    patrolling: AtomicBool,
}

impl PatrollingMonsterState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            patrol_target: SyncMutex::new(None),
            patrol_leader: AtomicBool::new(false),
            patrolling: AtomicBool::new(false),
        }
    }
}

impl Default for PatrollingMonsterState {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `PatrollingMonster`.
pub trait PatrollingMonster: Monster {
    /// Vanilla `PatrollingMonster.checkPatrollingMonsterSpawnRules`: no spawning
    /// in block light above 8, otherwise `checkAnyLightMonsterSpawnRules`.
    fn check_patrolling_monster_spawn_rules(
        entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
    ) -> bool
    where
        Self: Sized,
    {
        level.brightness(LightLayer::Block, pos) <= PATROL_SPAWN_MAX_BLOCK_LIGHT
            && <Self as Monster>::check_any_light_monster_spawn_rules(
                entity_type,
                level,
                spawn_reason,
                pos,
            )
    }

    fn patrolling_monster_state(&self) -> &PatrollingMonsterState;

    /// Vanilla `canBeLeader`.
    fn can_be_leader(&self) -> bool {
        true
    }

    /// Vanilla `canJoinPatrol`.
    fn can_join_patrol(&self) -> bool {
        true
    }

    fn patrol_target(&self) -> Option<BlockPos> {
        *self.patrolling_monster_state().patrol_target.lock()
    }

    fn has_patrol_target(&self) -> bool {
        self.patrol_target().is_some()
    }

    /// Vanilla `setPatrolTarget`: also marks the monster as patrolling.
    fn set_patrol_target(&self, target: BlockPos) {
        *self.patrolling_monster_state().patrol_target.lock() = Some(target);
        self.set_patrolling(true);
    }

    fn is_patrol_leader(&self) -> bool {
        self.patrolling_monster_state()
            .patrol_leader
            .load(Ordering::Relaxed)
    }

    /// Vanilla `setPatrolLeader`: also marks the monster as patrolling.
    fn set_patrol_leader(&self, leader: bool) {
        self.patrolling_monster_state()
            .patrol_leader
            .store(leader, Ordering::Relaxed);
        self.set_patrolling(true);
    }

    fn is_patrolling(&self) -> bool {
        self.patrolling_monster_state()
            .patrolling
            .load(Ordering::Relaxed)
    }

    fn set_patrolling(&self, patrolling: bool) {
        self.patrolling_monster_state()
            .patrolling
            .store(patrolling, Ordering::Relaxed);
    }

    /// Vanilla `findPatrolTarget`.
    fn find_patrol_target(&self) {
        let target = self.block_position().offset(
            -PATROL_TARGET_SPREAD + rand::random_range(0..PATROL_TARGET_SPREAD * 2),
            0,
            -PATROL_TARGET_SPREAD + rand::random_range(0..PATROL_TARGET_SPREAD * 2),
        );
        self.set_patrol_target(target);
    }

    /// Vanilla `removeWhenFarAway`.
    fn remove_when_far_away_patrolling(&self, dist_sqr: f64) -> bool {
        !self.is_patrolling() || dist_sqr > PATROLLING_DESPAWN_DISTANCE_SQR
    }

    /// The patrol part of Vanilla `finalizeSpawn`, run before the mob's own.
    fn finalize_spawn_patrolling(&self, spawn_reason: EntitySpawnReason) {
        if !matches!(
            spawn_reason,
            EntitySpawnReason::Patrol | EntitySpawnReason::Event | EntitySpawnReason::Structure
        ) && rand::random::<f32>() < PATROL_LEADER_CHANCE
            && self.can_be_leader()
        {
            self.patrolling_monster_state()
                .patrol_leader
                .store(true, Ordering::Relaxed);
        }

        if self.is_patrol_leader() {
            self.living_base()
                .equipment()
                .lock()
                .set(EquipmentSlot::Head, ominous_banner_instance());
            self.set_equipment_drop_chance(EquipmentSlot::Head, GUARANTEED_BANNER_DROP_CHANCE);
        }

        if spawn_reason == EntitySpawnReason::Patrol {
            self.set_patrolling(true);
        }
    }

    fn save_patrolling(&self, nbt: &mut NbtCompound) {
        if let Some(target) = self.patrol_target() {
            nbt.insert(
                TAG_PATROL_TARGET,
                NbtTag::IntArray(vec![target.x(), target.y(), target.z()]),
            );
        }
        nbt.insert(TAG_PATROL_LEADER, i8::from(self.is_patrol_leader()));
        nbt.insert(TAG_PATROLLING, i8::from(self.is_patrolling()));
    }

    fn load_patrolling(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let state = self.patrolling_monster_state();
        *state.patrol_target.lock() =
            nbt.int_array(TAG_PATROL_TARGET)
                .and_then(|pos| match pos.as_slice() {
                    [x, y, z] => Some(BlockPos::new(*x, *y, *z)),
                    _ => None,
                });
        state.patrol_leader.store(
            nbt.byte(TAG_PATROL_LEADER).is_some_and(|b| b != 0),
            Ordering::Relaxed,
        );
        state.patrolling.store(
            nbt.byte(TAG_PATROLLING).is_some_and(|b| b != 0),
            Ordering::Relaxed,
        );
    }
}
