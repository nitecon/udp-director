[← Back to README](../README.md)

# Metrics Documentation

UDP Director exposes Prometheus-compatible metrics for monitoring and observability.

## Metrics Endpoint

- **URL**: `http://<pod-ip>:9090/metrics`
- **Format**: Prometheus text format
- **Health Check**: `http://<pod-ip>:9090/health`

## Available Metrics

### Connection Metrics

#### `udp_director_active_sessions`
- **Type**: Gauge
- **Description**: Number of currently active UDP sessions
- **Use Case**: Monitor concurrent connections

#### `udp_director_total_sessions`
- **Type**: Counter
- **Labels**: `session_type` (query, default)
- **Description**: Total number of sessions created since startup
- **Use Case**: Track session creation rate

#### `udp_director_session_duration_seconds`
- **Type**: Histogram
- **Labels**: `session_type`
- **Buckets**: 1s, 5s, 10s, 30s, 60s, 300s, 600s, 1800s, 3600s
- **Description**: Duration of sessions in seconds
- **Use Case**: Analyze session lifetime patterns

#### `udp_director_unique_clients`
- **Type**: Gauge
- **Description**: Number of unique client addresses
- **Use Case**: Track unique user count

#### `udp_director_session_age_seconds`
- **Type**: Gauge
- **Labels**: `client_addr`
- **Description**: Age of active sessions in seconds
- **Use Case**: Monitor long-running sessions

### Packet Metrics

#### `udp_director_packets_received_total`
- **Type**: Counter
- **Labels**: `source` (client, server)
- **Description**: Total number of packets received
- **Use Case**: Monitor packet throughput

#### `udp_director_packets_sent_total`
- **Type**: Counter
- **Labels**: `destination` (client, server)
- **Description**: Total number of packets sent
- **Use Case**: Monitor outbound traffic

#### `udp_director_bytes_received_total`
- **Type**: Counter
- **Labels**: `source`
- **Description**: Total bytes received
- **Use Case**: Monitor bandwidth usage

#### `udp_director_bytes_sent_total`
- **Type**: Counter
- **Labels**: `destination`
- **Description**: Total bytes sent
- **Use Case**: Monitor bandwidth usage

#### `udp_director_packet_size_bytes`
- **Type**: Histogram
- **Labels**: `direction` (inbound, outbound)
- **Buckets**: 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384 bytes
- **Description**: Size distribution of packets
- **Use Case**: Analyze packet size patterns

### Query Server Metrics

#### `udp_director_query_requests_total`
- **Type**: Counter
- **Labels**: `status` (success, error)
- **Description**: Total number of query requests
- **Use Case**: Monitor query server health

#### `udp_director_query_duration_seconds`
- **Type**: Histogram
- **Labels**: `status`
- **Buckets**: 1ms, 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s
- **Description**: Duration of query processing
- **Use Case**: Track query performance

### Kubernetes Metrics

#### `udp_director_k8s_queries_total`
- **Type**: Counter
- **Labels**: `resource_type`, `status` (success, error)
- **Description**: Total Kubernetes API queries
- **Use Case**: Monitor K8s API usage

#### `udp_director_k8s_query_duration_seconds`
- **Type**: Histogram
- **Labels**: `resource_type`
- **Buckets**: 10ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s, 5s, 10s
- **Description**: Duration of Kubernetes queries
- **Use Case**: Track K8s API performance

#### `udp_director_default_endpoint_available`
- **Type**: Gauge
- **Description**: Whether default endpoint is available (1=yes, 0=no)
- **Use Case**: Alert on default endpoint unavailability

#### `udp_director_available_resources`
- **Type**: Gauge
- **Labels**: `resource_type`, `namespace`
- **Description**: Number of available resources by type
- **Use Case**: Monitor resource availability

### Error Metrics

#### `udp_director_errors_total`
- **Type**: Counter
- **Labels**: `error_type`, `component` (proxy, query_server, monitor)
- **Description**: Total errors by type and component
- **Use Case**: Monitor error rates and types

### System Metrics

#### `udp_director_uptime_seconds`
- **Type**: Gauge
- **Description**: Server uptime in seconds
- **Use Case**: Track service availability

## Prometheus Configuration

### Scrape Config

```yaml
scrape_configs:
  - job_name: 'udp-director'
    kubernetes_sd_configs:
      - role: pod
        namespaces:
          names:
            - game-servers
    relabel_configs:
      - source_labels: [__meta_kubernetes_pod_label_app]
        action: keep
        regex: udp-director
      - source_labels: [__meta_kubernetes_pod_ip]
        target_label: __address__
        replacement: '$1:9090'
      - source_labels: [__meta_kubernetes_namespace]
        target_label: namespace
      - source_labels: [__meta_kubernetes_pod_name]
        target_label: pod
```

### ServiceMonitor (Prometheus Operator)

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: udp-director
  namespace: game-servers
spec:
  selector:
    matchLabels:
      app: udp-director
  endpoints:
    - port: metrics
      interval: 15s
      path: /metrics
```

## Grafana Dashboards

### Key Queries

**Active Sessions**:
```promql
udp_director_active_sessions
```

**Session Creation Rate** (per minute):
```promql
rate(udp_director_total_sessions[1m])
```

**Packet Throughput** (packets/sec):
```promql
rate(udp_director_packets_received_total[1m])
```

**Bandwidth Usage** (bytes/sec):
```promql
rate(udp_director_bytes_received_total[1m]) + rate(udp_director_bytes_sent_total[1m])
```

**Query Latency** (95th percentile):
```promql
histogram_quantile(0.95, rate(udp_director_query_duration_seconds_bucket[5m]))
```

**Error Rate**:
```promql
rate(udp_director_errors_total[5m])
```

**K8s API Latency** (99th percentile):
```promql
histogram_quantile(0.99, rate(udp_director_k8s_query_duration_seconds_bucket[5m]))
```

## Alerting Rules

### Critical Alerts

```yaml
groups:
  - name: udp-director
    interval: 30s
    rules:
      - alert: UDPDirectorDown
        expr: up{job="udp-director"} == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "UDP Director is down"
          
      - alert: DefaultEndpointUnavailable
        expr: udp_director_default_endpoint_available == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Default endpoint is unavailable"
          
      - alert: HighErrorRate
        expr: rate(udp_director_errors_total[5m]) > 10
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High error rate detected"
          
      - alert: NoAvailableResources
        expr: udp_director_available_resources == 0
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "No available resources for {{ $labels.resource_type }}"
          
      - alert: HighQueryLatency
        expr: histogram_quantile(0.95, rate(udp_director_query_duration_seconds_bucket[5m])) > 0.5
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High query latency detected"
```

## Kubernetes Deployment

### Add Metrics Port to Deployment

```yaml
apiVersion: v1
kind: Service
metadata:
  name: udp-director-metrics
  namespace: game-servers
  labels:
    app: udp-director
spec:
  selector:
    app: udp-director
  ports:
    - name: metrics
      port: 9090
      targetPort: 9090
      protocol: TCP
  type: ClusterIP
```

### Update Pod Annotations

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: udp-director
spec:
  template:
    metadata:
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "9090"
        prometheus.io/path: "/metrics"
```

## Testing Metrics

### Manual Testing

```bash
# Check metrics endpoint
kubectl port-forward -n game-servers deployment/udp-director 9090:9090
curl http://localhost:9090/metrics

# Check health endpoint
curl http://localhost:9090/health
```

### Verify Metrics

```bash
# Check if metrics are being scraped
kubectl port-forward -n monitoring prometheus-0 9090:9090
# Open http://localhost:9090 and query: udp_director_active_sessions
```

## Performance Considerations

- Metrics collection has minimal overhead (~0.1% CPU)
- Histogram buckets are optimized for typical use cases
- High-cardinality labels (like `client_addr`) are used sparingly
- Metrics are updated atomically without locks

## Future Enhancements

- [ ] Add per-resource metrics (track individual gameserver connections)
- [ ] Add geographic distribution metrics (if applicable)
- [ ] Add SLO/SLI tracking metrics
- [ ] Add custom business metrics (e.g., game-specific events)

[← Back to README](../README.md)
