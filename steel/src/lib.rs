//! # Steel
//!
//! The main library for the Steel Minecraft server.

use std::sync::atomic::Ordering;
use std::{
    error::Error,
    fmt, io,
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, OnceLock},
    time::Duration,
};

use steel_core::player::connection::NetworkConnection;
use steel_core::{
    GIT_HASH_SHORT, command::CommandRegistry, permission::PermissionGroupManager, server::Server,
};
use steel_login::{JavaTcpClient, ServerConnectionSession};
use steel_utils::translations;
use tokio::{net::TcpListener, runtime::Runtime, select, time};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Server configuration module.
pub mod config;
/// A module for logging utilities.
pub mod logger;

/// Static access to the server
pub static SERVER: OnceLock<Arc<Server>> = OnceLock::new();

/// The main server struct.
pub struct SteelServer {
    /// The TCP listener for incoming connections.
    pub tcp_listener: TcpListener,
    /// The cancellation token for graceful shutdown.
    pub cancel_token: CancellationToken,
    /// The next client ID to be assigned.
    pub client_id: u64,
    /// The shared server state.
    pub server: Arc<Server>,
    /// Session id UUID state
    pub connection_session: Arc<ServerConnectionSession>,
    /// Parent of every client connection's token. Cancelled only after players were told the
    /// server is closing, so their disconnect packet still goes out on shutdown.
    connections_token: CancellationToken,
}

/// How long shutdown waits for the players' disconnect packets to be written.
const SHUTDOWN_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Startup error for expected operational failures.
#[derive(Debug)]
pub enum SteelServerError {
    /// Core server startup failed.
    Core(String),
    /// TCP listener could not bind.
    Bind {
        /// Server port that failed to bind.
        port: u16,
        /// Underlying IO error.
        source: io::Error,
    },
}

impl fmt::Display for SteelServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(error) => f.write_str(error),
            Self::Bind { port, source } => {
                write!(f, "failed to bind to server port {port}: {source}")
            }
        }
    }
}

impl Error for SteelServerError {}

impl SteelServer {
    /// Creates a new Steel server.
    ///
    pub async fn new(
        chunk_runtime: Arc<Runtime>,
        cancel_token: CancellationToken,
        steel_config: config::SteelConfig,
    ) -> Result<Self, SteelServerError> {
        Self::new_with_commands(
            chunk_runtime,
            cancel_token,
            steel_config,
            CommandRegistry::new(),
        )
        .await
    }

    /// Creates a new Steel server with additional commands registered atomically at startup.
    pub async fn new_with_commands(
        chunk_runtime: Arc<Runtime>,
        cancel_token: CancellationToken,
        steel_config: config::SteelConfig,
        command_registry: CommandRegistry,
    ) -> Result<Self, SteelServerError> {
        log::info!("Starting Steel Server ({GIT_HASH_SHORT})");

        let permission_group_store = steel_config.permission_group_store();
        let server_port = steel_config.server.server_port;
        let worlds_config = steel_config.worlds;
        let permission_groups =
            PermissionGroupManager::new(steel_config.groups, permission_group_store).map_err(
                |error| {
                    SteelServerError::Core(format!("failed to validate groups config: {error}"))
                },
            )?;
        let player_idle_timeout = steel_config.server.player_idle_timeout;
        let runtime_config = steel_config.server.into_runtime_config();

        let server = Server::new_with_commands(
            chunk_runtime,
            cancel_token.clone(),
            runtime_config,
            worlds_config,
            permission_groups,
            command_registry,
        )
        .await
        .map_err(SteelServerError::Core)?;

        server
            .player_idle_timeout
            .store(player_idle_timeout, Ordering::Relaxed);

        let tcp_listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, server_port))
            .await
            .map_err(|source| SteelServerError::Bind {
                port: server_port,
                source,
            })?;

        Ok(Self {
            tcp_listener,
            cancel_token,
            client_id: 0,
            server: Arc::new(server),
            connection_session: Arc::new(ServerConnectionSession::default()),
            connections_token: CancellationToken::new(),
        })
    }

    /// Starts the server and begins accepting connections.
    pub async fn start(&mut self, task_tracker: TaskTracker) {
        log::info!("Started Steel Server");

        let server = Arc::clone(&self.server);
        let token = self.cancel_token.clone();
        let server_handle = tokio::spawn(async move {
            server.run(token).await;
        });

        loop {
            select! {
                () = self.cancel_token.cancelled() => {
                    break;
                }
                accept_result = self.tcp_listener.accept() => {
                    let Ok((connection, address)) = accept_result else {
                        continue;
                    };
                    if let Err(e) = connection.set_nodelay(true) {
                        log::warn!("Failed to set TCP_NODELAY: {e}");
                    }
                    let (java_client, sender_recv, net_reader) = JavaTcpClient::new(
                        connection,
                        address,
                        self.client_id,
                        self.connections_token.child_token(),
                        Arc::clone(&self.server),
                        Arc::clone(&self.connection_session),
                        task_tracker.clone(),
                    );
                    self.client_id = self.client_id.wrapping_add(1);
                    log::info!("Accepted connection from Java Edition: {address} (id {})", self.client_id);

                    let java_client = Arc::new(java_client);
                    java_client.start_outgoing_packet_task(sender_recv);
                    java_client.start_incoming_packet_task(net_reader);
                    // Java_client won't drop until the incoming and outcoming task close
                    // So we dont need to care about them here anymore
                }
            }
        }
        self.disconnect_players_for_shutdown().await;
        let _ = server_handle.await;
    }

    /// Like vanilla's `PlayerList.removeAll` at the start of shutdown: every player gets
    /// "Server closed" before the connections go away. A proxy (Velocity, Gate) treats that as a
    /// normal kick and can move the player elsewhere; a socket that just closes reads as a crash.
    async fn disconnect_players_for_shutdown(&self) {
        let players = self.server.get_players();
        for player in &players {
            player.disconnect(translations::MULTIPLAYER_DISCONNECT_SERVER_SHUTDOWN.msg());
        }
        let flushed = time::timeout(SHUTDOWN_DISCONNECT_TIMEOUT, async {
            while players.iter().any(|player| !player.connection.closed()) {
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if flushed.is_err() {
            log::warn!("Timed out telling players the server is closing");
        }
        // Clients still logging in or configuring, and anyone whose write timed out.
        self.connections_token.cancel();
    }
}
