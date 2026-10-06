use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use http_body_util::Full;
use hyper::{Request, StatusCode, body::Bytes};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use tokio::time::Instant;

use crate::config::CapacityDemandConfig;

#[derive(Clone, Debug, thiserror::Error)]
pub enum DemandError {
    #[error("Unknown backend group")]
    UnknownGroup,
    #[error("Backend group is under maintenance")]
    Maintenance,
    #[error("Capacity demand unavailable")]
    Unavailable,
    #[error("Server startup timed out")]
    Timeout,
    #[error("Query disconnected")]
    Cancelled,
}

pub struct DemandFlight {
    pub deadline: Instant,
    pub accepted: Shared<BoxFuture<'static, Result<(), DemandError>>>,
}

/// Only concurrent, live cold queries share a signal. No persistent player state.
#[derive(Clone, Default)]
pub struct CapacityDemand {
    flights: Arc<Mutex<HashMap<String, Weak<DemandFlight>>>>,
}

impl CapacityDemand {
    pub fn signal(&self, config: &CapacityDemandConfig, group: &str) -> Arc<DemandFlight> {
        let mut flights = self.flights.lock().expect("capacity demand mutex poisoned");
        flights.retain(|_, flight| flight.strong_count() > 0);
        if let Some(flight) = flights.get(group).and_then(Weak::upgrade) {
            return flight;
        }
        let endpoint = config.endpoint.clone();
        let group_owned = group.to_owned();
        let deadline = Instant::now() + Duration::from_secs(config.boot_timeout_seconds);
        let flight = Arc::new(DemandFlight {
            deadline,
            accepted: async move {
                tokio::time::timeout_at(deadline, post_demand(&endpoint, &group_owned))
                    .await
                    .unwrap_or(Err(DemandError::Timeout))
            }
            .boxed()
            .shared(),
        });
        flights.insert(group.to_owned(), Arc::downgrade(&flight));
        flight
    }
}

async fn post_demand(endpoint: &str, group: &str) -> Result<(), DemandError> {
    let body = serde_json::to_vec(&serde_json::json!({"backendGroup": group}))
        .map_err(|_| DemandError::Unavailable)?;
    let request = Request::post(endpoint)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| DemandError::Unavailable)?;
    let client = Client::builder(TokioExecutor::new())
        .retry_canceled_requests(false)
        .build_http();
    let response = client.request(request).await.map_err(|error| {
        tracing::warn!("Capacity demand failed: {error}");
        DemandError::Unavailable
    })?;
    match response.status() {
        StatusCode::ACCEPTED => Ok(()),
        StatusCode::NOT_FOUND => Err(DemandError::UnknownGroup),
        StatusCode::SERVICE_UNAVAILABLE => Err(DemandError::Maintenance),
        status => {
            tracing::warn!("Unexpected capacity demand status: {status}");
            Err(DemandError::Unavailable)
        }
    }
}
