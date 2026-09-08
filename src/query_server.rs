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
    deny_unknown_fields,
    tag = "type"
)]
pub enum QueryRequest {
    CharacterList {
        map: String,
        #[serde(default)]
        character_ids: Vec<String>,
    },
    Query {
        map: String,
        character_id: String,
        #[serde(default)]
        friend_ids: Vec<String>,
    },
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
        let result = self.process_access(request, client_addr).await;
        match result {
            Ok(response) => response,
            Err(error) => {
                error!("Access query failed: {error:#}");
                QueryResponse::Error {
                    error: "Unable to establish route".into(),
                }
            }
        }
    }

    async fn process_access(
        &self,
        request: QueryRequest,
        client_addr: std::net::SocketAddr,
    ) -> Result<QueryResponse> {
        let (map, ids, character) = match request {
            QueryRequest::CharacterList { map, character_ids } => (map, character_ids, None),
            QueryRequest::Query {
                map,
                character_id,
                friend_ids,
            } => (map, friend_ids, Some(character_id)),
        };
        if !valid_character_id(&map)
            || ids.iter().any(|id| !valid_character_id(id))
            || character.as_ref().is_some_and(|id| !valid_character_id(id))
        {
            return Ok(QueryResponse::Error {
                error: "Invalid map or character ID".into(),
            });
        }
        let endpoint = &self.config.default_endpoint;
        let mapping = self
            .config
            .resource_query_mapping
            .get(&endpoint.resource_type)
            .context("Missing server mapping")?;
        anyhow::ensure!(
            mapping.group.is_empty() && mapping.resource == "pods",
            "Access requires a Pod mapping"
        );
        let mut labels = endpoint.label_selector.clone().unwrap_or_default();
        labels.insert(self.config.map_label.clone(), map);
        let status = endpoint.status_query.as_ref().map(|s| StatusQuery {
            json_path: s.json_path.clone(),
            expected_values: s.expected_values.clone(),
        });
        let resources = self
            .k8s_client
            .query_resources(
                &endpoint.namespace,
                mapping,
                status.as_ref(),
                Some(&labels),
                endpoint.annotation_selector.as_ref(),
            )
            .await?;
        let now = time::OffsetDateTime::now_utc();
        let mut candidates = Vec::new();
        for resource in resources {
            let pod: k8s_openapi::api::core::v1::Pod =
                serde_json::from_value(serde_json::to_value(&resource)?)?;
            if K8sClient::pod_ready(&pod) {
                let characters = characters_on_pod(&pod, &self.config, &[], now);
                candidates.push((resource, pod, characters));
            }
        }
        let Some(character) = character else {
            let mut servers = Vec::new();
            for (_, pod, mut characters) in candidates {
                characters.retain(|c| ids.is_empty() || ids.contains(&c.character_id));
                if !characters.is_empty() {
                    servers.push(CharacterServer {
                        server: pod.metadata.uid.context("Missing server identity")?,
                        characters,
                    });
                }
            }
            servers.sort_by(|a, b| a.server.cmp(&b.server));
            return Ok(QueryResponse::CharacterList { servers });
        };
        candidates.retain(|(_, _, chars)| {
            chars.iter().any(|c| c.character_id == character)
                || chars.len() < self.config.max_characters_per_server
        });
        candidates.sort_by_key(|(_, pod, chars)| {
            (
                std::cmp::Reverse(chars.iter().any(|c| c.character_id == character)),
                std::cmp::Reverse(
                    chars
                        .iter()
                        .filter(|c| ids.contains(&c.character_id))
                        .count(),
                ),
                chars.len(),
                pod.metadata.uid.clone(),
            )
        });
        let Some((resource, pod, characters)) = candidates.first() else {
            return Ok(QueryResponse::Error {
                error: "No server available".into(),
            });
        };
        let name = pod.metadata.name.as_deref().context("Missing Pod name")?;
        let (ip, ports) = if mapping.ports.is_some() {
            self.extract_multi_port_target_info(resource, mapping, &endpoint.namespace, name)
                .await
                .map_err(|_| anyhow::anyhow!("Invalid backend ports"))?
        } else {
            let (ip, port) = self
                .extract_target_info(resource, mapping, &endpoint.namespace, name)
                .await
                .map_err(|_| anyhow::anyhow!("Invalid backend port"))?;
            (
                ip,
                self.config
                    .get_data_ports()
                    .into_iter()
                    .map(|p| (p.name, port))
                    .collect(),
            )
        };
        let mut routes = HashMap::new();
        let mut public_ports = HashMap::new();
        for port in self.config.get_data_ports() {
            let target = ports.get(&port.name).context("Missing backend port")?;
            routes.insert((port.port, port.protocol), *target);
            public_ports.insert(port.name, port.port);
        }
        anyhow::ensure!(!routes.is_empty(), "No data ports");
        let key = format!("{}/{}", self.config.character_label_prefix, character);
        let server = pod
            .metadata
            .uid
            .clone()
            .context("Missing server identity")?;
        // A repeated query must not demote an actual connected player.
        if !characters.iter().any(|c| {
            c.character_id == character && c.status == crate::characters::CharacterStatus::Used
        }) {
            self.k8s_client
                .allocate_character(&endpoint.namespace, pod, &key)
                .await?;
        }
        self.session_manager
            .upsert_multi_port(client_addr, ip, routes)
            .await;
        Ok(QueryResponse::Success {
            status: "Allocated".into(),
            server,
            ports: public_ports,
        })
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
    fn rejects_infrastructure_fields() {
        assert!(
            serde_json::from_str::<QueryRequest>(
                r#"{"type":"query","map":"tutorial","characterId":"me","namespace":"private"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<QueryRequest>(
                r#"{"type":"query","map":"tutorial","characterId":"me","friendIds":["friend"]}"#
            )
            .is_ok()
        );
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
            "type": "characterList", "map": "tutorial", "characterIds": ids
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
