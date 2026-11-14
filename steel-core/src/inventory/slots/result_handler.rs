use std::sync::Arc;

use steel_registry::item_stack::ItemStack;

use crate::{
    inventory::lock::{ContainerLockGuard, ContainerRef},
    player::Player,
};

/// A trait for recipe handlers that update slots in containers according to recipes
pub trait ResultHandler: Send + Sync {
    /// The container the result is written to and read from.
    ///
    /// [`ResultSlot::new`](crate::inventory::slots::ResultSlot::new) derives the
    /// slot's container from this. Menu builders reuse that exact reference for
    /// validation and locking, so handler writes and slot reads cannot target
    /// different containers.
    fn result_container(&self) -> ContainerRef;

    /// Auxiliary containers accessed while validating or taking the result.
    ///
    /// The result container itself is already supplied by
    /// [`result_container`](Self::result_container) and must not be repeated.
    fn dependencies(&self) -> Vec<ContainerRef>;

    /// Recalculate the result based on current inputs, as seen by `player`.
    fn update_result(&self, guard: &mut ContainerLockGuard, player: &Player);

    /// Vanilla `Item.onCraftedBy` as the result slot runs it on a taken result, before the
    /// inputs are consumed; quick moves run it before the result moves. Most results need
    /// nothing.
    fn on_crafted_by(&self, _stack: &mut ItemStack, _player: &Player) {}

    /// Consume inputs when the result is taken. Return overflow remainders.
    fn on_result_taken(&self, guard: &mut ContainerLockGuard, player: &Player)
    -> Option<ItemStack>;

    /// Whether the stored result still matches the current inputs.
    fn is_result_valid(&self, guard: &ContainerLockGuard, player: &Player) -> bool;
}

impl<T: ResultHandler + ?Sized> ResultHandler for Arc<T> {
    fn result_container(&self) -> ContainerRef {
        (**self).result_container()
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        (**self).dependencies()
    }

    fn update_result(&self, guard: &mut ContainerLockGuard, player: &Player) {
        (**self).update_result(guard, player);
    }

    fn on_crafted_by(&self, stack: &mut ItemStack, player: &Player) {
        (**self).on_crafted_by(stack, player);
    }

    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) -> Option<ItemStack> {
        (**self).on_result_taken(guard, player)
    }

    fn is_result_valid(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        (**self).is_result_valid(guard, player)
    }
}
