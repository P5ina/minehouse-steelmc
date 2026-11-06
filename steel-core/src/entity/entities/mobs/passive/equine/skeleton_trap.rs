//! Vanilla `SkeletonTrapGoal`: a trap skeleton horse springs four skeleton
//! horsemen when a player comes close.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_entities, vanilla_items};
use steel_utils::Downcast as _;
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;

use super::{AbstractHorse, SkeletonHorseEntity};
use crate::entity::ai::goal::{Goal, GoalControls};
use crate::entity::entities::LightningBoltEntity;
use crate::entity::mob::spawn_equipment::reenchant_with_mob_spawn_equipment;
use crate::entity::{
    AgeableMob as _, Entity, EntitySpawnReason, PathfinderMob, SharedEntity,
    create_entity_instance, start_riding_entities,
};
use crate::world::{DifficultyInstance, World};

/// Vanilla `SkeletonTrapGoal.LIGHTNING_INVULNERABLE_TICKS`.
const LIGHTNING_INVULNERABLE_TICKS: i32 = 60;
/// Vanilla `canUse` player range.
const TRIGGER_RANGE: f64 = 10.0;
/// Extra horsemen the trap springs next to the trap horse.
const EXTRA_HORSEMEN: usize = 3;
/// Vanilla `triangle(0.0, 1.1485)` spread of the extra horses' push.
const HORSE_PUSH_DEVIATION: f64 = 1.1485;

/// Vanilla `SkeletonTrapGoal`. Steel keeps it registered for every skeleton
/// horse and gates it on `isTrap`, which matches vanilla adding and removing
/// it in `setTrap`: the goal has no controls, so it never blocks others.
pub(super) struct SkeletonTrapGoal;

impl SkeletonTrapGoal {
    pub(super) const fn new() -> Self {
        Self
    }
}

fn trap_horse(mob: &dyn PathfinderMob) -> Option<&SkeletonHorseEntity> {
    mob.downcast_ref::<SkeletonHorseEntity>()
        .filter(|horse| horse.is_trap())
}

impl Goal for SkeletonTrapGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    /// Vanilla `canUse`: `level.hasNearbyAlivePlayer(x, y, z, 10.0)`.
    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if trap_horse(mob).is_none() {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        world
            .nearest_player(mob.position(), TRIGGER_RANGE, |player| {
                !player.is_spectator() && Entity::is_alive(player)
            })
            .is_some()
    }

    /// Vanilla `tick`.
    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(horse) = trap_horse(mob) else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        let difficulty = world.current_difficulty_at(mob.block_position());
        horse.set_trap(false);
        horse.set_tamed(true);
        horse.set_age(0);

        let position = mob.position();
        let Ok(bolt) = create_entity_instance(&world, &vanilla_entities::LIGHTNING_BOLT, position)
        else {
            return;
        };
        bolt.snap_to(position, 0.0, 0.0);
        if let Some(bolt) = bolt.downcast_ref::<LightningBoltEntity>() {
            bolt.set_visual_only(true);
        }
        if world.try_add_entity(bolt).is_err() {
            return;
        }

        let Some(skeleton) = create_skeleton(&world, difficulty, position) else {
            return;
        };
        if world.try_add_entity(Arc::clone(&skeleton)).is_err() {
            return;
        }
        let _ = start_riding_entities(&skeleton, entity);

        let mut random = LegacyRandom::from_seed(rand::random());
        for _ in 0..EXTRA_HORSEMEN {
            let Some(other_horse) = create_horse(&world, difficulty, position) else {
                continue;
            };
            let Some(other_skeleton) = create_skeleton(&world, difficulty, position) else {
                continue;
            };
            other_horse.push_impulse(DVec3::new(
                random.triangle(0.0, HORSE_PUSH_DEVIATION),
                0.0,
                random.triangle(0.0, HORSE_PUSH_DEVIATION),
            ));
            if world.try_add_entity(Arc::clone(&other_horse)).is_err() {
                continue;
            }
            if world.try_add_entity(Arc::clone(&other_skeleton)).is_ok() {
                let _ = start_riding_entities(&other_skeleton, &other_horse);
            }
        }
    }
}

/// Vanilla `SkeletonTrapGoal.createHorse`.
fn create_horse(
    world: &Arc<World>,
    _difficulty: DifficultyInstance,
    position: DVec3,
) -> Option<SharedEntity> {
    let horse = create_entity_instance(world, &vanilla_entities::SKELETON_HORSE, position).ok()?;
    if let Some(mob) = horse.as_mob() {
        let _ = mob.finalize_spawn(world, EntitySpawnReason::Triggered, None);
        mob.set_persistence_required();
    }
    if let Some(living) = horse.as_living_entity() {
        living
            .living_base()
            .set_invulnerable_time(LIGHTNING_INVULNERABLE_TICKS);
    }
    if let Some(skeleton_horse) = horse.downcast_ref::<SkeletonHorseEntity>() {
        skeleton_horse.set_tamed(true);
        skeleton_horse.set_age(0);
    }
    Some(horse)
}

/// Vanilla `SkeletonTrapGoal.createSkeleton`.
fn create_skeleton(
    world: &Arc<World>,
    difficulty: DifficultyInstance,
    position: DVec3,
) -> Option<SharedEntity> {
    let skeleton = create_entity_instance(world, &vanilla_entities::SKELETON, position).ok()?;
    let mob = skeleton.as_mob()?;
    let _ = mob.finalize_spawn(world, EntitySpawnReason::Triggered, None);
    mob.living_base()
        .set_invulnerable_time(LIGHTNING_INVULNERABLE_TICKS);
    mob.set_persistence_required();
    {
        let mut equipment = mob.living_base().equipment().lock();
        if equipment.get_ref(EquipmentSlot::Head).is_empty() {
            let _ = equipment.set(
                EquipmentSlot::Head,
                ItemStack::new(&vanilla_items::IRON_HELMET),
            );
        }
    }
    let mut random = LegacyRandom::from_seed(rand::random());
    reenchant_with_mob_spawn_equipment(mob, EquipmentSlot::MainHand, &mut random, difficulty);
    reenchant_with_mob_spawn_equipment(mob, EquipmentSlot::Head, &mut random, difficulty);
    Some(skeleton)
}
