use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info};

use crate::characters::{CharacterServer, characters_on_pod, valid_character_id};
use crate::config::Config;
use crate::k8s_client::{K8sClient, StatusQuery};
use crate::session::SessionManager;

/// Query request from client
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum QueryRequest {
    /// Read-only search for Pods containing any requested character.
    CharacterList {
        namespace: String,
        character_ids: Vec<String>,
        label_selector: Option<HashMap<String, String>>,
    },
    /// Query for a resource and establish a session
    Query {
        resource_type: String,
        namespace: String,
        status_query: Option<StatusQueryDto>,
        label_selector: Option<HashMap<String, String>>,
        annotation_selector: Option<HashMap<String, String>>,
    },
}

/// Status query DTO
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusQueryDto {
    pub json_path: String,
    pub expected_values: Vec<String>,
}

/// Query response to client (single port - backwards compatibility)
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QueryResponse {
    CharacterList {
        servers: Vec<CharacterServer>,
    },
    Success {
        status: String,
        server: String,
        ports: HashMap<String, u16>,
    },
    Error {
        error: String,
    },
}

/// TCP Query Server (Phase 1)
/// Establishes the route during the TCP query, before gameplay begins
pub struct QueryServer {
    port: u16,
    k8s_client: K8sClient,
    session_manager: SessionManager,
    config: Config,
}

impl QueryServer {
    /// Create a new query server
    pub fn new(
        port: u16,
        k8s_client: K8sClient,
        session_manager: SessionManager,
        config: Config,
    ) -> Self {
        Self {
            port,
            k8s_client,
            session_manager,
            config,
        }
    }

    /// Run the query server
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port))
            .await
            .with_context(|| format!("Failed to bind query server to port {}", self.port))?;

        info!("Query server listening on port {}", self.port);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    debug!("New query connection from {}", addr);
                    let server = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = server.handle_connection(stream).await {
                            error!("Error handling query connection: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    /// Handle a single query connection
    pub(crate) async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        // Get client address for session establishment
        let client_addr = stream.peer_addr()?;

        let request = match read_request(&mut stream).await {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                let response = QueryResponse::Error {
                    error: error.to_string(),
                };
                stream.write_all(&serde_json::to_vec(&response)?).await?;
                return Ok(());
            }
        };

        debug!("Received query: {:?}", request);

        // Process the query and establish session
        let response = self.process_query(request, client_addr).await;
        let response_json = serde_json::to_string(&response)?;

        // Send response
        stream.write_all(response_json.as_bytes()).await?;
        stream.flush().await?;

        Ok(())
    }

    /// Process a query request and establish session for client
    async fn process_query(
        &self,
        request: QueryRequest,
        client_addr: std::net::SocketAddr,
    ) -> QueryResponse {
        match request {
            QueryRequest::CharacterList {
                namespace,
                character_ids,
                label_selector,
            } => {
                if character_ids.iter().any(|id| !valid_character_id(id)) {
                    return QueryResponse::Error {
                        error: "Invalid character ID for a Kubernetes label".into(),
                    };
                }
                let pods = match self
                    .k8s_client
                    .list_pods(&namespace, label_selector.as_ref())
                    .await
                {
                    Ok(pods) => pods,
                    Err(error) => {
                        return QueryResponse::Error {
                            error: error.to_string(),
                        };
                    }
                };
                let now = time::OffsetDateTime::now_utc();
                let mut servers: Vec<_> = pods
                    .into_iter()
                    .filter_map(|pod| {
                        let characters = characters_on_pod(&pod, &self.config, &character_ids, now);
                        if characters.is_empty() {
                            return None;
                        }
                        Some(CharacterServer {
                            namespace: namespace.clone(),
                            name: pod.metadata.name.unwrap_or_default(),
                            characters,
                        })
                    })
                    .collect();
                servers.sort_by(|a, b| a.name.cmp(&b.name));
                QueryResponse::CharacterList { servers }
            }
            QueryRequest::Query {
                resource_type,
                namespace,
                status_query,
                label_selector,
                annotation_selector,
            } => {
                self.process_resource_query(
                    resource_type,
                    namespace,
                    status_query,
                    label_selector,
                    annotation_selector,
                    client_addr,
                )
                .await
            }
        }
    }

    /// Process a resource query request
    async fn process_resource_query(
        &self,
        resource_type: String,
        namespace: String,
        status_query: Option<StatusQueryDto>,
        label_selector: Option<HashMap<String, String>>,
        annotation_selector: Option<HashMap<String, String>>,
        client_addr: std::net::SocketAddr,
    ) -> QueryResponse {
        let mapping = match self.config.resource_query_mapping.get(&resource_type) {
            Some(m) => m,
            None => {
                return QueryResponse::Error {
                    error: format!("Unknown resource type: {}", resource_type),
                };
            }
        };

        let status_query_obj = status_query.as_ref().map(|sq| StatusQuery {
            json_path: sq.json_path.clone(),
            expected_values: sq.expected_values.clone(),
        });

        let resources = match self
            .query_k8s_resources(
                &resource_type,
                &namespace,
                &label_selector,
                &annotation_selector,
                mapping,
                status_query_obj.as_ref(),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => return e,
        };

        let selected_resource = &resources[0];
        let resource_name = selected_resource
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        debug!("Selected resource: {}", resource_name);

        // Check if multi-port configuration is available
        if mapping.ports.is_some() {
            // Multi-port approach
            let (cluster_ip, ports_map) = match self
                .extract_multi_port_target_info(
                    selected_resource,
                    mapping,
                    &namespace,
                    &resource_name,
                )
                .await
            {
                Ok(info) => info,
                Err(e) => return e,
            };

            // Build port mappings for the selected target
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

            // Establish session immediately for this client
            self.session_manager
                .upsert_multi_port(client_addr, cluster_ip.clone(), port_mappings)
                .await;

            info!(
                "Established multi-port route for {} -> {} ({} ports)",
                client_addr,
                resource_name,
                ports_map.len()
            );

            QueryResponse::Success {
                status: "ready".to_string(),
                server: resource_name,
                ports: self
                    .config
                    .get_data_ports()
                    .into_iter()
                    .map(|p| (p.name, p.port))
                    .collect(),
            }
        } else {
            // Single port approach (backwards compatibility)
            let (cluster_ip, port) = match self
                .extract_target_info(selected_resource, mapping, &namespace, &resource_name)
                .await
            {
                Ok(info) => info,
                Err(e) => return e,
            };

            let port_mappings = self
                .config
                .get_data_ports()
                .into_iter()
                .map(|p| ((p.port, p.protocol), port))
                .collect();
            self.session_manager
                .upsert_multi_port(client_addr, cluster_ip, port_mappings)
                .await;

            QueryResponse::Success {
                status: "ready".to_string(),
                server: resource_name,
                ports: self
                    .config
                    .get_data_ports()
                    .into_iter()
                    .map(|p| (p.name, p.port))
                    .collect(),
            }
        }
    }

    /// Query Kubernetes for matching resources
    async fn query_k8s_resources(
        &self,
        _resource_type: &str,
        namespace: &str,
        label_selector: &Option<HashMap<String, String>>,
        annotation_selector: &Option<HashMap<String, String>>,
        mapping: &crate::config::ResourceMapping,
        status_query: Option<&StatusQuery>,
    ) -> Result<Vec<kube::api::DynamicObject>, QueryResponse> {
        let mut resources = self
            .k8s_client
            .query_resources(
                namespace,
                mapping,
                status_query,
                label_selector.as_ref(),
                annotation_selector.as_ref(),
            )
            .await
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to query resources: {}", e),
            })?;

        if mapping.group.is_empty() && mapping.resource == "pods" {
            resources.retain(|resource| {
                serde_json::to_value(resource)
                    .ok()
                    .and_then(|value| serde_json::from_value(value).ok())
                    .is_some_and(|pod| K8sClient::pod_ready(&pod))
            });
        }

        if resources.is_empty() {
            return Err(QueryResponse::Error {
                error: "No matching resources found".to_string(),
            });
        }

        Ok(resources)
    }

    /// Extract target IP and port from resource
    async fn extract_target_info(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        namespace: &str,
        resource_name: &str,
    ) -> Result<(String, u16), QueryResponse> {
        if let Some(address_path) = &mapping.address_path {
            self.extract_direct_target(resource, mapping, address_path)
        } else {
            self.extract_service_target(namespace, resource_name, mapping)
                .await
        }
    }

    /// Extract target using direct resource approach
    fn extract_direct_target(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        address_path: &str,
    ) -> Result<(String, u16), QueryResponse> {
        debug!(
            "Using direct resource approach with address_path: {}",
            address_path
        );

        let address = self
            .k8s_client
            .extract_address(resource, address_path, mapping.address_type.as_deref())
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to extract address: {}", e),
            })?;

        let port = self
            .k8s_client
            .extract_port(
                resource,
                mapping.port_path.as_deref(),
                mapping.port_name.as_deref(),
            )
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to extract port: {}", e),
            })?;

        debug!("Extracted address: {}, port: {}", address, port);
        Ok((address, port))
    }

    /// Extract target using service-based approach
    async fn extract_service_target(
        &self,
        namespace: &str,
        resource_name: &str,
        mapping: &crate::config::ResourceMapping,
    ) -> Result<(String, u16), QueryResponse> {
        debug!("Using service-based approach");

        let selector =
            mapping
                .service_selector_label
                .as_ref()
                .ok_or_else(|| QueryResponse::Error {
                    error: "service_selector_label is required for service-based approach"
                        .to_string(),
                })?;

        let port_name =
            mapping
                .service_target_port_name
                .as_ref()
                .ok_or_else(|| QueryResponse::Error {
                    error: "service_target_port_name is required for service-based approach"
                        .to_string(),
                })?;

        self.k8s_client
            .find_service_for_resource(namespace, resource_name, selector, port_name)
            .await
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to find service: {}", e),
            })?
            .ok_or_else(|| QueryResponse::Error {
                error: format!("No service found for resource: {}", resource_name),
            })
    }

    /// Extract multi-port target information from resource
    async fn extract_multi_port_target_info(
        &self,
        resource: &kube::api::DynamicObject,
        mapping: &crate::config::ResourceMapping,
        _namespace: &str,
        _resource_name: &str,
    ) -> Result<(String, HashMap<String, u16>), QueryResponse> {
        let address_path = mapping
            .address_path
            .as_ref()
            .ok_or_else(|| QueryResponse::Error {
                error: "address_path is required for multi-port approach".to_string(),
            })?;

        let port_mappings = mapping.ports.as_ref().ok_or_else(|| QueryResponse::Error {
            error: "ports configuration is required for multi-port approach".to_string(),
        })?;

        debug!("Using direct multi-port resource approach");

        let address = self
            .k8s_client
            .extract_address(resource, address_path, mapping.address_type.as_deref())
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to extract address: {}", e),
            })?;

        let ports = self
            .k8s_client
            .extract_ports(resource, port_mappings)
            .map_err(|e| QueryResponse::Error {
                error: format!("Failed to extract ports: {}", e),
            })?;

        debug!("Extracted address: {}, ports: {:?}", address, ports);
        Ok((address, ports))
    }
}

/// One JSON document per TCP connection. TCP reads are not message boundaries.
async fn read_request<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<Option<QueryRequest>> {
    let mut payload = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            if payload.is_empty() {
                return Ok(None);
            }
            anyhow::bail!("Incomplete JSON request");
        }
        payload.extend_from_slice(&chunk[..n]);
        if payload.len() > 65536 {
            anyhow::bail!("Query exceeds 65536 bytes");
        }
        match serde_json::from_slice(&payload) {
            Ok(request) => return Ok(Some(request)),
            Err(error) if error.is_eof() => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

// Manual Clone implementation since TcpListener is not Clone
impl Clone for QueryServer {
    fn clone(&self) -> Self {
        Self {
            port: self.port,
            k8s_client: self.k8s_client.clone(),
            session_manager: self.session_manager.clone(),
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_request_serialization() {
        // Test what the correct format should be
        let mut label_selector = HashMap::new();
        label_selector.insert("game.example.com/map".to_string(), "de_dust2".to_string());

        let request = QueryRequest::Query {
            resource_type: "gameserver".to_string(),
            namespace: "game-servers".to_string(),
            status_query: Some(StatusQueryDto {
                json_path: "status.state".to_string(),
                expected_values: vec!["Allocated".to_string(), "Ready".to_string()],
            }),
            label_selector: Some(label_selector),
            annotation_selector: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        println!("Serialized JSON: {}", json);

        // Now deserialize it back
        let deserialized: QueryRequest = serde_json::from_str(&json).unwrap();
        match deserialized {
            QueryRequest::Query {
                resource_type,
                namespace,
                status_query,
                label_selector,
                annotation_selector: _,
            } => {
                assert_eq!(resource_type, "gameserver");
                assert_eq!(namespace, "game-servers");
                assert!(status_query.is_some());
                assert!(label_selector.is_some());

                let sq = status_query.unwrap();
                assert_eq!(sq.expected_values.len(), 2);
                assert_eq!(sq.expected_values[0], "Allocated");
                assert_eq!(sq.expected_values[1], "Ready");
            }
            _ => panic!("Expected Query variant"),
        }
    }

    #[test]
    fn rejects_removed_ticket_requests() {
        for kind in ["allocation", "sessionReset"] {
            let value = serde_json::json!({"type": kind, "token": "obsolete"});
            assert!(serde_json::from_value::<QueryRequest>(value).is_err());
        }
    }

    #[tokio::test]
    async fn reads_fragmented_character_list_larger_than_one_tcp_read() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        let ids: Vec<_> = (0..128).map(|i| format!("character-{i:050}")).collect();
        let payload = serde_json::to_vec(&serde_json::json!({
            "type": "characterList", "namespace": "games", "characterIds": ids
        }))
        .unwrap();
        assert!(payload.len() > 4096);
        let sending = tokio::spawn(async move {
            for chunk in payload.chunks(17) {
                writer.write_all(chunk).await.unwrap();
            }
        });
        let request = read_request(&mut reader).await.unwrap().unwrap();
        sending.await.unwrap();
        match request {
            QueryRequest::CharacterList { character_ids, .. } => {
                assert_eq!(character_ids.len(), 128)
            }
            _ => panic!("expected characterList"),
        }
    }

    #[tokio::test]
    async fn truncated_request_is_not_processed() {
        let mut reader = &b"{\"type\":\"query\""[..];
        assert!(read_request(&mut reader).await.is_err());
    }
}
