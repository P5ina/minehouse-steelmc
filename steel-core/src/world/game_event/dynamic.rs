//! Vanilla `DynamicGameEventListener`: a listener that follows a moving entity
//! from chunk section to chunk section.

use std::sync::{Arc, OnceLock};

use steel_utils::SectionPos;
use steel_utils::locks::SyncMutex;

use super::SharedGameEventListener;
use crate::world::World;

/// What `Entity::update_dynamic_game_event_listeners` should do with each
/// listener: vanilla `DynamicGameEventListener::add`, `::remove` or `::move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicListenerAction {
    /// The entity started being tracked.
    Add,
    /// The entity stopped being tracked.
    Remove,
    /// The entity moved to another chunk section.
    Move,
}

/// Vanilla `DynamicGameEventListener`. The listener itself is created on
/// first use, because it needs the entity's shared handle.
#[derive(Default)]
pub struct DynamicGameEventListener {
    listener: OnceLock<SharedGameEventListener>,
    last_section: SyncMutex<Option<SectionPos>>,
}

impl DynamicGameEventListener {
    /// Creates an empty dynamic listener.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The listener, if it was created.
    #[must_use]
    pub fn listener(&self) -> Option<&SharedGameEventListener> {
        self.listener.get()
    }

    /// Applies `action`, creating the listener with `make` the first time.
    pub fn apply(
        &self,
        world: &World,
        action: DynamicListenerAction,
        make: impl FnOnce() -> SharedGameEventListener,
    ) {
        let listener = self.listener.get_or_init(make);
        match action {
            DynamicListenerAction::Add | DynamicListenerAction::Move => {
                Self::move_listener(world, listener, &self.last_section);
            }
            DynamicListenerAction::Remove => {
                let last_section = *self.last_section.lock();
                if let Some(section) = last_section {
                    world.unregister_game_event_listener(section, listener);
                }
            }
        }
    }

    /// Vanilla `move`: re-registers the listener when its section changes.
    fn move_listener(
        world: &World,
        listener: &SharedGameEventListener,
        last_section: &SyncMutex<Option<SectionPos>>,
    ) {
        let Some(position) = listener.listener_pos() else {
            return;
        };
        let current = SectionPos::from_entity_pos(position);
        let mut last = last_section.lock();
        if *last == Some(current) {
            return;
        }
        if let Some(previous) = *last {
            world.unregister_game_event_listener(previous, listener);
        }
        *last = Some(current);
        world.register_game_event_listener(current, Arc::clone(listener));
    }
}

#[cfg(test)]
mod tests;
