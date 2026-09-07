use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::config::{Config, DataPortConfig, Protocol};
use crate::k8s_client::K8sClient;
use crate::load_balancer::LoadBalancer;
use crate::session::SessionManager;

/// Cached default endpoint target with multi-port support
#[derive(Clone, Debug)]
pub(crate) struct DefaultEndpointCache {
    address: String,
    /// Port mappings: (proxy_port, protocol) -> target_port
    port_mappings: HashMap<(u16, Protocol), u16>,
}

/// Data Proxy for TCP/UDP forwarding - Multi-port support
pub struct DataProxy {
    data_ports: Vec<DataPortConfig>,
    session_manager: SessionManager,
    config: Config,
    k8s_client: K8sClient,
    default_endpoint_cache: Arc<RwLock<Option<DefaultEndpointCache>>>,
    load_balancer: LoadBalancer,
}

/// Shared cache for default endpoint that can be invalidated
#[derive(Clone)]
pub struct DefaultEndpointCacheHandle {
    cache: Arc<RwLock<Option<DefaultEndpointCache>>>,
}

impl DefaultEndpointCacheHandle {
    /// Create a new cache handle
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Invalidate the cache
    pub async fn invalidate(&self) {
        let mut cache = self.cache.write().await;
        *cache = None;
    }

    /// Get the cache
    pub fn get_cache(&self) -> Arc<RwLock<Option<DefaultEndpointCache>>> {
        self.cache.clone()
    }
}

impl DataProxy {
    /// Create a new multi-port data proxy
    pub fn new(
        session_manager: SessionManager,
        config: Config,
        k8s_client: K8sClient,
        cache_handle: DefaultEndpointCacheHandle,
    ) -> Self {
        let data_ports = config.get_data_ports();
        let lb_config = config.get_load_balancing();
        let load_balancer = LoadBalancer::new(lb_config.strategy, k8s_client.clone());

        Self {
            data_ports,
            session_manager,
            config,
            k8s_client,
            default_endpoint_cache: cache_handle.get_cache(),
            load_balancer,
        }
    }

    /// Run the multi-port data proxy
    pub async fn run(&self) -> Result<()> {
        let mut tasks = vec![];

        // Bind and spawn a task for each data port
        for port_config in &self.data_ports {
            let proxy = self.clone();
            let port = port_config.port;
            let protocol = port_config.protocol;

            let task = match protocol {
                Protocol::Udp => {
                    let socket = Arc::new(
                        UdpSocket::bind(format!("0.0.0.0:{}", port))
                            .await
                            .with_context(|| {
                                format!("Failed to bind UDP data proxy to port {}", port)
                            })?,
                    );

                    info!("Data proxy listening on UDP port {}", port);
                    tokio::spawn(async move { proxy.run_udp_socket(socket, port).await })
                }
                Protocol::Tcp => {
                    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
                        .await
                        .with_context(|| {
                            format!("Failed to bind TCP data proxy to port {}", port)
                        })?;

                    info!("Data proxy listening on TCP port {}", port);
                    tokio::spawn(async move { proxy.run_tcp_listener(listener, port).await })
                }
            };

            tasks.push(task);
        }

        // Wait for all tasks to complete
        futures::future::join_all(tasks).await;
        Ok(())
    }

    /// Run a UDP socket listener
    async fn run_udp_socket(&self, socket: Arc<UdpSocket>, proxy_port: u16) -> Result<()> {
        let mut buffer = vec![0u8; 65535]; // Max UDP packet size

        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((len, client_addr)) => {
                    let packet_data = buffer[..len].to_vec();
                    let socket_clone = socket.clone();
                    let proxy = self.clone();

                    tokio::spawn(async move {
                        let result = proxy
                            .handle_udp_packet(socket_clone, client_addr, packet_data, proxy_port)
                            .await;
                        if let Err(e) = result {
                            error!(
                                "Error handling UDP packet from {} on port {}: {}",
                                client_addr, proxy_port, e
                            );
                        }
                    });
                }
                Err(e) => {
                    error!("Error receiving UDP packet on port {}: {}", proxy_port, e);
                }
            }
        }
    }

    /// Run a TCP listener
    async fn run_tcp_listener(&self, listener: TcpListener, proxy_port: u16) -> Result<()> {
        loop {
            match listener.accept().await {
                Ok((stream, client_addr)) => {
                    let proxy = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = proxy
                            .handle_tcp_connection(stream, client_addr, proxy_port)
                            .await
                        {
                            error!(
                                "Error handling TCP connection from {} on port {}: {}",
                                client_addr, proxy_port, e
                            );
                        }
                    });
                }
                Err(e) => {
                    error!(
                        "Error accepting TCP connection on port {}: {}",
                        proxy_port, e
                    );
                }
            }
        }
    }

    /// Handle a UDP packet
    async fn handle_udp_packet(
        &self,
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        packet_data: Vec<u8>,
        proxy_port: u16,
    ) -> Result<()> {
        self.handle_udp_data_packet(socket, client_addr, packet_data, proxy_port)
            .await
    }

    /// Handle a TCP connection
    async fn handle_tcp_connection(
        &self,
        mut stream: TcpStream,
        client_addr: SocketAddr,
        proxy_port: u16,
    ) -> Result<()> {
        debug!("TCP connection from {} on port {}", client_addr, proxy_port);

        // Check if session exists for this client
        let session = self.session_manager.get_by_addr(&client_addr);

        if session.is_none() {
            // No session - establish default route
            self.establish_default_session(client_addr, proxy_port, Protocol::Tcp)
                .await?;
        }

        // Get session again after potential establishment
        let session = self
            .session_manager
            .get_by_addr(&client_addr)
            .ok_or_else(|| anyhow::anyhow!("Failed to establish session for TCP connection"))?;

        let target_addr = session.get_target_addr(proxy_port, Protocol::Tcp)?;

        // Connect to target
        let mut target_stream = TcpStream::connect(target_addr).await?;
        info!(
            "TCP connection established: {} -> {}",
            client_addr, target_addr
        );

        // Bidirectional copy
        match tokio::io::copy_bidirectional(&mut stream, &mut target_stream).await {
            Ok((from_client, from_server)) => {
                debug!(
                    "TCP connection closed: {} -> {} (client->server: {} bytes, server->client: {} bytes)",
                    client_addr, target_addr, from_client, from_server
                );
            }
            Err(e) => {
                error!(
                    "TCP proxy error for {} -> {}: {}",
                    client_addr, target_addr, e
                );
            }
        }

        // Touch session on close
        self.session_manager.touch_by_addr(&client_addr);
        Ok(())
    }

    /// Handle a UDP data packet (standard proxy) with port-specific routing
    /// Sessions are established via query port, so we just route based on existing session
    /// Now uses dedicated sockets for bi-directional communication
    async fn handle_udp_data_packet(
        &self,
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        packet_data: Vec<u8>,
        proxy_port: u16,
    ) -> Result<()> {
        // TCP queries establish the route before gameplay begins.
        if self.session_manager.get_by_addr(&client_addr).is_some() {
            // Session exists - get or create dedicated socket and forward packet
            self.proxy_packet_bidirectional(socket, client_addr, packet_data, proxy_port)
                .await?;
            self.session_manager.touch_by_addr(&client_addr);
        } else {
            // No session exists - establish default route for this client
            self.handle_first_packet(socket, client_addr, packet_data, proxy_port)
                .await?;
        }

        Ok(())
    }

    /// Handle the first UDP packet from a client without an established session
    /// This happens when clients connect without using the query port
    /// We establish a session to the default endpoint
    async fn handle_first_packet(
        &self,
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        packet_data: Vec<u8>,
        proxy_port: u16,
    ) -> Result<()> {
        // Establish default session
        self.establish_default_session(client_addr, proxy_port, Protocol::Udp)
            .await?;

        // Forward this first packet using bi-directional proxy
        self.proxy_packet_bidirectional(socket, client_addr, packet_data, proxy_port)
            .await?;

        Ok(())
    }

    /// Establish a default session for a client
    async fn establish_default_session(
        &self,
        client_addr: SocketAddr,
        proxy_port: u16,
        protocol: Protocol,
    ) -> Result<()> {
        debug!(
            "Establishing default route for {} on port {} ({})",
            client_addr, proxy_port, protocol
        );

        // Check cache first
        let cached_endpoint = self.default_endpoint_cache.read().await;
        let (target_ip, port_mappings) = if let Some(cache) = cached_endpoint.as_ref() {
            // Use cached endpoint
            if cache.port_mappings.contains_key(&(proxy_port, protocol)) {
                debug!(
                    "Using cached default endpoint for port {} ({})",
                    proxy_port, protocol
                );
                (cache.address.clone(), cache.port_mappings.clone())
            } else {
                // Port not in cache, need to query
                drop(cached_endpoint);
                let (address, mappings) = self.query_default_endpoint().await?;
                if !mappings.contains_key(&(proxy_port, protocol)) {
                    anyhow::bail!(
                        "Default endpoint does not support port {} ({})",
                        proxy_port,
                        protocol
                    );
                }
                (address, mappings)
            }
        } else {
            // Cache miss - need to query and cache
            drop(cached_endpoint); // Release read lock

            debug!("Cache miss, querying for default endpoint");
            let (address, mappings) = self.query_default_endpoint().await?;

            // Cache the result
            let mut cache_write = self.default_endpoint_cache.write().await;
            *cache_write = Some(DefaultEndpointCache {
                address: address.clone(),
                port_mappings: mappings.clone(),
            });
            drop(cache_write);

            info!(
                "Cached default endpoint: {} ({} ports)",
                address,
                mappings.len()
            );

            if !mappings.contains_key(&(proxy_port, protocol)) {
                anyhow::bail!(
                    "Default endpoint does not support port {} ({})",
                    proxy_port,
                    protocol
                );
            }
            (address, mappings)
        };

        // Create multi-port session for default endpoint
        self.session_manager
            .upsert_multi_port(client_addr, target_ip.clone(), port_mappings)
            .await;

        // Increment load balancer session count
        self.load_balancer.increment_session(&target_ip);

        info!(
            "New session to default endpoint: {} -> {} (port {} {})",
            client_addr, target_ip, proxy_port, protocol
        );

        Ok(())
    }

    /// Query Kubernetes for the default endpoint with multi-port support
    async fn query_default_endpoint(&self) -> Result<(String, HashMap<(u16, Protocol), u16>)> {
        let default_endpoint = self.config.get_default_endpoint();

        let mapping = self
            .config
            .resource_query_mapping
            .get(&default_endpoint.resource_type)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown resource type in default_endpoint: {}",
                    default_endpoint.resource_type
                )
            })?;

        debug!(
            "Querying for default endpoint: type={}, namespace={}, labels={:?}",
            default_endpoint.resource_type,
            default_endpoint.namespace,
            default_endpoint.label_selector
        );

        let status_query =
            default_endpoint
                .status_query
                .as_ref()
                .map(|sq| crate::k8s_client::StatusQuery {
                    json_path: sq.json_path.clone(),
                    expected_values: sq.expected_values.clone(),
                });

        let resources = self
            .k8s_client
            .query_resources(
                &default_endpoint.namespace,
                mapping,
                status_query.as_ref(),
                default_endpoint.label_selector.as_ref(),
                default_endpoint.annotation_selector.as_ref(),
            )
            .await?;

        debug!("Query returned {} resources", resources.len());

        if resources.is_empty() {
            anyhow::bail!("No matching resources found for default endpoint");
        }

        // Use load balancer to select the best backend
        let address_path = mapping
            .address_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("address_path is required for default endpoint"))?;

        let selected_resource = self.load_balancer.select_backend(
            &resources,
            address_path,
            mapping.address_type.as_deref(),
        )?;
        self.extract_endpoint_target_multi_port(
            &selected_resource,
            mapping,
            &default_endpoint.namespace,
        )
        .await
    }

    /// Extract target address and ports from a resource (multi-port)
    async fn extract_endpoint_target_multi_port(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        _namespace: &str,
    ) -> Result<(String, HashMap<(u16, Protocol), u16>)> {
        let address_path = mapping
            .address_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("address_path is required for default endpoint"))?;

        let address = self.k8s_client.extract_address(
            resource,
            address_path,
            mapping.address_type.as_deref(),
        )?;

        // Check if multi-port configuration exists
        if let Some(port_mappings_config) = &mapping.ports {
            // Multi-port approach
            let ports_map = self
                .k8s_client
                .extract_ports(resource, port_mappings_config)?;

            // Build port mappings
            let data_ports = self.config.get_data_ports();
            let mut port_mappings = HashMap::new();

            for data_port_config in &data_ports {
                if let Some(target_port) = ports_map.get(&data_port_config.name) {
                    port_mappings.insert(
                        (data_port_config.port, data_port_config.protocol),
                        *target_port,
                    );
                }
            }

            Ok((address, port_mappings))
        } else {
            // Single port approach (backwards compatibility)
            let port = self.k8s_client.extract_port(
                resource,
                mapping.port_path.as_deref(),
                mapping.port_name.as_deref(),
            )?;

            let mut port_mappings = HashMap::new();
            let data_ports = self.config.get_data_ports();
            for data_port_config in &data_ports {
                port_mappings.insert((data_port_config.port, data_port_config.protocol), port);
            }

            Ok((address, port_mappings))
        }
    }

    /// Extract target address and port from a resource
    #[allow(dead_code)]
    async fn extract_endpoint_target(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        namespace: &str,
    ) -> Result<(String, u16)> {
        if let Some(address_path) = &mapping.address_path {
            self.extract_direct_endpoint(resource, mapping, address_path)
        } else {
            self.extract_service_endpoint(resource, mapping, namespace)
                .await
        }
    }

    /// Extract target using direct resource approach
    #[allow(dead_code)]
    fn extract_direct_endpoint(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        address_path: &str,
    ) -> Result<(String, u16)> {
        let address = self.k8s_client.extract_address(
            resource,
            address_path,
            mapping.address_type.as_deref(),
        )?;
        let port = self.k8s_client.extract_port(
            resource,
            mapping.port_path.as_deref(),
            mapping.port_name.as_deref(),
        )?;
        Ok((address, port))
    }

    /// Extract endpoint using service-based approach
    #[allow(dead_code)]
    async fn extract_service_endpoint(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        namespace: &str,
    ) -> Result<(String, u16)> {
        let resource_name = resource
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let service_selector = mapping.service_selector_label.as_ref().ok_or_else(|| {
            anyhow::anyhow!("service_selector_label required for service-based approach")
        })?;

        let service_port_name = mapping.service_target_port_name.as_ref().ok_or_else(|| {
            anyhow::anyhow!("service_target_port_name required for service-based approach")
        })?;

        self.k8s_client
            .find_service_for_resource(
                namespace,
                &resource_name,
                service_selector,
                service_port_name,
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("No service found for default endpoint resource"))
    }

    /// Proxy a packet using dedicated socket for bi-directional communication
    async fn proxy_packet_bidirectional(
        &self,
        proxy_socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        packet_data: Vec<u8>,
        proxy_port: u16,
    ) -> Result<()> {
        // Get mutable session to create/get dedicated socket (by IP only)
        let mut session_ref = self
            .session_manager
            .get_mut_by_addr(&client_addr)
            .ok_or_else(|| anyhow::anyhow!("Session not found for client {}", client_addr.ip()))?;

        // Get target address
        let target_addr = session_ref.get_target_addr(proxy_port, Protocol::Udp)?;

        // Get or create dedicated socket for this session/port
        let session_socket = session_ref
            .get_or_create_udp_socket(proxy_port, client_addr, proxy_socket.clone())
            .await?;

        debug!(
            "Proxying packet via dedicated socket: {} -> {} ({} bytes)",
            client_addr,
            target_addr,
            packet_data.len()
        );

        // Send packet to target using dedicated socket
        // The receive task is already running to handle responses
        session_socket
            .socket()
            .send_to(&packet_data, target_addr)
            .await?;

        Ok(())
    }
}

// Manual Clone implementation
impl Clone for DataProxy {
    fn clone(&self) -> Self {
        Self {
            data_ports: self.data_ports.clone(),
            session_manager: self.session_manager.clone(),
            config: self.config.clone(),
            k8s_client: self.k8s_client.clone(),
            default_endpoint_cache: self.default_endpoint_cache.clone(),
            load_balancer: self.load_balancer.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_server::QueryServer;
    use http_body_util::Full;
    use hyper::{Response, body::Bytes, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn tcp_label_query_then_unmodified_udp_reaches_selected_pod() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let backend = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let backend_port = backend.local_addr().unwrap().port();
            let gameplay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let data_addr = gameplay.local_addr().unwrap();
            let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let api_addr = api.local_addr().unwrap();
            let pod_list = serde_json::json!({
                "apiVersion":"v1", "kind":"PodList", "metadata":{},
                "items":[{
                    "apiVersion":"v1", "kind":"Pod",
                    "metadata":{"name":"selected-server", "namespace":"games", "uid":"pod-1", "labels":{"map":"tutorial"}},
                    "spec":{"containers":[{"name":"game", "ports":[{"name":"game", "containerPort":backend_port}]}]},
                    "status":{"phase":"Running", "podIP":"127.0.0.1", "conditions":[{"type":"Ready", "status":"True"}]}
                }]
            }).to_string();
            let api_task = tokio::spawn(async move {
                let (stream, _) = api.accept().await.unwrap();
                http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    assert_eq!(request.uri().path(), "/api/v1/namespaces/games/pods");
                    assert!(request.uri().query().unwrap().contains("labelSelector=map%3Dtutorial"));
                    let body = pod_list.clone();
                    async move { Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from(body)))) }
                })).await.unwrap();
            });
            let client = kube::Client::try_from(kube::Config::new(format!("http://{api_addr}").parse().unwrap())).unwrap();
            let k8s = K8sClient::from_client(client);
            let config: Config = serde_yaml::from_str(&format!(r#"
queryPort: 9000
dataPort: {}
sessionTimeoutSeconds: 300
defaultEndpoint:
  resourceType: pod
  namespace: games
resourceQueryMapping:
  pod:
    group: ""
    version: v1
    resource: pods
    addressPath: status.podIP
    portName: game
"#, data_addr.port())).unwrap();
            let sessions = SessionManager::new(300);
            let query = QueryServer::new(9000, k8s.clone(), sessions.clone(), config.clone());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let query_addr = listener.local_addr().unwrap();
            let query_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                query.handle_connection(stream).await.unwrap();
            });
            let mut tcp = TcpStream::connect(query_addr).await.unwrap();
            tcp.write_all(br#"{"type":"query","resourceType":"pod","namespace":"games","labelSelector":{"map":"tutorial"}}"#).await.unwrap();
            let mut reply = Vec::new();
            tcp.read_to_end(&mut reply).await.unwrap();
            let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
            assert_eq!(reply["status"], "ready");
            assert_eq!(reply["server"], "selected-server");
            assert_eq!(reply["ports"]["default"], data_addr.port());
            assert!(reply.get("token").is_none());
            query_task.await.unwrap();

            let proxy = DataProxy::new(sessions.clone(), config, k8s, DefaultEndpointCacheHandle::new());
            let proxy_task = tokio::spawn(async move { proxy.run_udp_socket(gameplay, data_addr.port()).await.unwrap() });
            let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let payload = b"\xff\xff\xff\xffRESETordinary-game-bytes";
            udp.send_to(payload, data_addr).await.unwrap();
            let mut buffer = [0u8; 128];
            let (size, upstream) = backend.recv_from(&mut buffer).await.unwrap();
            assert_eq!(&buffer[..size], payload);
            backend.send_to(b"server-reply", upstream).await.unwrap();
            let (size, source) = udp.recv_from(&mut buffer).await.unwrap();
            assert_eq!(&buffer[..size], b"server-reply");
            assert_eq!(source, data_addr);
            proxy_task.abort();
            api_task.abort();
            sessions.clear_all().await;
        }).await.expect("query and UDP forwarding should complete");
    }
}
