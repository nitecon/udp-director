use lazy_static::lazy_static;
use prometheus::{
    Encoder, Gauge, GaugeVec, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, TextEncoder,
    register_gauge, register_gauge_vec, register_histogram_vec, register_int_counter_vec,
    register_int_gauge, register_int_gauge_vec,
};

lazy_static! {
    // Connection metrics
    pub static ref ACTIVE_SESSIONS: IntGauge = register_int_gauge!(
        "udp_director_active_sessions",
        "Number of active UDP sessions"
    )
    .unwrap();

    pub static ref TOTAL_SESSIONS: IntCounterVec = register_int_counter_vec!(
        "udp_director_total_sessions",
        "Total number of sessions created",
        &["session_type"] // "query", "default"
    )
    .unwrap();

    pub static ref SESSION_DURATION: HistogramVec = register_histogram_vec!(
        "udp_director_session_duration_seconds",
        "Duration of sessions in seconds",
        &["session_type"],
        vec![1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0]
    )
    .unwrap();

    // Packet metrics
    pub static ref PACKETS_RECEIVED: IntCounterVec = register_int_counter_vec!(
        "udp_director_packets_received_total",
        "Total number of packets received",
        &["source"] // "client", "server"
    )
    .unwrap();

    pub static ref PACKETS_SENT: IntCounterVec = register_int_counter_vec!(
        "udp_director_packets_sent_total",
        "Total number of packets sent",
        &["destination"] // "client", "server"
    )
    .unwrap();

    pub static ref BYTES_RECEIVED: IntCounterVec = register_int_counter_vec!(
        "udp_director_bytes_received_total",
        "Total bytes received",
        &["source"]
    )
    .unwrap();

    pub static ref BYTES_SENT: IntCounterVec = register_int_counter_vec!(
        "udp_director_bytes_sent_total",
        "Total bytes sent",
        &["destination"]
    )
    .unwrap();

    pub static ref PACKET_SIZE: HistogramVec = register_histogram_vec!(
        "udp_director_packet_size_bytes",
        "Size of packets in bytes",
        &["direction"], // "inbound", "outbound"
        vec![64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0]
    )
    .unwrap();

    // Query server metrics
    pub static ref QUERY_REQUESTS: IntCounterVec = register_int_counter_vec!(
        "udp_director_query_requests_total",
        "Total number of query requests",
        &["status"] // "success", "error"
    )
    .unwrap();

    pub static ref QUERY_DURATION: HistogramVec = register_histogram_vec!(
        "udp_director_query_duration_seconds",
        "Duration of query processing",
        &["status"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]
    )
    .unwrap();

    // Kubernetes metrics
    pub static ref K8S_QUERIES: IntCounterVec = register_int_counter_vec!(
        "udp_director_k8s_queries_total",
        "Kubernetes API queries",
        &["resource_type", "status"] // status: "success", "error"
    )
    .unwrap();

    pub static ref K8S_QUERY_DURATION: HistogramVec = register_histogram_vec!(
        "udp_director_k8s_query_duration_seconds",
        "Duration of Kubernetes queries",
        &["resource_type"],
        vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .unwrap();

    pub static ref DEFAULT_ENDPOINT_AVAILABLE: IntGauge = register_int_gauge!(
        "udp_director_default_endpoint_available",
        "Whether default endpoint is available (1=yes, 0=no)"
    )
    .unwrap();

    pub static ref AVAILABLE_RESOURCES: IntGaugeVec = register_int_gauge_vec!(
        "udp_director_available_resources",
        "Number of available resources by type",
        &["resource_type", "namespace"]
    )
    .unwrap();

    // Error metrics
    pub static ref ERRORS: IntCounterVec = register_int_counter_vec!(
        "udp_director_errors_total",
        "Total errors by type",
        &["error_type", "component"] // component: "proxy", "query_server", "monitor"
    )
    .unwrap();

    // Connection age tracking
    pub static ref SESSION_AGE: GaugeVec = register_gauge_vec!(
        "udp_director_session_age_seconds",
        "Age of active sessions in seconds",
        &["client_addr"]
    )
    .unwrap();

    // Unique clients
    pub static ref UNIQUE_CLIENTS: IntGauge = register_int_gauge!(
        "udp_director_unique_clients",
        "Number of unique client addresses"
    )
    .unwrap();

    // Server uptime
    pub static ref UPTIME_SECONDS: Gauge = register_gauge!(
        "udp_director_uptime_seconds",
        "Server uptime in seconds"
    )
    .unwrap();
}

/// Gather all metrics and encode them in Prometheus text format
///
/// # Panics
///
/// This function will panic if the encoder fails or if the buffer contains invalid UTF-8.
/// In practice, this should never happen as Prometheus metrics are always valid UTF-8.
pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    // SAFETY: This should never fail as we're encoding to a Vec<u8>
    // If it does, it's a critical bug in the prometheus crate
    encoder
        .encode(&metric_families, &mut buffer)
        .expect("Failed to encode metrics - this is a bug");
    // SAFETY: Prometheus metrics are always valid UTF-8
    String::from_utf8(buffer).expect("Metrics buffer contained invalid UTF-8 - this is a bug")
}

/// Record a new session
#[allow(dead_code)]
pub fn record_session_start(session_type: &str) {
    TOTAL_SESSIONS.with_label_values(&[session_type]).inc();
    ACTIVE_SESSIONS.inc();
}

/// Record a session end
#[allow(dead_code)]
pub fn record_session_end(session_type: &str, duration_seconds: f64) {
    ACTIVE_SESSIONS.dec();
    SESSION_DURATION
        .with_label_values(&[session_type])
        .observe(duration_seconds);
}

/// Record packet received
#[allow(dead_code)]
pub fn record_packet_received(source: &str, size: usize) {
    PACKETS_RECEIVED.with_label_values(&[source]).inc();
    BYTES_RECEIVED
        .with_label_values(&[source])
        .inc_by(size as u64);
    PACKET_SIZE
        .with_label_values(&["inbound"])
        .observe(size as f64);
}

/// Record packet sent
#[allow(dead_code)]
pub fn record_packet_sent(destination: &str, size: usize) {
    PACKETS_SENT.with_label_values(&[destination]).inc();
    BYTES_SENT
        .with_label_values(&[destination])
        .inc_by(size as u64);
    PACKET_SIZE
        .with_label_values(&["outbound"])
        .observe(size as f64);
}

/// Record query request
#[allow(dead_code)]
pub fn record_query_request(status: &str, duration_seconds: f64) {
    QUERY_REQUESTS.with_label_values(&[status]).inc();
    QUERY_DURATION
        .with_label_values(&[status])
        .observe(duration_seconds);
}

/// Record Kubernetes query
#[allow(dead_code)]
pub fn record_k8s_query(resource_type: &str, status: &str, duration_seconds: f64) {
    K8S_QUERIES
        .with_label_values(&[resource_type, status])
        .inc();
    K8S_QUERY_DURATION
        .with_label_values(&[resource_type])
        .observe(duration_seconds);
}

/// Record error
#[allow(dead_code)]
pub fn record_error(error_type: &str, component: &str) {
    ERRORS.with_label_values(&[error_type, component]).inc();
}

/// Update default endpoint availability
#[allow(dead_code)]
pub fn update_default_endpoint_available(available: bool) {
    DEFAULT_ENDPOINT_AVAILABLE.set(if available { 1 } else { 0 });
}

/// Update available resources count
#[allow(dead_code)]
pub fn update_available_resources(resource_type: &str, namespace: &str, count: i64) {
    AVAILABLE_RESOURCES
        .with_label_values(&[resource_type, namespace])
        .set(count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_recording() {
        // Test session metrics
        record_session_start("query");
        assert_eq!(ACTIVE_SESSIONS.get(), 1);

        record_session_end("query", 10.0);
        assert_eq!(ACTIVE_SESSIONS.get(), 0);

        // Test packet metrics
        record_packet_received("client", 1024);
        record_packet_sent("server", 512);

        // Test query metrics
        record_query_request("success", 0.05);

        // Test K8s metrics
        record_k8s_query("gameserver", "success", 0.1);

        // Test error recording
        record_error("timeout", "proxy");

        // Test default endpoint
        update_default_endpoint_available(true);
        assert_eq!(DEFAULT_ENDPOINT_AVAILABLE.get(), 1);

        // Test resource count
        update_available_resources("gameserver", "default", 5);

        // Verify metrics can be gathered
        let metrics = gather_metrics();
        assert!(!metrics.is_empty());
        assert!(metrics.contains("udp_director_active_sessions"));
    }
}
