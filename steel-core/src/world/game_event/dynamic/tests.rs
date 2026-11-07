use std::sync::Arc;

use glam::DVec3;
use steel_registry::game_events::GameEventRef;
use steel_registry::vanilla_game_events;
use steel_utils::ChunkPos;
use steel_utils::locks::SyncMutex;

use super::{DynamicGameEventListener, DynamicListenerAction};
use crate::bootstrap::init_globals;
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
use crate::world::World;
use crate::world::game_event::{GameEventContext, GameEventListener};

struct MovableListener {
    pos: SyncMutex<DVec3>,
    heard: SyncMutex<u32>,
}

impl GameEventListener for MovableListener {
    fn listener_pos(&self) -> Option<DVec3> {
        Some(*self.pos.lock())
    }

    fn listener_radius(&self) -> i32 {
        16
    }

    fn handle_game_event(
        &self,
        _world: &Arc<World>,
        _event: GameEventRef,
        _context: &GameEventContext<'_>,
        _source_pos: DVec3,
    ) -> bool {
        *self.heard.lock() += 1;
        true
    }
}

#[test]
fn listener_follows_its_section_and_unregisters() {
    init_globals();
    let fixture = fresh_test_world("dynamic_game_event_listener");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    insert_ready_full_chunk(world, ChunkPos::new(4, 0));

    let listener = Arc::new(MovableListener {
        pos: SyncMutex::new(DVec3::new(8.5, 65.0, 8.5)),
        heard: SyncMutex::new(0),
    });
    let dynamic = DynamicGameEventListener::new();
    let shared = Arc::clone(&listener);
    dynamic.apply(world, DynamicListenerAction::Add, || shared);

    let event = &vanilla_game_events::NOTE_BLOCK_PLAY;
    let context = GameEventContext::default();
    world.game_event_at(event, DVec3::new(10.5, 65.5, 8.5), &context);
    assert_eq!(*listener.heard.lock(), 1);

    // Moving 64 blocks away re-registers it in the new section.
    *listener.pos.lock() = DVec3::new(72.5, 65.0, 8.5);
    dynamic.apply(world, DynamicListenerAction::Move, || unreachable!());
    world.game_event_at(event, DVec3::new(10.5, 65.5, 8.5), &context);
    assert_eq!(*listener.heard.lock(), 1);
    world.game_event_at(event, DVec3::new(70.5, 65.5, 8.5), &context);
    assert_eq!(*listener.heard.lock(), 2);

    dynamic.apply(world, DynamicListenerAction::Remove, || unreachable!());
    world.game_event_at(event, DVec3::new(70.5, 65.5, 8.5), &context);
    assert_eq!(*listener.heard.lock(), 2);
}
