use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info};

use crate::config::Config;
use crate::k8s_client::{K8sClient, StatusQuery};
use crate::reservation_controller::ReservationRouter;
use crate::session::SessionManager;
use crate::token_cache::{TokenCache, TokenTarget};

/// Query request from client
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum QueryRequest {
    /// Query for a resource and establish a session
    Query {
        resource_type: String,
        namespace: String,
        status_query: Option<StatusQueryDto>,
        label_selector: Option<HashMap<String, String>>,
        annotation_selector: Option<HashMap<String, String>>,
    },
    /// Reset an existing session with a new token
    SessionReset { token: String },
    /// Consume a trusted reservation-controller token.
    Allocation { token: String },
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
    Success {
        token: String,
    },
    SuccessMultiPort {
        token: String,
        address: String,
        ports: HashMap<String, u16>,
    },
    Error {
        error: String,
    },
}

/// TCP Query Server (Phase 1)
/// Now establishes sessions immediately when returning tokens
pub struct QueryServer {
    port: u16,
    k8s_client: K8sClient,
    token_cache: TokenCache,
    session_manager: SessionManager,
    config: Config,
    reservation_router: Option<ReservationRouter>,
}

impl QueryServer {
    /// Create a new query server
    pub fn new(
        port: u16,
        k8s_client: K8sClient,
        token_cache: TokenCache,
        session_manager: SessionManager,
        config: Config,
        reservation_router: Option<ReservationRouter>,
    ) -> Self {
        Self {
            port,
            k8s_client,
            token_cache,
            session_manager,
            config,
            reservation_router,
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
    async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        // Get client address for session establishment
        let client_addr = stream.peer_addr()?;

        // Read the JSON payload
        let mut buffer = vec![0u8; 4096];
        let n = stream
            .read(&mut buffer)
            .await
            .context("Failed to read from stream")?;

        if n == 0 {
            return Ok(());
        }

        let request_data = &buffer[..n];
        let request: QueryRequest = match serde_json::from_slice(request_data) {
            Ok(req) => req,
            Err(e) => {
                let response = QueryResponse::Error {
                    error: format!("Invalid JSON: {}", e),
                };
                let response_json = serde_json::to_string(&response)?;
                stream.write_all(response_json.as_bytes()).await?;
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
            QueryRequest::Query {
                resource_type,
                namespace,
                status_query,
                label_selector,
                annotation_selector,
            } => {
                if self.config.reservation_only {
                    return QueryResponse::Error {
                        error: "controller allocation required".to_string(),
                    };
                }
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
            QueryRequest::SessionReset { token } => {
                if self.config.reservation_only {
                    return QueryResponse::Error {
                        error: "controller allocation required".to_string(),
                    };
                }
                self.process_session_reset(token, client_addr).await
            }
            QueryRequest::Allocation { token } => match &self.reservation_router {
                Some(router) => match router.bind(&token, client_addr, &self.config).await {
                    Ok(()) => QueryResponse::Success { token },
                    Err(error) => QueryResponse::Error {
                        error: error.to_string(),
                    },
                },
                None => QueryResponse::Error {
                    error: "Reservation-controller routing is not configured".to_string(),
                },
            },
        }
    }

    /// Process a session reset request
    async fn process_session_reset(
        &self,
        token: String,
        client_addr: std::net::SocketAddr,
    ) -> QueryResponse {
        // Look up the token
        match self.token_cache.lookup(&token).await {
            Some(target) => {
                // Valid token - update session
                self.session_manager
                    .upsert_multi_port(
                        client_addr,
                        target.cluster_ip.clone(),
                        target.port_mappings.clone(),
                    )
                    .await;
                info!(
                    "Session reset via query port: {} -> {} ({} ports)",
                    client_addr,
                    target.cluster_ip,
                    target.port_mappings.len()
                );
                QueryResponse::Success { token }
            }
            None => QueryResponse::Error {
                error: "Invalid or expired token".to_string(),
            },
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

            // Build port mappings for TokenTarget
            let data_ports = self.config.get_data_ports();
            let mut token_port_mappings = HashMap::new();

            for data_port_config in &data_ports {
                if let Some(target_port) = ports_map.get(&data_port_config.name) {
                    token_port_mappings.insert(
                        (data_port_config.port, data_port_config.protocol),
                        *target_port,
                    );
                }
            }

            let target = TokenTarget::multi_port(cluster_ip.clone(), token_port_mappings.clone());
            let token = self.token_cache.generate_token(target).await;

            // Establish session immediately for this client
            self.session_manager
                .upsert_multi_port(client_addr, cluster_ip.clone(), token_port_mappings)
                .await;

            info!(
                "Generated multi-port token and established session for {} -> {} ({} ports)",
                client_addr,
                resource_name,
                ports_map.len()
            );

            QueryResponse::SuccessMultiPort {
                token,
                address: cluster_ip,
                ports: ports_map,
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

            let target = TokenTarget::single_port(cluster_ip.clone(), port);
            let token = self.token_cache.generate_token(target).await;

            // Establish session immediately for this client
            let target_addr =
                format!("{}:{}", cluster_ip, port)
                    .parse()
                    .map_err(|e| QueryResponse::Error {
                        error: format!("Invalid target address: {}", e),
                    });

            if let Ok(addr) = target_addr {
                self.session_manager.upsert(client_addr, addr).await;
                info!(
                    "Generated token and established session for {} -> {}",
                    client_addr, resource_name
                );
            }

            QueryResponse::Success { token }
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
        let resources = self
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

// Manual Clone implementation since TcpListener is not Clone
impl Clone for QueryServer {
    fn clone(&self) -> Self {
        Self {
            port: self.port,
            k8s_client: self.k8s_client.clone(),
            token_cache: self.token_cache.clone(),
            session_manager: self.session_manager.clone(),
            config: self.config.clone(),
            reservation_router: self.reservation_router.clone(),
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
    fn test_session_reset_request_deserialization() {
        let json = r#"{
            "type": "sessionReset",
            "token": "test-token-123"
        }"#;

        let request: QueryRequest = serde_json::from_str(json).unwrap();
        match request {
            QueryRequest::SessionReset { token } => {
                assert_eq!(token, "test-token-123");
            }
            _ => panic!("Expected SessionReset variant"),
        }
    }

    #[test]
    fn test_query_response_serialization() {
        let response = QueryResponse::Success {
            token: "test-token-123".to_string(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("test-token-123"));

        let response = QueryResponse::Error {
            error: "Test error".to_string(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Test error"));
    }
}
