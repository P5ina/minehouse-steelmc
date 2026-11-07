//! Vanilla melee fight behaviors: `StopAttackingIfTargetInvalid`,
//! `SetWalkTargetFromAttackTargetIfTargetOutOfReach`, `MeleeAttack` and
//! `EraseMemoryIf`.

use std::sync::Arc;

use steel_registry::equipment::EquipmentSlot;
use steel_registry::vanilla_items;
use steel_utils::types::InteractionHand;

use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{AnyMemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    ATTACK_COOLING_DOWN, ATTACK_TARGET, BREED_TARGET, CANT_REACH_WALK_TARGET_SINCE, LOOK_TARGET,
    NEAREST_VISIBLE_LIVING_ENTITIES, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::{LivingEntity, Mob, SharedEntity};
use crate::world::World;

/// Vanilla `StopAttackingIfTargetInvalid.TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE`.
const TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE: i64 = 200;
/// Vanilla `SetWalkTargetFromAttackTargetIfTargetOutOfReach.PROJECTILE_ATTACK_RANGE_BUFFER`.
const PROJECTILE_ATTACK_RANGE_BUFFER: i32 = 1;
/// Vanilla `BowItem.getDefaultProjectileRange`.
const BOW_PROJECTILE_RANGE: i32 = 15;
/// Vanilla `CrossbowItem.getDefaultProjectileRange`.
const CROSSBOW_PROJECTILE_RANGE: i32 = 8;

/// Vanilla `BehaviorUtils.isBreeding`.
pub fn is_breeding(body: &(impl LivingEntity + ?Sized)) -> bool {
    body.brain_memories()
        .is_some_and(|memories| memories.lock().has_value(&BREED_TARGET))
}

/// Vanilla `BehaviorUtils.isWithinAttackRange`: a usable projectile weapon in
/// the main hand gives its range less `margin`, anything else melee range.
pub fn is_within_attack_range(
    body: &(impl Mob + ?Sized),
    target: &dyn LivingEntity,
    margin: i32,
) -> bool {
    let mut projectile_range = None;
    body.with_equipment_slot(EquipmentSlot::MainHand, &mut |main_hand| {
        let range = if main_hand.is(&vanilla_items::BOW) {
            Some(BOW_PROJECTILE_RANGE)
        } else if main_hand.is(&vanilla_items::CROSSBOW) {
            Some(CROSSBOW_PROJECTILE_RANGE)
        } else {
            None
        };
        projectile_range = range.filter(|_| body.can_use_non_melee_weapon(main_hand));
    });
    match projectile_range {
        Some(range) => {
            let max_allowed_distance = f64::from(range - margin);
            body.position().distance_squared(target.position())
                < max_allowed_distance * max_allowed_distance
        }
        _ => body.is_within_melee_attack_range(target),
    }
}

fn attack_target_of(body: &(impl LivingEntity + ?Sized)) -> Option<SharedEntity> {
    body.brain_memories()?.lock().get(&ATTACK_TARGET)
}

/// Drops the attack target once it is dead, gone to another level, no longer
/// attackable, or out of reach for too long (vanilla
/// `StopAttackingIfTargetInvalid.create(onTargetErased)`).
pub fn stop_attacking_if_target_invalid<E: Mob + ?Sized>(
    mut on_target_erased: impl FnMut(&Arc<World>, &E, &SharedEntity) + Send + 'static,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&CANT_REACH_WALK_TARGET_SINCE, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let Some(target) = attack_target_of(body) else {
                return false;
            };
            let cant_reach_since = memories.lock().get(&CANT_REACH_WALK_TARGET_SINCE);
            let tired = cant_reach_since.is_some_and(|since| {
                world.game_time() - since > TIMEOUT_TO_GET_WITHIN_ATTACK_RANGE
            });
            let same_level = match (target.level(), body.level()) {
                (Some(target_level), Some(body_level)) => Arc::ptr_eq(&target_level, &body_level),
                _ => false,
            };
            let valid = target.as_living_entity().is_some_and(|living| {
                Mob::can_attack(body, living) && !tired && LivingEntity::is_alive(living)
            }) && same_level;
            if !valid {
                on_target_erased(world, body, &target);
                memories.lock().erase(&ATTACK_TARGET);
            }
            true
        },
    )
}

/// Walks toward the attack target until it is visible and within attack
/// range (vanilla `SetWalkTargetFromAttackTargetIfTargetOutOfReach`).
pub fn set_walk_target_from_attack_target_if_target_out_of_reach<E: Mob + ?Sized>(
    speed_modifier: impl Fn(&E) -> f32 + Send + 'static,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let Some(target) = attack_target_of(body) else {
                return false;
            };
            let Some(living) = target.as_living_entity() else {
                return false;
            };
            // Clone the memory out first: the lazy visibility test locks the
            // body's memories again.
            let nearest_visible = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES);
            let visible = nearest_visible.is_some_and(|entities| entities.contains(target.id()));
            if visible && is_within_attack_range(body, living, PROJECTILE_ATTACK_RANGE_BUFFER) {
                memories.lock().erase(&WALK_TARGET);
            } else {
                let walk_target = WalkTarget::new(
                    PositionTracker::entity(Arc::clone(&target), false),
                    speed_modifier(body),
                    0,
                );
                let mut memories = memories.lock();
                memories.set(&LOOK_TARGET, Some(PositionTracker::entity(target, true)));
                memories.set(&WALK_TARGET, Some(walk_target));
            }
            true
        },
    )
}

/// Hits a visible attack target in melee range, then cools down for
/// `cooldown_between_attacks` ticks (vanilla `MeleeAttack.create`).
pub fn melee_attack<E: Mob + ?Sized>(cooldown_between_attacks: i32) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&ATTACK_TARGET, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&ATTACK_COOLING_DOWN, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let Some(target) = attack_target_of(body) else {
                return false;
            };
            let Some(living) = target.as_living_entity() else {
                return false;
            };
            let holding_usable_non_melee_weapon =
                body.is_holding(&mut |item| body.can_use_non_melee_weapon(item));
            // Clone the memory out first: the lazy visibility test locks the
            // body's memories again.
            let nearest_visible = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES);
            let visible = nearest_visible.is_some_and(|entities| entities.contains(target.id()));
            if holding_usable_non_melee_weapon
                || !body.is_within_melee_attack_range(living)
                || !visible
            {
                return false;
            }
            let Some(attacker) = world.get_entity_by_id(body.id()) else {
                return false;
            };
            memories.lock().set(
                &LOOK_TARGET,
                Some(PositionTracker::entity(Arc::clone(&target), true)),
            );
            body.swing_for_attack(InteractionHand::MainHand);
            let _ = body.do_hurt_target(&attacker, world, &target);
            memories.lock().set_with_expiry(
                &ATTACK_COOLING_DOWN,
                true,
                i64::from(cooldown_between_attacks),
            );
            true
        },
    )
}

/// Erases `memory` whenever `predicate` holds (vanilla `EraseMemoryIf`).
pub fn erase_memory_if<E: LivingEntity + ?Sized>(
    predicate: impl Fn(&E) -> bool + Send + 'static,
    memory: &'static dyn AnyMemoryModuleType,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(memory, MemoryStatus::ValuePresent)],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if !memories.lock().has_value(memory) || !predicate(body) {
                return false;
            }
            memories.lock().erase(memory);
            true
        },
    )
}
