//! Vanilla item-carrying behaviors: `GoToWantedItem`, `GoAndGiveItemsToTarget`
//! and `StayCloseToTarget`.

use std::sync::Arc;

use glam::DVec3;

use super::utils::set_walk_and_look_target_memories;
use crate::entity::ai::brain::behavior::{Behavior, OneShot};
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    ITEM_PICKUP_COOLDOWN_TICKS, LOOK_TARGET, NEAREST_VISIBLE_WANTED_ITEM, PositionTracker,
    WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::{LivingEntity, Mob};
use crate::world::World;

/// Vanilla `GoAndGiveItemsToTarget.CLOSE_ENOUGH_DISTANCE_TO_TARGET`.
const CLOSE_ENOUGH_DISTANCE_TO_TARGET: i32 = 3;

/// Where a behavior should walk to, if anywhere.
pub type TargetPositionGetter<E> = fn(&E) -> Option<PositionTracker>;

/// Walks to the nearest wanted item (vanilla `GoToWantedItem`).
pub fn go_to_wanted_item<E: Mob + ?Sized>(
    predicate: impl Fn(&E) -> bool + Send + 'static,
    speed_modifier: f32,
    interrupt_ongoing_walk: bool,
    max_dist_to_walk: i32,
) -> impl BehaviorControl<E> {
    let walk_status = if interrupt_ongoing_walk {
        MemoryStatus::Registered
    } else {
        MemoryStatus::ValueAbsent
    };
    let max_dist_sqr = f64::from(max_dist_to_walk) * f64::from(max_dist_to_walk);
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&WALK_TARGET, walk_status),
            MemoryRequirement::new(&NEAREST_VISIBLE_WANTED_ITEM, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&ITEM_PICKUP_COOLDOWN_TICKS, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let item = {
                let memories = memories.lock();
                if !interrupt_ongoing_walk && memories.has_value(&WALK_TARGET) {
                    return false;
                }
                if memories.has_value(&ITEM_PICKUP_COOLDOWN_TICKS) {
                    return false;
                }
                let Some(item) = memories.get(&NEAREST_VISIBLE_WANTED_ITEM) else {
                    return false;
                };
                item
            };
            if !predicate(body)
                || item.position().distance_squared(body.position()) >= max_dist_sqr
                || !world
                    .world_border_snapshot()
                    .is_block_within_bounds(item.block_position())
                || !body.can_pick_up_loot()
            {
                return false;
            }
            let mut memories = memories.lock();
            memories.set(
                &LOOK_TARGET,
                Some(PositionTracker::entity(Arc::clone(&item), true)),
            );
            memories.set(
                &WALK_TARGET,
                Some(WalkTarget::new(
                    PositionTracker::entity(item, false),
                    speed_modifier,
                    0,
                )),
            );
            true
        },
    )
}

/// Throws an item the carrier holds toward `target`.
pub type ItemThrower<E> = fn(&Arc<World>, &E, DVec3);

/// Carries items to a target and throws them there
/// (vanilla `GoAndGiveItemsToTarget`).
pub struct GoAndGiveItemsToTarget<E: ?Sized> {
    target_position_getter: TargetPositionGetter<E>,
    speed_modifier: f32,
    timeout_duration: i32,
    item_thrower: ItemThrower<E>,
    cooldown_memory: &'static MemoryModuleType<i32>,
    cooldown_duration: i32,
    has_item_predicate: fn(&E) -> bool,
    entry_condition: [MemoryRequirement; 3],
}

impl<E: ?Sized> GoAndGiveItemsToTarget<E> {
    #[must_use]
    pub fn new(
        target_position_getter: TargetPositionGetter<E>,
        speed_modifier: f32,
        timeout_duration: i32,
        item_thrower: ItemThrower<E>,
        cooldown_memory: &'static MemoryModuleType<i32>,
        cooldown_duration: i32,
        has_item_predicate: fn(&E) -> bool,
    ) -> Self {
        Self {
            target_position_getter,
            speed_modifier,
            timeout_duration,
            item_thrower,
            cooldown_memory,
            cooldown_duration,
            has_item_predicate,
            entry_condition: [
                MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
                MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
                MemoryRequirement::new(cooldown_memory, MemoryStatus::Registered),
            ],
        }
    }

    fn can_throw_item_to_target(&self, body: &E) -> bool {
        (self.has_item_predicate)(body) && (self.target_position_getter)(body).is_some()
    }
}

impl<E: LivingEntity + ?Sized> Behavior<E> for GoAndGiveItemsToTarget<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &self.entry_condition
    }

    fn duration(&self) -> (i32, i32) {
        (self.timeout_duration, self.timeout_duration)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        self.can_throw_item_to_target(body)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        self.can_throw_item_to_target(body)
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(target) = (self.target_position_getter)(body) {
            set_walk_and_look_target_memories(
                body,
                target,
                self.speed_modifier,
                CLOSE_ENOUGH_DISTANCE_TO_TARGET,
            );
        }
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, _time: i64) {
        let Some(target) = (self.target_position_getter)(body) else {
            return;
        };
        let deposit_position = target.current_position();
        if deposit_position.distance(DVec3::new(
            body.position().x,
            body.get_eye_y(),
            body.position().z,
        )) < f64::from(CLOSE_ENOUGH_DISTANCE_TO_TARGET)
        {
            (self.item_thrower)(world, body, deposit_position);
            if let Some(memories) = body.brain_memories() {
                memories
                    .lock()
                    .set(self.cooldown_memory, Some(self.cooldown_duration));
            }
        }
    }
}

/// Walks toward a target once it is `too_far` away (vanilla `StayCloseToTarget`).
pub fn stay_close_to_target<E: LivingEntity + ?Sized>(
    target_position_getter: TargetPositionGetter<E>,
    should_run_predicate: fn(&E) -> bool,
    close_enough: i32,
    too_far: i32,
    speed_modifier: f32,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(target) = target_position_getter(body) else {
                return false;
            };
            if !should_run_predicate(body) {
                return false;
            }
            let too_far = f64::from(too_far);
            if body.position().distance_squared(target.current_position()) < too_far * too_far {
                return false;
            }
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let mut memories = memories.lock();
            memories.set(&LOOK_TARGET, Some(target.clone()));
            memories.set(
                &WALK_TARGET,
                Some(WalkTarget::new(target, speed_modifier, close_enough)),
            );
            true
        },
    )
}
