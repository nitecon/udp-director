use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{
    Client,
    api::{Api, DynamicObject, ListParams},
    discovery::ApiResource,
};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, info};

use crate::config::{PortMapping, ResourceMapping};

/// Kubernetes client wrapper
#[derive(Clone)]
pub struct K8sClient {
    client: Client,
}

impl K8sClient {
    /// Create a new Kubernetes client using in-cluster configuration
    pub async fn new() -> Result<Self> {
        let client = Client::try_default().await.context(
            "Failed to create Kubernetes client. Ensure running in-cluster or KUBECONFIG is set",
        )?;

        info!("Kubernetes client initialized successfully");
        Ok(Self { client })
    }

    /// Verify that an allocation still names the exact Ready Project X Pod.
    pub async fn verify_project_x_pod(
        &self,
        namespace: &str,
        pod_name: &str,
        pod_uid: &str,
        map: &str,
        build: &str,
    ) -> Result<String> {
        let pod = Api::<Pod>::namespaced(self.client.clone(), namespace)
            .get(pod_name)
            .await
            .with_context(|| format!("Failed to read allocated Pod {namespace}/{pod_name}"))?;
        Self::validate_project_x_pod(&pod, pod_uid, map, build)?;
        pod.status
            .and_then(|status| status.pod_ip)
            .context("allocated Pod has no IP address")
    }

    fn validate_project_x_pod(pod: &Pod, pod_uid: &str, map: &str, build: &str) -> Result<()> {
        if pod.metadata.uid.as_deref() != Some(pod_uid) {
            anyhow::bail!("allocated Pod UID no longer matches");
        }
        if pod.metadata.deletion_timestamp.is_some() {
            anyhow::bail!("allocated Pod is terminating");
        }
        let labels = pod
            .metadata
            .labels
            .as_ref()
            .context("allocated Pod has no labels")?;
        if labels.get("games.nitecon.org/map").map(String::as_str) != Some(map)
            || labels.get("games.nitecon.org/build").map(String::as_str) != Some(build)
        {
            anyhow::bail!("allocated Pod labels do not match the reservation");
        }
        let status = pod.status.as_ref().context("allocated Pod has no status")?;
        let ready = status
            .conditions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|condition| condition.type_ == "Ready" && condition.status == "True");
        if status.phase.as_deref() != Some("Running") || !ready {
            anyhow::bail!("allocated Pod is not Ready");
        }
        Ok(())
    }

    /// Query for resources matching the given criteria
    pub async fn query_resources(
        &self,
        namespace: &str,
        mapping: &ResourceMapping,
        status_query: Option<&StatusQuery>,
        label_selector: Option<&HashMap<String, String>>,
        annotation_selector: Option<&HashMap<String, String>>,
    ) -> Result<Vec<DynamicObject>> {
        // Create API resource definition
        let api_resource = ApiResource {
            group: mapping.group.clone(),
            version: mapping.version.clone(),
            api_version: if mapping.group.is_empty() {
                mapping.version.clone()
            } else {
                format!("{}/{}", mapping.group, mapping.version)
            },
            kind: String::new(), // Not needed for dynamic queries
            plural: mapping.resource.clone(),
        };

        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), namespace, &api_resource);

        // Build label selector string
        let label_selector_str = label_selector.map(|labels| {
            labels
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(",")
        });

        let mut list_params = ListParams::default();
        if let Some(selector) = label_selector_str {
            list_params = list_params.labels(&selector);
        }

        // List resources
        let resource_list = api
            .list(&list_params)
            .await
            .with_context(|| format!("Failed to list resources: {}", mapping.resource))?;

        debug!(
            "Found {} resources of type {}",
            resource_list.items.len(),
            mapping.resource
        );

        // Filter by status query if provided
        let mut filtered: Vec<DynamicObject> = if let Some(query) = status_query {
            resource_list
                .items
                .into_iter()
                .filter(|resource| self.matches_status_query(resource, query))
                .collect()
        } else {
            resource_list.items
        };

        // Filter by annotations if provided (client-side filtering)
        if let Some(annotations) = annotation_selector {
            filtered.retain(|resource| Self::matches_annotation_selector(resource, annotations));
        }

        debug!("After filtering: {} resources match", filtered.len());
        Ok(filtered)
    }

    /// Check if a resource matches the status query
    fn matches_status_query(&self, resource: &DynamicObject, query: &StatusQuery) -> bool {
        // Parse the JSONPath and extract the value
        let resource_json = serde_json::to_value(resource).ok();
        if resource_json.is_none() {
            return false;
        }

        let value = self.extract_json_path(&resource_json.unwrap(), &query.json_path);

        match value {
            Some(Value::String(s)) => query.expected_values.iter().any(|expected| expected == &s),
            Some(Value::Number(n)) => {
                let n_str = n.to_string();
                query
                    .expected_values
                    .iter()
                    .any(|expected| expected == &n_str)
            }
            Some(Value::Bool(b)) => {
                let b_str = b.to_string();
                query
                    .expected_values
                    .iter()
                    .any(|expected| expected == &b_str)
            }
            _ => false,
        }
    }

    /// Check if a resource matches the annotation selector
    fn matches_annotation_selector(
        resource: &DynamicObject,
        selector: &HashMap<String, String>,
    ) -> bool {
        let annotations = match &resource.metadata.annotations {
            Some(annot) => annot,
            None => return false, // No annotations, doesn't match
        };

        // All selector annotations must match
        for (key, expected_value) in selector {
            match annotations.get(key) {
                Some(actual_value) => {
                    if actual_value != expected_value {
                        return false;
                    }
                }
                None => return false, // Required annotation not found
            }
        }

        true
    }

    /// Extract a value from JSON using a simple JSONPath-like syntax
    /// Supports paths like "status.state", "metadata.name", or "spec.containers[0].ports[1].containerPort"
    fn extract_json_path(&self, json: &Value, path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = json;

        for part in parts {
            // Check if this part contains array indexing like "containers[0]"
            if let Some(bracket_pos) = part.find('[') {
                let field_name = &part[..bracket_pos];
                let index_str = &part[bracket_pos + 1..part.len() - 1]; // Extract index between [ and ]

                // Get the field (which should be an array)
                current = current.get(field_name)?;

                // Parse the index and get the array element
                if let Ok(index) = index_str.parse::<usize>() {
                    current = current.get(index)?;
                } else {
                    return None;
                }
            } else {
                // Simple field access
                current = current.get(part)?;
            }
        }

        Some(current.clone())
    }

    /// Extract address from a resource using JSONPath
    /// If address_type is provided, will search an array of addresses for the matching type
    pub fn extract_address(
        &self,
        resource: &DynamicObject,
        address_path: &str,
        address_type: Option<&str>,
    ) -> Result<String> {
        let resource_json =
            serde_json::to_value(resource).context("Failed to serialize resource to JSON")?;

        let value = self
            .extract_json_path(&resource_json, address_path)
            .context(format!(
                "Failed to extract address from path: {}",
                address_path
            ))?;

        // If address_type is specified, search the array for matching type
        if let Some(addr_type) = address_type {
            match value {
                Value::Array(addresses) => {
                    // Search for address with matching type
                    for addr_entry in addresses {
                        if let Some(Value::String(entry_type)) = addr_entry.get("type") {
                            if entry_type == addr_type {
                                if let Some(Value::String(address)) = addr_entry.get("address") {
                                    debug!("Found address of type '{}': {}", addr_type, address);
                                    return Ok(address.to_string());
                                }
                            }
                        }
                    }
                    anyhow::bail!(
                        "No address found with type '{}' in addresses array",
                        addr_type
                    )
                }
                _ => anyhow::bail!(
                    "Address path did not resolve to array when addressType is specified: {}",
                    address_path
                ),
            }
        } else {
            // Simple string extraction (original behavior)
            match value {
                Value::String(s) => Ok(s),
                _ => anyhow::bail!("Address path did not resolve to a string: {}", address_path),
            }
        }
    }

    /// Extract port from a resource using JSONPath or port name
    pub fn extract_port(
        &self,
        resource: &DynamicObject,
        port_path: Option<&str>,
        port_name: Option<&str>,
    ) -> Result<u16> {
        let resource_json =
            serde_json::to_value(resource).context("Failed to serialize resource to JSON")?;

        // If port_name is provided, look it up in status.ports array or spec.containers[].ports array
        if let Some(name) = port_name {
            // First try status.ports (for resources like GameServers)
            if let Some(Value::Object(status)) = resource_json.get("status") {
                if let Some(Value::Array(ports)) = status.get("ports") {
                    for port in ports {
                        if let Some(Value::String(port_name_val)) = port.get("name") {
                            if port_name_val == name {
                                if let Some(Value::Number(port_num)) = port.get("port") {
                                    debug!("Found port '{}' in status.ports: {}", name, port_num);
                                    return port_num
                                        .as_u64()
                                        .and_then(|n| u16::try_from(n).ok())
                                        .context("Port number out of range");
                                }
                            }
                        }
                    }
                }
            }

            // If not found in status, try spec.containers[].ports[] (for Pods)
            if let Some(Value::Object(spec)) = resource_json.get("spec") {
                if let Some(Value::Array(containers)) = spec.get("containers") {
                    for container in containers {
                        if let Some(Value::Array(ports)) = container.get("ports") {
                            for port in ports {
                                if let Some(Value::String(port_name_val)) = port.get("name") {
                                    if port_name_val == name {
                                        if let Some(Value::Number(port_num)) =
                                            port.get("containerPort")
                                        {
                                            debug!(
                                                "Found port '{}' in spec.containers[].ports: {}",
                                                name, port_num
                                            );
                                            return port_num
                                                .as_u64()
                                                .and_then(|n| u16::try_from(n).ok())
                                                .context("Port number out of range");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            anyhow::bail!("Port with name '{}' not found in resource", name);
        }

        // Otherwise use port_path
        if let Some(path) = port_path {
            let value = self
                .extract_json_path(&resource_json, path)
                .context(format!("Failed to extract port from path: {}", path))?;

            match value {
                Value::Number(n) => n
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .context("Port number out of range"),
                _ => anyhow::bail!("Port path did not resolve to a number: {}", path),
            }
        } else {
            anyhow::bail!("Either port_path or port_name must be provided");
        }
    }

    /// Extract multiple ports from a resource based on port mappings
    pub fn extract_ports(
        &self,
        resource: &DynamicObject,
        port_mappings: &[PortMapping],
    ) -> Result<HashMap<String, u16>> {
        let mut ports = HashMap::new();

        for mapping in port_mappings {
            let port = self.extract_port(
                resource,
                mapping.port_path.as_deref(),
                mapping.port_name.as_deref(),
            )?;
            ports.insert(mapping.name.clone(), port);
            debug!("Extracted port '{}': {}", mapping.name, port);
        }

        Ok(ports)
    }

    /// Find a service for a given resource
    pub async fn find_service_for_resource(
        &self,
        namespace: &str,
        resource_name: &str,
        selector_label: &str,
        port_name: &str,
    ) -> Result<Option<(String, u16)>> {
        let services: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        // List all services in the namespace
        let service_list = services
            .list(&ListParams::default())
            .await
            .context("Failed to list services")?;

        // Find a service with the matching label
        for service in service_list.items {
            if let Some(labels) = &service.metadata.labels {
                if let Some(label_value) = labels.get(selector_label) {
                    if label_value == resource_name {
                        // Found matching service, extract cluster IP and port
                        if let Some(spec) = &service.spec {
                            let cluster_ip = spec.cluster_ip.clone().unwrap_or_default();

                            // Find the port by name
                            if let Some(ports) = &spec.ports {
                                for port in ports {
                                    if let Some(name) = &port.name {
                                        if name == port_name {
                                            let port_number = port.port;
                                            debug!(
                                                "Found service {} with IP {} and port {}",
                                                service
                                                    .metadata
                                                    .name
                                                    .as_ref()
                                                    .unwrap_or(&"unknown".to_string()),
                                                cluster_ip,
                                                port_number
                                            );
                                            return Ok(Some((cluster_ip, port_number as u16)));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        debug!(
            "No service found for resource {} with label {}",
            resource_name, selector_label
        );
        Ok(None)
    }
}

/// Status query for filtering resources
#[derive(Debug, Clone)]
pub struct StatusQuery {
    pub json_path: String,
    pub expected_values: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_extract_json_path() {
        // Create a mock client for testing
        let client = match K8sClient::new().await {
            Ok(c) => c,
            Err(_) => {
                // Skip test if not in a k8s environment
                return;
            }
        };

        let json = json!({
            "status": {
                "state": "Allocated"
            },
            "metadata": {
                "name": "test-server"
            }
        });

        let value = client.extract_json_path(&json, "status.state");
        assert_eq!(value, Some(Value::String("Allocated".to_string())));

        let value = client.extract_json_path(&json, "metadata.name");
        assert_eq!(value, Some(Value::String("test-server".to_string())));

        let value = client.extract_json_path(&json, "nonexistent.path");
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_extract_json_path_with_arrays() {
        // Create a mock client for testing
        let client = match K8sClient::new().await {
            Ok(c) => c,
            Err(_) => {
                // Skip test if not in a k8s environment
                return;
            }
        };

        // Test with pod-like structure
        let json = json!({
            "spec": {
                "containers": [
                    {
                        "name": "starx",
                        "ports": [
                            {
                                "name": "game-udp",
                                "containerPort": 7777,
                                "protocol": "UDP"
                            },
                            {
                                "name": "game-tcp",
                                "containerPort": 7777,
                                "protocol": "TCP"
                            }
                        ]
                    }
                ]
            },
            "status": {
                "podIP": "10.244.1.44"
            }
        });

        // Test array indexing
        let value = client.extract_json_path(&json, "spec.containers[0].name");
        assert_eq!(value, Some(Value::String("starx".to_string())));

        let value = client.extract_json_path(&json, "spec.containers[0].ports[0].containerPort");
        assert_eq!(value, Some(Value::Number(7777.into())));

        let value = client.extract_json_path(&json, "spec.containers[0].ports[1].protocol");
        assert_eq!(value, Some(Value::String("TCP".to_string())));

        let value = client.extract_json_path(&json, "status.podIP");
        assert_eq!(value, Some(Value::String("10.244.1.44".to_string())));

        // Test invalid array index
        let value = client.extract_json_path(&json, "spec.containers[5].name");
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_extract_port_from_pod_spec() {
        // Create a mock client for testing
        let client = match K8sClient::new().await {
            Ok(c) => c,
            Err(_) => {
                // Skip test if not in a k8s environment
                return;
            }
        };

        // Create a mock pod resource
        let pod_json = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "test-pod"
            },
            "spec": {
                "containers": [
                    {
                        "name": "starx",
                        "ports": [
                            {
                                "name": "game-udp",
                                "containerPort": 7777,
                                "protocol": "UDP"
                            },
                            {
                                "name": "game-tcp",
                                "containerPort": 7777,
                                "protocol": "TCP"
                            }
                        ]
                    }
                ]
            },
            "status": {
                "podIP": "10.244.1.44",
                "phase": "Running"
            }
        });

        let pod: DynamicObject = serde_json::from_value(pod_json).unwrap();

        // Test port extraction by name
        let port = client.extract_port(&pod, None, Some("game-udp")).unwrap();
        assert_eq!(port, 7777);

        let port = client.extract_port(&pod, None, Some("game-tcp")).unwrap();
        assert_eq!(port, 7777);

        // Test port extraction by path
        let port = client
            .extract_port(
                &pod,
                Some("spec.containers[0].ports[0].containerPort"),
                None,
            )
            .unwrap();
        assert_eq!(port, 7777);

        let port = client
            .extract_port(
                &pod,
                Some("spec.containers[0].ports[1].containerPort"),
                None,
            )
            .unwrap();
        assert_eq!(port, 7777);

        // Test non-existent port name
        let result = client.extract_port(&pod, None, Some("non-existent"));
        assert!(result.is_err());
    }

    #[test]
    fn test_annotation_selector_matching() {
        // Note: `matches_annotation_selector` is a pure associated function that
        // operates only on the resource and selector, so this test does not need a
        // live Kubernetes client (constructing one would fail in CI where no
        // kubeconfig or in-cluster config is available).

        // Create a resource with annotations
        let resource_json = json!({
            "apiVersion": "agones.dev/v1",
            "kind": "GameServer",
            "metadata": {
                "name": "test-server",
                "annotations": {
                    "currentPlayers": "32",
                    "maxPlayers": "64",
                    "map": "de_dust2"
                }
            },
            "status": {
                "state": "Ready"
            }
        });

        let resource: DynamicObject = serde_json::from_value(resource_json).unwrap();

        // Test exact match
        let mut selector = HashMap::new();
        selector.insert("currentPlayers".to_string(), "32".to_string());
        assert!(K8sClient::matches_annotation_selector(&resource, &selector));

        // Test multiple annotations match
        selector.insert("map".to_string(), "de_dust2".to_string());
        assert!(K8sClient::matches_annotation_selector(&resource, &selector));

        // Test annotation value mismatch
        selector.insert("currentPlayers".to_string(), "64".to_string());
        assert!(!K8sClient::matches_annotation_selector(
            &resource, &selector
        ));

        // Test missing annotation
        let mut selector2 = HashMap::new();
        selector2.insert("nonExistent".to_string(), "value".to_string());
        assert!(!K8sClient::matches_annotation_selector(
            &resource, &selector2
        ));

        // Test resource without annotations
        let resource_no_annot = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "test-pod"
            }
        });
        let resource_no_annot: DynamicObject = serde_json::from_value(resource_no_annot).unwrap();
        assert!(!K8sClient::matches_annotation_selector(
            &resource_no_annot,
            &selector
        ));
    }

    #[test]
    fn project_x_pod_validation_requires_exact_uid_ready_and_labels() {
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "tutorial-0",
                "uid": "exact-uid",
                "labels": {
                    "games.nitecon.org/map": "tutorial",
                    "games.nitecon.org/build": "sha256:abc"
                }
            },
            "status": {
                "phase": "Running",
                "podIP": "10.0.0.1",
                "conditions": [{"type": "Ready", "status": "True"}]
            }
        }))
        .unwrap();

        assert!(
            K8sClient::validate_project_x_pod(&pod, "exact-uid", "tutorial", "sha256:abc").is_ok()
        );
        assert!(
            K8sClient::validate_project_x_pod(&pod, "replacement-uid", "tutorial", "sha256:abc")
                .is_err()
        );
        assert!(
            K8sClient::validate_project_x_pod(&pod, "exact-uid", "wrong-map", "sha256:abc")
                .is_err()
        );
    }
}
