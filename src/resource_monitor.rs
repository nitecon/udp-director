use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::k8s_client::{K8sClient, StatusQuery};
use crate::proxy::DefaultEndpointCacheHandle;
use crate::session::SessionManager;

/// Resource monitor that watches for changes to default endpoint and active sessions
pub struct ResourceMonitor {
    config: Config,
    k8s_client: K8sClient,
    session_manager: SessionManager,
    check_interval_seconds: u64,
    last_default_endpoint: Arc<tokio::sync::RwLock<Option<String>>>,
    cache_handle: DefaultEndpointCacheHandle,
}

impl ResourceMonitor {
    /// Create a new resource monitor
    pub fn new(
        config: Config,
        k8s_client: K8sClient,
        session_manager: SessionManager,
        check_interval_seconds: u64,
        cache_handle: DefaultEndpointCacheHandle,
    ) -> Self {
        Self {
            config,
            k8s_client,
            session_manager,
            check_interval_seconds,
            last_default_endpoint: Arc::new(tokio::sync::RwLock::new(None)),
            cache_handle,
        }
    }

    /// Run the resource monitor
    pub async fn run(self) -> Result<()> {
        let monitor = Arc::new(self);

        info!(
            "Resource monitor started (checking every {} seconds)",
            monitor.check_interval_seconds
        );

        let mut check_interval = interval(Duration::from_secs(monitor.check_interval_seconds));

        loop {
            check_interval.tick().await;

            // Check default endpoint
            if let Err(e) = monitor.check_default_endpoint().await {
                error!("Error checking default endpoint: {}", e);
            }

            // Check active sessions
            if let Err(e) = monitor.check_active_sessions().await {
                error!("Error checking active sessions: {}", e);
            }
        }
    }

    /// Check if the default endpoint is still valid
    async fn check_default_endpoint(&self) -> Result<()> {
        let default_endpoint = self.config.get_default_endpoint();

        let mapping = match self
            .config
            .resource_query_mapping
            .get(&default_endpoint.resource_type)
        {
            Some(m) => m,
            None => {
                warn!(
                    "Default endpoint resource type '{}' not found in mapping",
                    default_endpoint.resource_type
                );
                return Ok(());
            }
        };

        // Convert status query
        let status_query = default_endpoint
            .status_query
            .as_ref()
            .map(|sq| StatusQuery {
                json_path: sq.json_path.clone(),
                expected_values: sq.expected_values.clone(),
            });

        // Query for matching resources
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

        // Get the current default endpoint target
        let current_target = if resources.is_empty() {
            None
        } else {
            // Extract the first resource's target
            let resource = &resources[0];
            let resource_name = resource.metadata.name.as_deref().unwrap_or("unknown");

            // Try to extract address and port
            if let Some(address_path) = &mapping.address_path {
                match self.k8s_client.extract_address(
                    resource,
                    address_path,
                    mapping.address_type.as_deref(),
                ) {
                    Ok(address) => {
                        match self.k8s_client.extract_port(
                            resource,
                            mapping.port_path.as_deref(),
                            mapping.port_name.as_deref(),
                        ) {
                            Ok(port) => Some(format!("{} ({}:{})", resource_name, address, port)),
                            Err(_) => Some(resource_name.to_string()),
                        }
                    }
                    Err(_) => Some(resource_name.to_string()),
                }
            } else {
                Some(resource_name.to_string())
            }
        };

        // Compare with last known state
        let mut last_endpoint = self.last_default_endpoint.write().await;

        match (&*last_endpoint, &current_target) {
            (None, None) => {
                // Still no resources - already warned on startup
                debug!("Default endpoint check: still no matching resources");
            }
            (Some(last), None) => {
                // Resources disappeared - invalidate cache
                warn!("⚠️  Default endpoint lost! Previous: {}", last);
                warn!(
                    "No matching resources found for default endpoint (type: {}, namespace: {})",
                    default_endpoint.resource_type, default_endpoint.namespace
                );
                self.cache_handle.invalidate().await;
                info!("Invalidated default endpoint cache");
                *last_endpoint = None;
            }
            (None, Some(current)) => {
                // Resources appeared - invalidate cache to force refresh
                info!("✓ Default endpoint found: {}", current);
                info!(
                    "Default endpoint now available ({} resource(s) match)",
                    resources.len()
                );
                self.cache_handle.invalidate().await;
                info!("Invalidated default endpoint cache to force refresh");
                *last_endpoint = current_target;
            }
            (Some(last), Some(current)) => {
                if last != current {
                    // Resources changed - invalidate cache to force refresh
                    info!("🔄 Default endpoint changed: {} → {}", last, current);
                    self.cache_handle.invalidate().await;
                    info!("Invalidated default endpoint cache to force refresh");
                    *last_endpoint = current_target;
                } else {
                    // No change
                    debug!(
                        "Default endpoint check: {} resource(s) available ({})",
                        resources.len(),
                        current
                    );
                }
            }
        }

        Ok(())
    }

    /// Check active sessions and reconnect if targets are unavailable
    async fn check_active_sessions(&self) -> Result<()> {
        // This is a placeholder for session health checking
        // In a full implementation, we would:
        // 1. Iterate through active sessions
        // 2. Check if the target is still reachable
        // 3. If not, query for a replacement resource
        // 4. Update the session with the new target

        // For now, we'll just log the session count
        let session_count = self.session_manager.count();
        if session_count > 0 {
            debug!("Active sessions: {}", session_count);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_resource_monitor_creation() {
        // This test requires a k8s environment, so we'll skip if not available
        if K8sClient::new().await.is_err() {
            return;
        }

        let config = crate::config::Config {
            query_port: 9000,
            character_label_prefix: "characters.udp-director.io".into(),
            disconnect_annotation: "udp-director.io/disconnected-at".into(),
            data_port: Some(7777),
            data_ports: None,
            default_endpoint: crate::config::DefaultEndpoint {
                resource_type: "gameserver".to_string(),
                namespace: "default".to_string(),
                label_selector: None,
                annotation_selector: None,
                status_query: None,
            },
            session_timeout_seconds: 300,
            resource_query_mapping: HashMap::new(),
            load_balancing: None,
        };

        let k8s_client = K8sClient::new().await.unwrap();
        let session_manager = crate::session::SessionManager::new(300);
        let cache_handle = DefaultEndpointCacheHandle::new();

        let _monitor = ResourceMonitor::new(config, k8s_client, session_manager, 10, cache_handle);
        // Just verify it can be created
    }
}
