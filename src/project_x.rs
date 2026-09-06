use anyhow::{Context, Result};
use dashmap::DashMap;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Body, Bytes};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::config::{Config, Protocol};
use crate::k8s_client::K8sClient;
use crate::session::SessionManager;

#[derive(Clone)]
pub(crate) struct ProjectXRouter {
    controller_url: String,
    token_path: PathBuf,
    k8s_client: K8sClient,
    sessions: SessionManager,
    admin_port: u16,
    allocations: Arc<DashMap<String, InstalledAllocation>>,
    install_ids: Arc<DashMap<String, String>>,
    install_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Reservation {
    pod_uid: String,
    namespace: String,
    pod_name: String,
    map: String,
    build: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallAllocation {
    api_version: String,
    install_id: String,
    ticket_id: String,
    allocation_id: String,
    routing_token: String,
    pod_uid: String,
    namespace: String,
    pod_name: String,
    map: String,
    build: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
struct InstalledAllocation {
    request: InstallAllocation,
    client_addr: Option<SocketAddr>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RouteStatus {
    active_routes: usize,
    last_new_route_at: String,
}

impl ProjectXRouter {
    pub(crate) fn from_env(
        k8s_client: K8sClient,
        sessions: SessionManager,
    ) -> Result<Option<Self>> {
        let Ok(controller_url) = std::env::var("PROJECT_X_CONTROLLER_URL") else {
            return Ok(None);
        };
        let token_path = std::env::var("PROJECT_X_CONTROLLER_TOKEN_FILE")
            .unwrap_or_else(|_| "/var/run/secrets/starx/tokens/controller-token".to_string());
        let admin_port = std::env::var("PROJECT_X_ADMIN_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .context("PROJECT_X_ADMIN_PORT must be a TCP port")?;
        Ok(Some(Self {
            controller_url: controller_url.trim_end_matches('/').to_string(),
            token_path: PathBuf::from(token_path),
            k8s_client,
            sessions,
            admin_port,
            allocations: Arc::new(DashMap::new()),
            install_ids: Arc::new(DashMap::new()),
            install_lock: Arc::new(Mutex::new(())),
        }))
    }

    pub(crate) async fn bind(
        &self,
        allocation_token: &str,
        client_addr: SocketAddr,
        config: &Config,
    ) -> Result<()> {
        self.bind_route(allocation_token, client_addr, config, false)
            .await
    }

    /// Consume a reservation and bind it to the exact gameplay UDP socket.
    pub(crate) async fn bind_socket(
        &self,
        allocation_token: &str,
        client_addr: SocketAddr,
        config: &Config,
    ) -> Result<()> {
        self.bind_route(allocation_token, client_addr, config, true)
            .await
    }

    async fn bind_route(
        &self,
        allocation_token: &str,
        client_addr: SocketAddr,
        config: &Config,
        exact_socket: bool,
    ) -> Result<()> {
        Self::validate_allocation_token(allocation_token)?;
        let reservation = if exact_socket {
            self.consume_installed(allocation_token, client_addr)?
        } else {
            self.consume_legacy(allocation_token).await?
        };
        Self::validate_reservation(&reservation, OffsetDateTime::now_utc())?;
        let target_ip = self
            .k8s_client
            .verify_project_x_pod(
                &reservation.namespace,
                &reservation.pod_name,
                &reservation.pod_uid,
                &reservation.map,
                &reservation.build,
            )
            .await?;
        let port_mappings = config
            .get_data_ports()
            .into_iter()
            .map(|port| ((port.port, port.protocol), port.port))
            .collect::<HashMap<(u16, Protocol), u16>>();
        if exact_socket {
            self.sessions
                .upsert_project_x(client_addr, target_ip, port_mappings, reservation.pod_uid)
                .await;
        } else {
            self.sessions
                .upsert_project_x_legacy(client_addr, target_ip, port_mappings, reservation.pod_uid)
                .await;
        }
        Ok(())
    }

    fn validate_allocation_token(allocation_token: &str) -> Result<()> {
        if allocation_token.trim().is_empty()
            || allocation_token.contains('/')
            || allocation_token.len() > 512
        {
            anyhow::bail!("malformed allocation token");
        }
        Ok(())
    }

    fn validate_reservation(reservation: &Reservation, now: OffsetDateTime) -> Result<()> {
        if reservation.expires_at <= now {
            anyhow::bail!("allocation expired");
        }
        if reservation.pod_uid.is_empty()
            || reservation.namespace.is_empty()
            || reservation.pod_name.is_empty()
            || reservation.map.is_empty()
            || reservation.build.is_empty()
        {
            anyhow::bail!("allocation is incomplete");
        }
        Ok(())
    }

    fn consume_installed(
        &self,
        routing_token: &str,
        client_addr: SocketAddr,
    ) -> Result<Reservation> {
        let now = OffsetDateTime::now_utc();
        let mut allocation = self
            .allocations
            .get_mut(routing_token)
            .context("allocation is unknown or was not installed")?;
        allocation.bind(client_addr, now)
    }

    async fn consume_legacy(&self, allocation_token: &str) -> Result<Reservation> {
        let identity_token = tokio::fs::read_to_string(&self.token_path)
            .await
            .with_context(|| format!("failed to read {}", self.token_path.display()))?;
        let uri: Uri = format!(
            "{}/v1alpha1/reservations/{}/consume",
            self.controller_url, allocation_token
        )
        .parse()
        .context("invalid controller URL")?;
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("Authorization", format!("Bearer {}", identity_token.trim()))
            .body(Empty::<Bytes>::new())?;
        let client: Client<HttpConnector, Empty<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let response = client
            .request(request)
            .await
            .context("controller request failed")?;
        if !response.status().is_success() {
            anyhow::bail!("controller rejected allocation ({})", response.status());
        }
        let body = response.into_body().collect().await?.to_bytes();
        serde_json::from_slice(&body).context("controller returned an invalid reservation")
    }

    pub(crate) async fn run_admin_server(self) -> Result<()> {
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], self.admin_port))).await?;
        info!("Project X admin server listening on {}", self.admin_port);
        loop {
            let (stream, _) = listener.accept().await?;
            let router = self.clone();
            tokio::spawn(async move {
                let result = http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request| {
                            let router = router.clone();
                            async move { router.admin_request(request).await }
                        }),
                    )
                    .await;
                if let Err(error) = result {
                    error!("Project X admin connection failed: {}", error);
                }
            });
        }
    }

    async fn admin_request(
        self,
        request: Request<hyper::body::Incoming>,
    ) -> Result<Response<Full<Bytes>>> {
        let path = request.uri().path();
        if request.method() == Method::GET && (path == "/livez" || path == "/readyz") {
            return Self::response(StatusCode::NO_CONTENT, Bytes::new(), "text/plain");
        }
        if request.method() == Method::POST && path == "/v1alpha1/allocations" {
            return self.install_allocation(request).await;
        }
        let prefix = "/v1alpha1/pods/";
        let suffix = "/routes";
        if request.method() != Method::GET || !path.starts_with(prefix) || !path.ends_with(suffix) {
            return Self::response(
                StatusCode::NOT_FOUND,
                Bytes::from("not found"),
                "text/plain",
            );
        }
        let pod_uid = &path[prefix.len()..path.len() - suffix.len()];
        let Some((active_routes, last_new)) = self.sessions.project_x_route_status(pod_uid) else {
            return Self::response(
                StatusCode::SERVICE_UNAVAILABLE,
                Bytes::from("route state unknown"),
                "text/plain",
            );
        };
        let body = serde_json::to_vec(&RouteStatus {
            active_routes,
            last_new_route_at: last_new.format(&Rfc3339)?,
        })?;
        Self::response(StatusCode::OK, Bytes::from(body), "application/json")
    }

    async fn install_allocation(
        &self,
        request: Request<hyper::body::Incoming>,
    ) -> Result<Response<Full<Bytes>>> {
        if request
            .body()
            .size_hint()
            .upper()
            .is_some_and(|length| length > 65_536)
        {
            return Self::response(
                StatusCode::PAYLOAD_TOO_LARGE,
                Bytes::from("request body is too large"),
                "text/plain",
            );
        }
        let body = request.into_body().collect().await?.to_bytes();
        if body.len() > 65_536 {
            return Self::response(
                StatusCode::PAYLOAD_TOO_LARGE,
                Bytes::from("request body is too large"),
                "text/plain",
            );
        }
        let install: InstallAllocation = match serde_json::from_slice(&body) {
            Ok(install) => install,
            Err(_) => {
                return Self::response(
                    StatusCode::BAD_REQUEST,
                    Bytes::from("invalid allocation install"),
                    "text/plain",
                );
            }
        };
        if let Err(error) = Self::validate_install(&install, OffsetDateTime::now_utc()) {
            return Self::response(
                StatusCode::BAD_REQUEST,
                Bytes::from(error.to_string()),
                "text/plain",
            );
        }
        let _install_guard = self.install_lock.lock().await;
        if let Some(existing_token) = self.install_ids.get(&install.install_id) {
            let identical = existing_token.value() == &install.routing_token
                && self
                    .allocations
                    .get(existing_token.value())
                    .is_some_and(|existing| existing.request == install);
            return if identical {
                Self::install_response(&install)
            } else {
                Self::response(
                    StatusCode::CONFLICT,
                    Bytes::from("installId conflicts with an existing allocation"),
                    "text/plain",
                )
            };
        }
        if self.allocations.contains_key(&install.routing_token) {
            return Self::response(
                StatusCode::CONFLICT,
                Bytes::from("routingToken conflicts with an existing allocation"),
                "text/plain",
            );
        }
        if let Err(error) = self
            .k8s_client
            .verify_project_x_pod(
                &install.namespace,
                &install.pod_name,
                &install.pod_uid,
                &install.map,
                &install.build,
            )
            .await
        {
            return Self::response(
                StatusCode::CONFLICT,
                Bytes::from(format!("allocated Pod was not verified: {error}")),
                "text/plain",
            );
        }
        self.allocations.insert(
            install.routing_token.clone(),
            InstalledAllocation {
                request: install.clone(),
                client_addr: None,
            },
        );
        self.install_ids
            .insert(install.install_id.clone(), install.routing_token.clone());
        Self::install_response(&install)
    }

    fn validate_install(install: &InstallAllocation, now: OffsetDateTime) -> Result<()> {
        if install.api_version != "runtime.games.nitecon.org/v1alpha1" {
            anyhow::bail!("unsupported apiVersion");
        }
        if install.install_id != format!("director:{}", install.allocation_id) {
            anyhow::bail!("installId does not match allocationId");
        }
        if install.expires_at <= now {
            anyhow::bail!("allocation expired");
        }
        for (name, value) in [
            ("installId", install.install_id.as_str()),
            ("ticketId", install.ticket_id.as_str()),
            ("allocationId", install.allocation_id.as_str()),
            ("routingToken", install.routing_token.as_str()),
            ("podUid", install.pod_uid.as_str()),
            ("namespace", install.namespace.as_str()),
            ("podName", install.pod_name.as_str()),
            ("map", install.map.as_str()),
            ("build", install.build.as_str()),
        ] {
            if value.is_empty() || value.len() > 512 {
                anyhow::bail!("{name} is invalid");
            }
        }
        Self::validate_allocation_token(&install.routing_token)
    }

    fn install_response(install: &InstallAllocation) -> Result<Response<Full<Bytes>>> {
        let body = serde_json::to_vec(&serde_json::json!({
            "allocationId": install.allocation_id,
            "status": "installed"
        }))?;
        Self::response(StatusCode::OK, Bytes::from(body), "application/json")
    }

    fn response(
        status: StatusCode,
        body: Bytes,
        content_type: &str,
    ) -> Result<Response<Full<Bytes>>> {
        Response::builder()
            .status(status)
            .header("Content-Type", content_type)
            .body(Full::new(body))
            .context("failed to build admin response")
    }
}

impl InstallAllocation {
    fn reservation(&self) -> Reservation {
        Reservation {
            pod_uid: self.pod_uid.clone(),
            namespace: self.namespace.clone(),
            pod_name: self.pod_name.clone(),
            map: self.map.clone(),
            build: self.build.clone(),
            expires_at: self.expires_at,
        }
    }
}

impl InstalledAllocation {
    fn bind(&mut self, client_addr: SocketAddr, now: OffsetDateTime) -> Result<Reservation> {
        ProjectXRouter::validate_install(&self.request, now)?;
        match self.client_addr {
            None => self.client_addr = Some(client_addr),
            Some(bound) if bound == client_addr => {}
            Some(_) => anyhow::bail!("allocation routing token was already consumed"),
        }
        Ok(self.request.reservation())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(expires_at: OffsetDateTime) -> Reservation {
        Reservation {
            pod_uid: "pod-uid".to_string(),
            namespace: "games".to_string(),
            pod_name: "map-0".to_string(),
            map: "tutorial".to_string(),
            build: "sha256:abc".to_string(),
            expires_at,
        }
    }

    fn install(expires_at: OffsetDateTime) -> InstallAllocation {
        InstallAllocation {
            api_version: "runtime.games.nitecon.org/v1alpha1".to_string(),
            install_id: "director:allocation-id".to_string(),
            ticket_id: "ticket-id".to_string(),
            allocation_id: "allocation-id".to_string(),
            routing_token: "route-token".to_string(),
            pod_uid: "pod-uid".to_string(),
            namespace: "project-x".to_string(),
            pod_name: "map-0".to_string(),
            map: "new-dawn-01".to_string(),
            build: "078e1fdc".to_string(),
            expires_at,
        }
    }

    #[test]
    fn rejects_malformed_allocation_tokens() {
        assert!(ProjectXRouter::validate_allocation_token("").is_err());
        assert!(ProjectXRouter::validate_allocation_token("../token").is_err());
        assert!(ProjectXRouter::validate_allocation_token("valid-token").is_ok());
    }

    #[test]
    fn rejects_expired_allocations() {
        let now = OffsetDateTime::now_utc();
        assert!(ProjectXRouter::validate_reservation(&reservation(now), now).is_err());
        assert!(
            ProjectXRouter::validate_reservation(&reservation(now + time::Duration::SECOND), now)
                .is_ok()
        );
    }

    #[test]
    fn decodes_go_rfc3339_reservation() {
        let parsed: Reservation = serde_json::from_str(
            r#"{
                "podUid":"pod-uid",
                "namespace":"project-x",
                "podName":"tutorial-0",
                "map":"m-tutorial",
                "build":"506f5559",
                "expiresAt":"2026-09-02T03:00:00.123456789Z"
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.expires_at.year(), 2026);
    }

    #[test]
    fn validates_authoritative_install_contract() {
        let now = OffsetDateTime::now_utc();
        assert!(
            ProjectXRouter::validate_install(&install(now + time::Duration::MINUTE), now).is_ok()
        );
        assert!(ProjectXRouter::validate_install(&install(now), now).is_err());
        let mut wrong_version = install(now + time::Duration::MINUTE);
        wrong_version.api_version = "v1".to_string();
        assert!(ProjectXRouter::validate_install(&wrong_version, now).is_err());
        let mut mismatched_install = install(now + time::Duration::MINUTE);
        mismatched_install.install_id = "director:other-allocation".to_string();
        assert!(ProjectXRouter::validate_install(&mismatched_install, now).is_err());
    }

    #[test]
    fn routing_token_retries_only_from_the_same_gameplay_socket() {
        let now = OffsetDateTime::now_utc();
        let mut allocation = InstalledAllocation {
            request: install(now + time::Duration::MINUTE),
            client_addr: None,
        };
        let first = "10.0.0.1:41000".parse().unwrap();
        let replay = "10.0.0.1:41001".parse().unwrap();
        assert!(allocation.bind(first, now).is_ok());
        assert!(allocation.bind(first, now).is_ok());
        assert!(allocation.bind(replay, now).is_err());
    }
}
