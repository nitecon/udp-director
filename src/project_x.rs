use anyhow::{Context, Result};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
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
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::net::TcpListener;
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
}

#[derive(Debug, Deserialize)]
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
        }))
    }

    pub(crate) async fn bind(
        &self,
        allocation_token: &str,
        client_addr: SocketAddr,
        config: &Config,
    ) -> Result<()> {
        Self::validate_allocation_token(allocation_token)?;
        let reservation = self.consume(allocation_token).await?;
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
        self.sessions
            .upsert_project_x(client_addr, target_ip, port_mappings, reservation.pod_uid)
            .await;
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

    async fn consume(&self, allocation_token: &str) -> Result<Reservation> {
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
                            async move { router.admin_request(request) }
                        }),
                    )
                    .await;
                if let Err(error) = result {
                    error!("Project X admin connection failed: {}", error);
                }
            });
        }
    }

    fn admin_request(
        self,
        request: Request<hyper::body::Incoming>,
    ) -> Result<Response<Full<Bytes>>> {
        let path = request.uri().path();
        if request.method() == Method::GET && (path == "/livez" || path == "/readyz") {
            return Self::response(StatusCode::NO_CONTENT, Bytes::new(), "text/plain");
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
}
