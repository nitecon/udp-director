use anyhow::Result;
use tokio::signal;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod k8s_client;
mod load_balancer;
mod metrics;
mod metrics_server;
mod project_x;
mod proxy;
mod query_server;
mod resource_monitor;
mod session;
mod token_cache;

use config::Config;
use k8s_client::K8sClient;
use load_balancer::LoadBalancer;
use project_x::ProjectXRouter;
use proxy::{DataProxy, DefaultEndpointCacheHandle};
use query_server::QueryServer;
use resource_monitor::ResourceMonitor;
use session::SessionManager;
use token_cache::TokenCache;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "udp_director=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting UDP Director");

    // Initialize uptime tracking
    let start_time = std::time::Instant::now();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            metrics::UPTIME_SECONDS.set(start_time.elapsed().as_secs_f64());
        }
    });

    // Load initial configuration
    let config = Config::load().await?;
    info!("Configuration loaded successfully");

    // Initialize Kubernetes client
    let k8s_client = K8sClient::new().await?;
    info!("Kubernetes client initialized");

    // Verify default endpoint configuration
    verify_default_endpoint(&config, &k8s_client).await;

    // Initialize shared state
    let token_cache = TokenCache::new(config.token_ttl_seconds);
    let mut session_manager = SessionManager::new(config.session_timeout_seconds);
    let default_endpoint_cache = DefaultEndpointCacheHandle::new();
    let project_x_router = ProjectXRouter::from_env(k8s_client.clone(), session_manager.clone())?;

    // Initialize load balancer for session tracking
    let lb_config = config.get_load_balancing();
    let load_balancer = LoadBalancer::new(lb_config.strategy, k8s_client.clone());

    // Set up cleanup callback to decrement load balancer counts
    let lb_for_callback = load_balancer.clone();
    session_manager.set_cleanup_callback(std::sync::Arc::new(move |target_ip: &str| {
        lb_for_callback.decrement_session(target_ip);
    }));

    // Start Query Server (Phase 1)
    let query_handle = {
        let query_server = QueryServer::new(
            config.query_port,
            k8s_client.clone(),
            token_cache.clone(),
            session_manager.clone(),
            config.clone(),
            project_x_router.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = query_server.run().await {
                warn!("Query server error: {}", e);
            }
        })
    };

    // Start Multi-Port Data Proxy (Phase 2 & 3)
    let proxy_handle = {
        let data_proxy = DataProxy::new(
            token_cache.clone(),
            session_manager.clone(),
            config.clone(),
            k8s_client.clone(),
            default_endpoint_cache.clone(),
            project_x_router.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = data_proxy.run().await {
                warn!("Data proxy error: {}", e);
            }
        })
    };

    // Start Resource Monitor
    let monitor_handle = {
        let resource_monitor = ResourceMonitor::new(
            config.clone(),
            k8s_client.clone(),
            session_manager.clone(),
            10, // Check every 10 seconds
            default_endpoint_cache.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = resource_monitor.run().await {
                warn!("Resource monitor error: {}", e);
            }
        })
    };

    // Start Metrics Server
    let metrics_handle = {
        tokio::spawn(async move {
            if let Err(e) = metrics_server::run_metrics_server(9090).await {
                warn!("Metrics server error: {}", e);
            }
        })
    };

    let project_x_admin_handle = project_x_router.map(|router| {
        tokio::spawn(async move {
            if let Err(error) = router.run_admin_server().await {
                warn!("Project X admin server error: {}", error);
            }
        })
    });

    info!("UDP Director is running");
    info!("Query port: {}", config.query_port);

    // Log all configured data ports
    let data_ports = config.get_data_ports();
    info!("Data ports configured: {}", data_ports.len());
    for port_config in &data_ports {
        info!(
            "  - {} port {} ({})",
            port_config.protocol, port_config.port, port_config.name
        );
    }

    info!("Metrics port: 9090");

    // Wait for shutdown signal or task termination
    tokio::select! {
        _ = shutdown_signal() => {
            info!("Shutdown signal received, initiating graceful shutdown...");
        }
        _ = query_handle => warn!("Query server terminated unexpectedly"),
        _ = proxy_handle => warn!("Data proxy terminated unexpectedly"),
        _ = monitor_handle => warn!("Resource monitor terminated unexpectedly"),
        _ = metrics_handle => warn!("Metrics server terminated unexpectedly"),
        _ = async {
            if let Some(handle) = project_x_admin_handle {
                let _ = handle.await;
            } else {
                std::future::pending::<()>().await;
            }
        } => warn!("Project X admin server terminated unexpectedly"),
    }

    // Perform graceful shutdown
    info!("Shutting down UDP Director...");
    info!("Active sessions at shutdown: {}", session_manager.count());

    // Clear all active sessions
    session_manager.clear_all().await;

    // Give tasks a moment to finish their current operations
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    info!("UDP Director shutdown complete");
    Ok(())
}

/// Verify and display the default endpoint configuration
async fn verify_default_endpoint(config: &Config, k8s_client: &K8sClient) {
    use tracing::error;

    let default_endpoint = config.get_default_endpoint();

    log_endpoint_config(default_endpoint);

    let mapping = match config
        .resource_query_mapping
        .get(&default_endpoint.resource_type)
    {
        Some(m) => m,
        None => {
            error!(
                "  ✗ Resource type '{}' not found in resourceQueryMapping!",
                default_endpoint.resource_type
            );
            error!("  Default endpoint is misconfigured!");
            info!("======================================");
            return;
        }
    };

    log_resource_mapping(mapping);

    let status_query = default_endpoint
        .status_query
        .as_ref()
        .map(|sq| k8s_client::StatusQuery {
            json_path: sq.json_path.clone(),
            expected_values: sq.expected_values.clone(),
        });

    match k8s_client
        .query_resources(
            &default_endpoint.namespace,
            mapping,
            status_query.as_ref(),
            default_endpoint.label_selector.as_ref(),
            default_endpoint.annotation_selector.as_ref(),
        )
        .await
    {
        Ok(resources) => handle_query_success(&resources, k8s_client, mapping),
        Err(e) => handle_query_error(e, default_endpoint, mapping),
    }

    info!("======================================");
}

/// Log the endpoint configuration details
fn log_endpoint_config(endpoint: &config::DefaultEndpoint) {
    info!("=== Default Endpoint Configuration ===");
    info!("  Resource Type: {}", endpoint.resource_type);
    info!("  Namespace: {}", endpoint.namespace);

    if let Some(labels) = &endpoint.label_selector {
        info!("  Label Selector:");
        for (key, value) in labels {
            info!("    {} = {}", key, value);
        }
    }

    if let Some(status_query) = &endpoint.status_query {
        info!("  Status Query:");
        info!("    JSONPath: {}", status_query.json_path);
        info!("    Expected Values: {:?}", status_query.expected_values);
    }
}

/// Log the resource mapping details
fn log_resource_mapping(mapping: &config::ResourceMapping) {
    info!("  Resource Mapping Found:");
    info!("    Group: {}", mapping.group);
    info!("    Resource: {}", mapping.resource);
}

/// Handle successful resource query
fn handle_query_success(
    resources: &[kube::api::DynamicObject],
    k8s_client: &K8sClient,
    mapping: &config::ResourceMapping,
) {
    if resources.is_empty() {
        warn!("  ⚠️  No matching resources found for default endpoint!");
        warn!("  Clients without tokens will fail to connect.");
        return;
    }

    info!("  ✓ Found {} matching resource(s)", resources.len());

    if let Some(resource) = resources.first() {
        let resource_name = resource.metadata.name.as_deref().unwrap_or("unknown");
        info!("  Selected Resource: {}", resource_name);

        // Extract and log the target address and port(s)
        if let Some(address_path) = &mapping.address_path {
            extract_and_log_target(resource, k8s_client, mapping, address_path);
        }
    }
}

/// Extract and log the target address and port
fn extract_and_log_target(
    resource: &kube::api::DynamicObject,
    k8s_client: &K8sClient,
    mapping: &config::ResourceMapping,
    address_path: &str,
) {
    use tracing::error;

    match k8s_client.extract_address(resource, address_path, mapping.address_type.as_deref()) {
        Ok(address) => {
            // Check if multi-port configuration exists
            if let Some(port_mappings) = &mapping.ports {
                // Multi-port approach
                match k8s_client.extract_ports(resource, port_mappings) {
                    Ok(ports) => {
                        info!("  Default Target: {} ({} ports)", address, ports.len());
                        for (name, port) in &ports {
                            info!("    - {}: {}", name, port);
                        }
                    }
                    Err(e) => {
                        error!("  ✗ Failed to extract ports: {}", e);
                    }
                }
            } else {
                // Single port approach (backwards compatibility)
                match k8s_client.extract_port(
                    resource,
                    mapping.port_path.as_deref(),
                    mapping.port_name.as_deref(),
                ) {
                    Ok(port) => {
                        info!("  Default Target: {}:{}", address, port);
                    }
                    Err(e) => {
                        error!("  ✗ Failed to extract port: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            error!("  ✗ Failed to extract address: {}", e);
        }
    }
}

/// Handle resource query error
fn handle_query_error(
    error: anyhow::Error,
    endpoint: &config::DefaultEndpoint,
    mapping: &config::ResourceMapping,
) {
    use tracing::error;

    error!("  ✗ Failed to query default endpoint resources: {}", error);
    error!("  Namespace: {}", endpoint.namespace);
    error!(
        "  Resource: {}/{}/{}",
        mapping.group, mapping.version, mapping.resource
    );
    error!("  This may be a permissions issue. Check RBAC configuration.");
    error!("  Clients without tokens will fail to connect.");
}

/// Wait for shutdown signal (SIGTERM, SIGINT, or Ctrl+C)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal");
        },
        _ = terminate => {
            info!("Received SIGTERM signal");
        },
    }
}
