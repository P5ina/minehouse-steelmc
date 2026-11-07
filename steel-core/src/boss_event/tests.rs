use std::io::{Cursor, Read as _};
use std::sync::Arc;

use steel_protocol::packet_traits::{CompressionInfo, EncodedPacket};
use steel_protocol::packets::game::{BossBarColor, BossBarOverlay};
use steel_registry::packets::play::C_BOSS_EVENT;
use steel_utils::codec::VarInt;
use steel_utils::locks::SyncMutex;
use steel_utils::serial::ReadFrom as _;
use text_components::TextComponent;
use uuid::Uuid;

use super::ServerBossEvent;
use crate::player::connection::NetworkConnection;
use crate::player::{Player, PlayerConnection};
use crate::test_support::{TestPlayerBuilder, test_runtime_config, test_world};

const ADD: i32 = 0;
const REMOVE: i32 = 1;
const UPDATE_PROGRESS: i32 = 2;
const UPDATE_NAME: i32 = 3;

struct RecordingConnection {
    packets: Arc<SyncMutex<Vec<EncodedPacket>>>,
}

impl NetworkConnection for RecordingConnection {
    fn compression(&self) -> Option<CompressionInfo> {
        None
    }

    fn send_encoded(&self, packet: EncodedPacket) {
        self.packets.lock().push(packet);
    }

    fn send_encoded_bundle(&self, packets: Vec<EncodedPacket>) {
        self.packets.lock().extend(packets);
    }

    fn disconnect_with_reason(&self, _reason: TextComponent) {}

    fn tick(&self) {}

    fn latency(&self) -> i32 {
        0
    }

    fn close(&self) {}

    fn closed(&self) -> bool {
        false
    }
}

fn recording_player(
    name: &str,
    entity_id: i32,
) -> (Arc<Player>, Arc<SyncMutex<Vec<EncodedPacket>>>) {
    let packets = Arc::new(SyncMutex::new(Vec::new()));
    let connection = Arc::new(PlayerConnection::Other(Box::new(RecordingConnection {
        packets: Arc::clone(&packets),
    })));
    let player = TestPlayerBuilder::new(Arc::clone(test_world()), name, entity_id)
        .detached_config(test_runtime_config(2))
        .connection(connection)
        .build();
    (player, packets)
}

/// Drains the recorded boss event operation ids, checking they target `bar`.
fn operations(packets: &SyncMutex<Vec<EncodedPacket>>, bar: Uuid) -> Vec<i32> {
    packets
        .lock()
        .drain(..)
        .filter_map(|packet| {
            let mut cursor = Cursor::new(packet.encoded_data.as_slice());
            VarInt::read(&mut cursor).expect("length");
            let id = VarInt::read(&mut cursor).expect("packet id").0;
            if id != C_BOSS_EVENT {
                return None;
            }
            let mut uuid = [0; 16];
            cursor.read_exact(&mut uuid).expect("uuid");
            assert_eq!(Uuid::from_bytes(uuid), bar);
            Some(VarInt::read(&mut cursor).expect("operation").0)
        })
        .collect()
}

fn bar() -> ServerBossEvent {
    ServerBossEvent::new(
        Uuid::from_u128(7),
        TextComponent::plain("Raid"),
        BossBarColor::Red,
        BossBarOverlay::Notched10,
    )
}

#[test]
fn players_get_add_and_updates_only_on_change() {
    let bar = bar();
    let (player, packets) = recording_player("BossViewer", 9101);
    packets.lock().clear();

    bar.add_player(&player);
    bar.add_player(&player);
    assert_eq!(operations(&packets, bar.id()), [ADD]);

    bar.set_progress(0.5);
    bar.set_progress(0.5);
    bar.set_name(TextComponent::plain("Raid"));
    bar.set_name(TextComponent::plain("Raid - 2"));
    assert_eq!(
        operations(&packets, bar.id()),
        [UPDATE_PROGRESS, UPDATE_NAME]
    );

    bar.remove_player(&player);
    bar.remove_player(&player);
    assert_eq!(operations(&packets, bar.id()), [REMOVE]);
    assert!(bar.players().is_empty());
}

#[test]
fn hidden_bar_sends_nothing_until_shown_again() {
    let bar = bar();
    let (player, packets) = recording_player("HiddenBossViewer", 9102);
    bar.add_player(&player);
    packets.lock().clear();

    bar.set_visible(false);
    bar.set_progress(0.25);
    assert_eq!(operations(&packets, bar.id()), [REMOVE]);

    bar.set_visible(true);
    assert_eq!(operations(&packets, bar.id()), [ADD]);
    assert!((bar.progress() - 0.25).abs() < f32::EPSILON);

    bar.remove_all_players();
    assert_eq!(operations(&packets, bar.id()), [REMOVE]);
}
