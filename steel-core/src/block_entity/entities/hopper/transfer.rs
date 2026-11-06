//! Item transfer between containers, shared by hoppers and droppers.
//!
//! Containers are addressed as an ordered list so a double chest is one
//! destination (Vanilla `CompoundContainer`). Every function works on an
//! already locked [`ContainerLockGuard`].

use steel_registry::item_stack::ItemStack;
use steel_utils::Direction;

use super::HopperContainer;
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerId, ContainerLockGuard};

/// One slot of a container list.
#[derive(Clone, Copy)]
pub(crate) struct SlotRef {
    container: ContainerId,
    slot: usize,
}

/// The slots of `containers` reachable from `direction`.
///
/// A single sided container (furnace, brewing stand) limits the slots per face;
/// anything else, including a double chest, exposes every slot in order.
pub(crate) fn get_slots(
    guard: &ContainerLockGuard,
    containers: &[ContainerId],
    direction: Option<Direction>,
) -> Vec<SlotRef> {
    if let [container] = containers
        && let Some(direction) = direction
        && let Some(face) = guard
            .get(*container)
            .and_then(|locked| locked.slots_for_face(direction))
    {
        return face
            .iter()
            .map(|&slot| SlotRef {
                container: *container,
                slot,
            })
            .collect();
    }
    containers
        .iter()
        .flat_map(|&container| {
            let size = guard
                .get(container)
                .map_or(0, Container::get_container_size);
            (0..size).map(move |slot| SlotRef { container, slot })
        })
        .collect()
}

fn item_at(guard: &ContainerLockGuard, slot: SlotRef) -> ItemStack {
    guard
        .get(slot.container)
        .map_or_else(ItemStack::empty, |c| c.get_item(slot.slot).clone())
}

/// Whether every reachable slot holds a full stack.
pub(crate) fn is_full_container(
    guard: &ContainerLockGuard,
    containers: &[ContainerId],
    direction: Direction,
) -> bool {
    get_slots(guard, containers, Some(direction))
        .into_iter()
        .all(|slot| {
            let item = item_at(guard, slot);
            !item.is_empty() && item.count() >= item.max_stack_size()
        })
}

fn is_empty(guard: &ContainerLockGuard, containers: &[ContainerId]) -> bool {
    containers
        .iter()
        .all(|&container| guard.get(container).is_none_or(Container::is_empty))
}

/// Takes one item from `slot` of `source` into the single container `destination`.
///
/// Returns whether an item moved.
pub(crate) fn try_take_in_item_from_slot(
    guard: &mut ContainerLockGuard,
    destination: ContainerId,
    source: &[ContainerId],
    slot: SlotRef,
    direction: Direction,
) -> bool {
    let item = item_at(guard, slot);
    if item.is_empty() || !can_take_item_from_container(guard, destination, slot, &item, direction)
    {
        return false;
    }

    let Some(source_container) = guard.get_mut(slot.container) else {
        return false;
    };
    let taken = source_container.get_item_mut(slot.slot).split(1);
    let rest = add_item(guard, Some(slot.container), &[destination], taken, None);
    if rest.is_empty() {
        for &container in source {
            guard.set_changed(container);
        }
        return true;
    }
    if let Some(source_container) = guard.get_mut(slot.container) {
        *source_container.get_item_mut(slot.slot) = item;
    }
    false
}

fn can_take_item_from_container(
    guard: &ContainerLockGuard,
    destination: ContainerId,
    slot: SlotRef,
    item: &ItemStack,
    direction: Direction,
) -> bool {
    let (Some(source), Some(destination)) = (guard.get(slot.container), guard.get(destination))
    else {
        return false;
    };
    source.can_take_item(destination, slot.slot, item)
        && source.can_take_item_through_face(slot.slot, item, direction)
}

/// Inserts `stack` into `destination` and returns what did not fit.
///
/// `source` is the container the stack came from, if any; a hopper source can
/// shorten the cooldown of the hopper it fills.
pub(crate) fn add_item(
    guard: &mut ContainerLockGuard,
    source: Option<ContainerId>,
    destination: &[ContainerId],
    mut stack: ItemStack,
    direction: Option<Direction>,
) -> ItemStack {
    for slot in get_slots(guard, destination, direction) {
        if stack.is_empty() {
            break;
        }
        stack = try_move_in_item(guard, source, destination, stack, slot, direction);
    }
    stack
}

fn try_move_in_item(
    guard: &mut ContainerLockGuard,
    source: Option<ContainerId>,
    destination: &[ContainerId],
    mut stack: ItemStack,
    slot: SlotRef,
    direction: Option<Direction>,
) -> ItemStack {
    let Some(target) = guard.get(slot.container) else {
        return stack;
    };
    let can_place = target.can_place_item(slot.slot, &stack)
        && direction.is_none_or(|direction| {
            target.can_place_item_through_face(slot.slot, &stack, direction)
        });
    if !can_place {
        return stack;
    }

    let was_empty = is_empty(guard, destination);
    let existing = item_at(guard, slot);
    let moved = if existing.is_empty() {
        guard.set_item(slot.container, slot.slot, stack);
        stack = ItemStack::empty();
        true
    } else if can_merge_items(&existing, &stack) {
        let space = stack.max_stack_size() - existing.count();
        let amount = stack.count().min(space);
        stack.shrink(amount);
        if let Some(target) = guard.get_mut(slot.container) {
            target.get_item_mut(slot.slot).grow(amount);
        }
        amount > 0
    } else {
        false
    };

    if moved {
        if was_empty {
            shorten_hopper_cooldown(guard, source, slot.container);
        }
        for &container in destination {
            guard.set_changed(container);
        }
    }
    stack
}

/// A hopper that just received its first item waits a full transfer cooldown,
/// one tick less if the item came from a hopper that has not ticked after it.
fn shorten_hopper_cooldown(
    guard: &mut ContainerLockGuard,
    source: Option<ContainerId>,
    destination: ContainerId,
) {
    let source_ticked = source
        .and_then(|source| guard.get_typed::<HopperContainer>(source))
        .map(|hopper| hopper.ticked_game_time);
    let Some(hopper) = guard.get_typed_mut::<HopperContainer>(destination) else {
        return;
    };
    if hopper.is_on_custom_cooldown() {
        return;
    }
    let offset = i32::from(source_ticked.is_some_and(|ticked| hopper.ticked_game_time >= ticked));
    hopper.cooldown_time = super::MOVE_ITEM_SPEED - offset;
}

fn can_merge_items(existing: &ItemStack, stack: &ItemStack) -> bool {
    existing.count() <= existing.max_stack_size()
        && ItemStack::is_same_item_same_components(existing, stack)
}
