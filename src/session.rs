use dashmap::DashMap;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, error, info};

use crate::config::Protocol;

/// Dedicated socket for a session to enable bi-directional UDP communication
#[derive(Clone)]
pub struct SessionSocket {
    /// The dedicated UDP socket for this session
    socket: Arc<UdpSocket>,
    /// Shutdown signal to stop the receive task
    shutdown: Arc<RwLock<bool>>,
}

impl SessionSocket {
    /// Create a new session socket bound to an ephemeral port
    pub async fn new() -> Result<Self, std::io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self {
            socket: Arc::new(socket),
            shutdown: Arc::new(RwLock::new(false)),
        })
    }

    /// Get the socket for sending packets
    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }

    /// Get the local address of the socket
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.socket.local_addr()
    }

    /// Signal shutdown to the receive task
    pub async fn shutdown(&self) {
        let mut shutdown = self.shutdown.write().await;
        *shutdown = true;
    }

    /// Start a background task to receive packets from the target and forward
    /// them to the exact client socket associated with this upstream socket.
    pub fn start_receive_task(
        &self,
        client_addr: SocketAddr,
        proxy_port: u16,
        proxy_socket: Arc<UdpSocket>,
    ) {
        let socket = self.socket.clone();
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let mut buffer = vec![0u8; 65535];
            loop {
                // Check for shutdown signal
                if *shutdown.read().await {
                    debug!(
                        "Receive task shutting down for client {} on port {}",
                        client_addr, proxy_port
                    );
                    break;
                }

                // Set a timeout for recv_from to periodically check shutdown
                match tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buffer))
                    .await
                {
                    Ok(Ok((len, target_addr))) => {
                        debug!(
                            "Received {} bytes from target {} for client {}",
                            len, target_addr, client_addr
                        );

                        if let Err(e) = proxy_socket.send_to(&buffer[..len], client_addr).await {
                            error!("Failed to forward packet to client {}: {}", client_addr, e);
                        }
                    }
                    Ok(Err(e)) => {
                        error!(
                            "Error receiving from target for client {}: {}",
                            client_addr, e
                        );
                        break;
                    }
                    Err(_) => {
                        // Timeout - continue loop to check shutdown
                        continue;
                    }
                }
            }
            debug!(
                "Receive task terminated for client {} on port {}",
                client_addr, proxy_port
            );
        });
    }
}

/// Session information for a client connection with multi-port support
#[derive(Clone)]
pub struct Session {
    pub target_ip: String,
    /// Port mappings: (proxy_port, protocol) -> target_port
    pub port_mappings: HashMap<(u16, Protocol), u16>,
    pub last_activity: Instant,
    /// Dedicated UDP sockets keyed by (proxy port, client source port).
    /// Keeping client sockets isolated prevents overlapping connections from the
    /// same IP (for example during Unreal ClientTravel) from sharing a backend
    /// UDP flow and corrupting each other's packets.
    pub udp_sockets: HashMap<(u16, u16), SessionSocket>,
    /// Exact Kubernetes Pod UID for controller-issued Project X routes.
    pub pod_uid: Option<String>,
}

impl Session {
    /// Create a new session with a single port (backwards compatibility)
    pub fn new(target_addr: SocketAddr) -> Self {
        let mut port_mappings = HashMap::new();
        port_mappings.insert((target_addr.port(), Protocol::Udp), target_addr.port());
        Self {
            target_ip: target_addr.ip().to_string(),
            port_mappings,
            last_activity: Instant::now(),
            udp_sockets: HashMap::new(),
            pod_uid: None,
        }
    }

    /// Create a new multi-port session
    pub fn new_multi_port(target_ip: String, port_mappings: HashMap<(u16, Protocol), u16>) -> Self {
        Self {
            target_ip,
            port_mappings,
            last_activity: Instant::now(),
            udp_sockets: HashMap::new(),
            pod_uid: None,
        }
    }

    /// Get or create a dedicated UDP socket for a specific proxy port
    pub async fn get_or_create_udp_socket(
        &mut self,
        proxy_port: u16,
        client_addr: SocketAddr,
        proxy_socket: Arc<UdpSocket>,
    ) -> Result<SessionSocket, std::io::Error> {
        let socket_key = (proxy_port, client_addr.port());

        if let Some(session_socket) = self.udp_sockets.get(&socket_key) {
            return Ok(session_socket.clone());
        }

        // Create new socket
        let session_socket = SessionSocket::new().await?;
        let local_addr = session_socket.local_addr()?;
        debug!(
            "Created dedicated socket {} for client {} on proxy port {}",
            local_addr, client_addr, proxy_port
        );

        // Responses from this upstream socket belong only to this client socket.
        session_socket.start_receive_task(client_addr, proxy_port, proxy_socket);

        // Store socket
        self.udp_sockets.insert(socket_key, session_socket.clone());

        Ok(session_socket)
    }

    /// Shutdown all UDP sockets for this session
    pub async fn shutdown_sockets(&mut self) {
        for ((proxy_port, client_port), socket) in &self.udp_sockets {
            debug!(
                "Shutting down socket for proxy port {} and client port {}",
                proxy_port, client_port
            );
            socket.shutdown().await;
        }
        self.udp_sockets.clear();
    }

    /// Get target address for a specific proxy port and protocol
    pub fn get_target_addr(
        &self,
        proxy_port: u16,
        protocol: Protocol,
    ) -> Result<SocketAddr, std::io::Error> {
        let target_port = self
            .port_mappings
            .get(&(proxy_port, protocol))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "No port mapping found for proxy port {} ({})",
                        proxy_port, protocol
                    ),
                )
            })?;

        format!("{}:{}", self.target_ip, target_port)
            .parse()
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Invalid socket address: {}", e),
                )
            })
    }

    /// Update the last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check if the session has timed out
    pub fn is_timed_out(&self, timeout_seconds: u64) -> bool {
        self.last_activity.elapsed() > Duration::from_secs(timeout_seconds)
    }
}

/// Callback type for session cleanup notifications
pub type SessionCleanupCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Session manager for tracking active client sessions with multi-port support
/// Sessions are now tracked by client address only, established via query port
#[derive(Clone)]
pub struct SessionManager {
    /// Key: client_ip -> Session
    /// Sessions are keyed by IP address only, not IP:Port
    /// This ensures all connections from the same client use the same session
    sessions: Arc<DashMap<IpAddr, Session>>,
    timeout_seconds: u64,
    /// Optional callback to notify when sessions are cleaned up
    cleanup_callback: Option<SessionCleanupCallback>,
    route_history: Arc<DashMap<String, OffsetDateTime>>,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(timeout_seconds: u64) -> Self {
        let manager = Self {
            sessions: Arc::new(DashMap::new()),
            timeout_seconds,
            cleanup_callback: None,
            route_history: Arc::new(DashMap::new()),
        };

        // Start cleanup task
        let cleanup_manager = manager.clone();
        tokio::spawn(async move {
            cleanup_manager.cleanup_loop().await;
        });

        manager
    }

    /// Set a cleanup callback to be notified when sessions are removed
    pub fn set_cleanup_callback(&mut self, callback: SessionCleanupCallback) {
        self.cleanup_callback = Some(callback);
    }

    /// Get an existing session for a client IP address
    pub fn get(&self, client_ip: &IpAddr) -> Option<Session> {
        self.sessions.get(client_ip).map(|entry| entry.clone())
    }

    /// Get an existing session for a client SocketAddr (convenience method)
    pub fn get_by_addr(&self, client_addr: &SocketAddr) -> Option<Session> {
        self.get(&client_addr.ip())
    }

    /// Get a mutable reference to a session for socket creation
    pub fn get_mut(
        &self,
        client_ip: &IpAddr,
    ) -> Option<dashmap::mapref::one::RefMut<'_, IpAddr, Session>> {
        self.sessions.get_mut(client_ip)
    }

    /// Get a mutable reference to a session by SocketAddr (convenience method)
    pub fn get_mut_by_addr(
        &self,
        client_addr: &SocketAddr,
    ) -> Option<dashmap::mapref::one::RefMut<'_, IpAddr, Session>> {
        self.get_mut(&client_addr.ip())
    }

    /// Update or create a session (for session reset) - single port version
    pub async fn upsert(&self, client_addr: SocketAddr, target_addr: SocketAddr) {
        let client_ip = client_addr.ip();

        // If session exists, shut down old sockets
        if let Some(mut old_session) = self.sessions.get_mut(&client_ip) {
            old_session.shutdown_sockets().await;
        }

        let session = Session::new(target_addr);
        self.sessions.insert(client_ip, session.clone());
        debug!("Session upserted: {} -> {}", client_ip, target_addr);
    }

    /// Update or create a multi-port session
    pub async fn upsert_multi_port(
        &self,
        client_addr: SocketAddr,
        target_ip: String,
        port_mappings: HashMap<(u16, Protocol), u16>,
    ) {
        let client_ip = client_addr.ip();

        // If session exists, shut down old sockets
        if let Some(mut old_session) = self.sessions.get_mut(&client_ip) {
            old_session.shutdown_sockets().await;
        }

        let session = Session::new_multi_port(target_ip.clone(), port_mappings.clone());
        self.sessions.insert(client_ip, session);

        debug!(
            "Multi-port session upserted: {} -> {} ({} ports)",
            client_ip,
            target_ip,
            port_mappings.len()
        );
    }

    /// Install a route bound to one exact Project X Pod UID.
    pub async fn upsert_project_x(
        &self,
        client_addr: SocketAddr,
        target_ip: String,
        port_mappings: HashMap<(u16, Protocol), u16>,
        pod_uid: String,
    ) {
        self.upsert_multi_port(client_addr, target_ip, port_mappings)
            .await;
        if let Some(mut session) = self.sessions.get_mut(&client_addr.ip()) {
            session.pod_uid = Some(pod_uid.clone());
        }
        self.route_history
            .insert(pod_uid, OffsetDateTime::now_utc());
    }

    /// Return the route count and last binding time, or `None` after an unknown restart.
    pub fn project_x_route_status(&self, pod_uid: &str) -> Option<(usize, OffsetDateTime)> {
        let last_new = *self.route_history.get(pod_uid)?;
        let active = self
            .sessions
            .iter()
            .filter(|entry| entry.value().pod_uid.as_deref() == Some(pod_uid))
            .count();
        Some((active, last_new))
    }

    /// Touch a session to update its last activity
    pub fn touch(&self, client_ip: &IpAddr) {
        if let Some(mut entry) = self.sessions.get_mut(client_ip) {
            entry.touch();
        }
    }

    /// Touch a session by SocketAddr (convenience method)
    pub fn touch_by_addr(&self, client_addr: &SocketAddr) {
        self.touch(&client_addr.ip());
    }

    /// Get the number of active sessions
    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Clear all sessions (called during shutdown)
    pub async fn clear_all(&self) {
        let count = self.sessions.len();

        // Shutdown all sockets before clearing
        for mut entry in self.sessions.iter_mut() {
            entry.shutdown_sockets().await;
        }

        self.sessions.clear();
        if count > 0 {
            info!("Cleared {} active sessions during shutdown", count);
        }
    }

    /// Cleanup loop to remove timed-out sessions
    async fn cleanup_loop(&self) {
        let mut cleanup_interval = interval(Duration::from_secs(30));

        loop {
            cleanup_interval.tick().await;

            let mut removed_count = 0;
            let mut to_remove = Vec::new();

            // Collect sessions to remove
            for entry in self.sessions.iter() {
                if entry.value().is_timed_out(self.timeout_seconds) {
                    to_remove.push(*entry.key());
                }
            }

            // Remove timed out sessions and shutdown their sockets
            for key in to_remove {
                if let Some((_, mut session)) = self.sessions.remove(&key) {
                    debug!("Session timed out: {:?}", key);

                    // Notify callback if set
                    if let Some(callback) = &self.cleanup_callback {
                        callback(&session.target_ip);
                    }

                    session.shutdown_sockets().await;
                    removed_count += 1;
                }
            }

            if removed_count > 0 {
                info!(
                    "Cleaned up {} timed-out sessions. Active sessions: {}",
                    removed_count,
                    self.count()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_creation() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let target_addr: SocketAddr = "10.0.0.1:7777".parse().unwrap();

        manager.upsert(client_addr, target_addr).await;
        let session = manager.get_by_addr(&client_addr).unwrap();
        assert_eq!(session.target_ip, "10.0.0.1");
        assert_eq!(manager.count(), 1);
    }

    #[tokio::test]
    async fn test_session_upsert() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let target_addr1: SocketAddr = "10.0.0.1:7777".parse().unwrap();
        let target_addr2: SocketAddr = "10.0.0.2:7777".parse().unwrap();

        manager.upsert(client_addr, target_addr1).await;
        let session = manager.get_by_addr(&client_addr).unwrap();
        assert_eq!(session.target_ip, "10.0.0.1");

        manager.upsert(client_addr, target_addr2).await;
        let session = manager.get_by_addr(&client_addr).unwrap();
        assert_eq!(session.target_ip, "10.0.0.2");
        assert_eq!(manager.count(), 1);
    }

    #[tokio::test]
    async fn test_session_timeout() {
        let manager = SessionManager::new(1); // 1 second timeout
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let target_addr: SocketAddr = "10.0.0.1:7777".parse().unwrap();

        manager.upsert(client_addr, target_addr).await;
        let session = manager.get_by_addr(&client_addr).unwrap();
        assert!(!session.is_timed_out(1));

        tokio::time::sleep(Duration::from_secs(2)).await;
        let session = manager.get_by_addr(&client_addr).unwrap();
        assert!(session.is_timed_out(1));
    }

    #[tokio::test]
    async fn test_multi_port_session() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let target_ip = "10.0.0.1".to_string();

        let mut port_mappings = HashMap::new();
        port_mappings.insert((7777, Protocol::Udp), 7777);
        port_mappings.insert((7777, Protocol::Tcp), 7778);
        port_mappings.insert((27015, Protocol::Udp), 27015);

        manager
            .upsert_multi_port(client_addr, target_ip.clone(), port_mappings)
            .await;

        let session = manager.get_by_addr(&client_addr).unwrap();
        assert_eq!(session.target_ip, target_ip);
        assert_eq!(session.port_mappings.len(), 3);

        // Test getting target addresses for different ports
        let udp_addr = session.get_target_addr(7777, Protocol::Udp).unwrap();
        assert_eq!(udp_addr.to_string(), "10.0.0.1:7777");

        let tcp_addr = session.get_target_addr(7777, Protocol::Tcp).unwrap();
        assert_eq!(tcp_addr.to_string(), "10.0.0.1:7778");

        let query_addr = session.get_target_addr(27015, Protocol::Udp).unwrap();
        assert_eq!(query_addr.to_string(), "10.0.0.1:27015");
    }

    #[tokio::test]
    async fn test_session_touch() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let target_addr: SocketAddr = "10.0.0.1:7777".parse().unwrap();

        manager.upsert(client_addr, target_addr).await;
        let session1 = manager.get_by_addr(&client_addr).unwrap();
        let time1 = session1.last_activity;

        tokio::time::sleep(Duration::from_millis(100)).await;
        manager.touch_by_addr(&client_addr);

        let session2 = manager.get_by_addr(&client_addr).unwrap();
        let time2 = session2.last_activity;

        assert!(time2 > time1);
    }

    #[tokio::test]
    async fn test_ip_based_sessions() {
        // Route selection is keyed by IP, so a query from a new source port
        // replaces the destination used by the client's data socket.
        let manager = SessionManager::new(300);
        let client_addr1: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let client_addr2: SocketAddr = "127.0.0.1:54321".parse().unwrap(); // Same IP, different port
        let target_addr1: SocketAddr = "10.0.0.1:7777".parse().unwrap();
        let target_addr2: SocketAddr = "10.0.0.2:7777".parse().unwrap();

        // Create session with first port
        manager.upsert(client_addr1, target_addr1).await;
        assert_eq!(manager.count(), 1);

        // A second query from another source port must replace that same session.
        manager.upsert(client_addr2, target_addr2).await;
        let session1 = manager.get_by_addr(&client_addr1).unwrap();
        let session2 = manager.get_by_addr(&client_addr2).unwrap();
        assert_eq!(session1.target_ip, "10.0.0.2");
        assert_eq!(session1.target_ip, session2.target_ip);
        assert_eq!(manager.count(), 1); // Still only one session
    }

    #[tokio::test]
    async fn test_udp_sockets_are_isolated_by_client_source_port() {
        let mut session = Session::new("10.0.0.1:7777".parse().unwrap());
        let proxy_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let old_client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let new_client_addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();

        let old_socket = session
            .get_or_create_udp_socket(7777, old_client_addr, proxy_socket.clone())
            .await
            .unwrap();
        let new_socket = session
            .get_or_create_udp_socket(7777, new_client_addr, proxy_socket.clone())
            .await
            .unwrap();
        let reused_new_socket = session
            .get_or_create_udp_socket(7777, new_client_addr, proxy_socket)
            .await
            .unwrap();

        assert_ne!(
            old_socket.local_addr().unwrap(),
            new_socket.local_addr().unwrap()
        );
        assert_eq!(
            new_socket.local_addr().unwrap(),
            reused_new_socket.local_addr().unwrap()
        );
        assert_eq!(session.udp_sockets.len(), 2);

        session.shutdown_sockets().await;
    }

    #[tokio::test]
    async fn project_x_routes_are_exact_and_preserved_during_drain() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let mut ports = HashMap::new();
        ports.insert((7777, Protocol::Udp), 7777);
        manager
            .upsert_project_x(
                client_addr,
                "10.0.0.1".to_string(),
                ports,
                "exact-pod-uid".to_string(),
            )
            .await;

        // Drain only blocks future controller reservations. The established
        // route remains pinned until its normal session timeout.
        let (active, _) = manager.project_x_route_status("exact-pod-uid").unwrap();
        assert_eq!(active, 1);
        assert_eq!(
            manager
                .get_by_addr(&client_addr)
                .unwrap()
                .pod_uid
                .as_deref(),
            Some("exact-pod-uid")
        );
        assert!(
            manager
                .project_x_route_status("different-pod-uid")
                .is_none()
        );
    }

    #[tokio::test]
    async fn project_x_route_history_reports_zero_after_route_ends() {
        let manager = SessionManager::new(300);
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        manager
            .upsert_project_x(
                client_addr,
                "10.0.0.1".to_string(),
                HashMap::new(),
                "pod-uid".to_string(),
            )
            .await;
        manager
            .upsert(client_addr, "10.0.0.2:7777".parse().unwrap())
            .await;

        let (active, _) = manager.project_x_route_status("pod-uid").unwrap();
        assert_eq!(active, 0);
    }

    #[tokio::test]
    async fn project_x_route_state_is_unknown_after_restart() {
        let manager = SessionManager::new(300);
        assert!(
            manager
                .project_x_route_status("pod-from-before-restart")
                .is_none()
        );
    }
}
