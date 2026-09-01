[← Back to README](../README.md)

# UDP Director - Technical Reference

This document provides in-depth technical details for developers and operators working with UDP Director internals.

**Version**: 1.3 (Rust Edition w/ Session Reset)  
**Status**: Production Ready  
**Target**: Cilium Service Mesh on Kubernetes

---

## Table of Contents

1. [Control Packet Protocol](#control-packet-protocol)
2. [Session Management Internals](#session-management-internals)
3. [Token Cache Implementation](#token-cache-implementation)
4. [Kubernetes API Integration](#kubernetes-api-integration)
5. [Performance Tuning](#performance-tuning)
6. [Security Considerations](#security-considerations)
7. [Advanced Configuration](#advanced-configuration)

---

## Control Packet Protocol

### Packet Format Specification

**Control Packet Structure**:
```
[Magic Bytes (9 bytes)][Token (36 bytes)]
```

**Default Magic Bytes** (hex): `FFFFFFFF5245534554`
- Decoded: `[0xFF, 0xFF, 0xFF, 0xFF, 'R', 'E', 'S', 'E', 'T']`
- First 4 bytes: `0xFFFFFFFF` (unlikely to appear in normal game data)
- Next 5 bytes: ASCII "RESET"

**Token Format**: UUID v4 (36 bytes ASCII)
- Example: `550e8400-e29b-41d4-a716-446655440000`

### Packet Detection Algorithm

```rust
fn is_control_packet(packet: &[u8], magic_bytes: &[u8]) -> bool {
    packet.len() >= magic_bytes.len() && 
    packet.starts_with(magic_bytes)
}
```

**Performance**: O(1) prefix check, < 100ns overhead per packet

### Custom Magic Bytes

You can customize the magic bytes to avoid conflicts:

```yaml
# Use a different sequence
controlPacketMagicBytes: "DEADBEEF52535400"  # 8 bytes

# Or longer for more uniqueness
controlPacketMagicBytes: "FFFFFFFF52455345545F5632"  # "RESET_V2"
```

**Requirements**:
- Must be valid hex string
- Recommended: 8-16 bytes
- Should not appear in normal application data
- Must be consistent across all clients

---

## Session Management Internals

### Session State Machine

```
[No Session] 
    │
    ├─ First packet is valid token
    │  └─> [Active Session] (Client → Target A)
    │
    └─ First packet is not token
       └─> [Active Session] (Client → Default Endpoint)

[Active Session]
    │
    ├─ Receives data packet
    │  └─> Forward to target, reset timeout
    │
    ├─ Receives control packet with valid token
    │  └─> Update target to Target B, reset timeout
    │
    ├─ Receives control packet with invalid token
    │  └─> Drop packet, log warning, keep existing session
    │
    └─ Inactive for sessionTimeoutSeconds
       └─> [Session Cleaned Up]
```

### Data Structures

**Session Entry**:
```rust
struct Session {
    target_ip: String,
    port_mappings: HashMap<(u16, Protocol), u16>,
    last_activity: Instant,
    udp_sockets: HashMap<(u16, u16), SessionSocket>,
    // UDP socket key: (director proxy port, client source port)
}
```

**Session Map**:
```rust
DashMap<IpAddr, Session>  // Client IP → active route
```

Route selection is keyed by client IP so a query socket and a later game socket
can use different source ports while sharing the newly selected destination.
Within that route, each `(proxy port, client source port)` gets a dedicated
upstream UDP socket. This prevents overlapping old and new game sockets during
travel from being multiplexed into the same backend UDP flow.

**Cleanup Strategy**:
- Background task runs every 30 seconds
- Removes sessions inactive > `sessionTimeoutSeconds`
- Lock-free concurrent access via DashMap

### Memory Usage

- **Per Session**: ~48 bytes (SocketAddr + Instant + overhead)
- **1,000 sessions**: ~48 KB
- **10,000 sessions**: ~480 KB
- **Base overhead**: ~128 MB (runtime, caches, etc.)

---

## Token Cache Implementation

### Cache Architecture

**Technology**: moka (high-performance, TTL-based cache)

**Characteristics**:
- Lock-free concurrent access
- Automatic expiration (TTL)
- O(1) lookup and insert
- Memory-bounded

### Token Generation

```rust
use uuid::Uuid;

let token = Uuid::new_v4().to_string();
// Example: "550e8400-e29b-41d4-a716-446655440000"
```

**Security Properties**:
- 122 bits of randomness
- Cryptographically secure RNG
- Collision probability: negligible (< 10^-18)

### TTL Behavior

```
T=0s:  Token generated, inserted into cache
T=15s: Token still valid
T=30s: Token expires, automatically removed
T=31s: Lookup returns None
```

**Configuration**:
```yaml
tokenTTLSeconds: 30  # Adjust based on network latency
```

**Recommendations**:
- LAN: 15-30 seconds
- WAN: 30-60 seconds
- High-latency: 60-120 seconds

---

## Kubernetes API Integration

### Resource Query Flow

```
1. Client sends query JSON
2. Parse resourceType, namespace, filters
3. Look up GVR from resourceQueryMapping
4. Query K8s API: GET /apis/{group}/{version}/namespaces/{ns}/{resource}
5. Apply label selector (server-side)
6. Apply status query (client-side JSONPath)
7. Select first matching resource
8. Find Service with serviceSelectorLabel
9. Extract clusterIP and port
10. Generate token, cache target
11. Return token to client
```

### JSONPath Status Queries

**Syntax**: Simple dot-notation paths

Examples:
```yaml
# Check if GameServer is Allocated
statusQuery:
  jsonPath: "status.state"
  expectedValue: "Allocated"

# Check if Pod is Running
statusQuery:
  jsonPath: "status.phase"
  expectedValue: "Running"

# Check custom field
statusQuery:
  jsonPath: "status.players.current"
  expectedValue: "0"
```

**Limitations**:
- Only supports simple paths (no arrays, no filters)
- Exact string match only
- For complex queries, use label selectors instead

### Label and Annotation Filtering

UDP Director supports both **labels** and **annotations** following Kubernetes best practices:

**Labels** (Server-Side Filtering):
- Static/identifying metadata (e.g., `maxPlayers`, `map`, `tier`)
- Indexed by Kubernetes for fast queries
- Applied server-side before resources are retrieved
- Use for primary filtering criteria

```yaml
labelSelector:
  agones.dev/fleet: "my-fleet"
  map: "de_dust2"
  maxPlayers: "64"
```

**Annotations** (Client-Side Filtering):
- Dynamic/operational data (e.g., `currentPlayers`, `status`, `lastUpdated`)
- Not indexed, filtered after retrieval
- Use for fine-grained selection on dynamic values
- Supports larger values and frequently changing data

```yaml
annotationSelector:
  currentPlayers: "32"
  status: "accepting-players"
  region: "us-east"
```

**Filtering Order**:
1. Label selector (server-side, most efficient)
2. Status query (client-side JSONPath)
3. Annotation selector (client-side exact match)
4. Load balancer selection (if configured)

**Best Practices**:
- Use labels for static configuration that doesn't change
- Use annotations for dynamic operational data
- Start with labels to reduce the resource set, then use annotations for fine-tuning
- See [Annotation Support](AnnotationSupport.md) for detailed examples

### Service Discovery

**Requirements**:
1. Service must have label: `{serviceSelectorLabel}: {resource-name}`
2. Service must have named port: `{serviceTargetPortName}`
3. Service must have `clusterIP` (not headless)

**Example**:
```yaml
# Resource
metadata:
  name: gameserver-abc123

# Service
metadata:
  name: gameserver-abc123-service
  labels:
    agones.dev/gameserver: gameserver-abc123  # Matches serviceSelectorLabel
spec:
  clusterIP: 10.96.1.50
  ports:
    - name: default  # Matches serviceTargetPortName
      port: 7777
```

### RBAC Requirements

Minimum permissions:
```yaml
rules:
  - apiGroups: [""]
    resources: ["services"]
    verbs: ["get", "list", "watch"]
  
  - apiGroups: ["agones.dev"]
    resources: ["gameservers"]
    verbs: ["get", "list", "watch"]
```

**Security**: Read-only access, no mutations

---

## Performance Tuning

### Kubernetes API Optimization

**Problem**: K8s API calls add latency to query phase

**Solutions**:
1. **Use label selectors** (server-side filtering)
   ```yaml
   labelSelector:
     "game.example.com/map": "de_dust2"
   ```
   
2. **Cache resource lists** (future enhancement)
   - Watch API for changes
   - Maintain in-memory resource cache
   - Reduce API calls from O(queries) to O(1)

3. **Use resourceVersion** for efficient watches

### UDP Proxy Optimization

**Current**: Single socket, async I/O

**Bottlenecks**:
- Packet inspection (magic byte check)
- Session lookup (DashMap)
- Socket send/recv

**Optimizations Applied**:
- Zero-copy where possible
- Lock-free data structures
- Async I/O (no blocking)
- Minimal allocations

**Future Enhancements**:
- Dedicated socket per session (full bi-directional)
- SO_REUSEPORT for multi-threaded receive
- eBPF for packet steering (Cilium integration)

### Scaling

**Vertical Scaling**:
```yaml
resources:
  requests:
    cpu: 500m
    memory: 256Mi
  limits:
    cpu: 2000m
    memory: 1Gi
```

**Horizontal Scaling**:
- Requires session affinity (not yet implemented)
- Use consistent hashing on client IP
- Or use single replica with vertical scaling

---

## Security Considerations

### Token Security

**Threat Model**:
- **Token Interception**: Tokens sent in plaintext over UDP
- **Token Replay**: Attacker reuses captured token
- **Token Guessing**: Attacker tries to guess valid tokens

**Mitigations**:
- Short TTL (30s default) limits replay window
- UUIDv4 has 122 bits entropy (guessing infeasible)
- Use Cilium encryption for network-level security
- Tokens are single-use (consumed on first use)

**Recommendations**:
- Deploy in trusted network (VPC, private subnet)
- Use Cilium WireGuard encryption
- Implement client IP allowlisting (future)
- Add HMAC signatures (future)

### RBAC Isolation

**Principle**: Least privilege

```yaml
# UDP Director can only READ resources
verbs: ["get", "list", "watch"]

# Cannot create, update, or delete
# Cannot access secrets or configmaps outside its namespace
```

### Network Policies

**Recommended**:
```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: udp-director-policy
spec:
  podSelector:
    matchLabels:
      app: udp-director
  policyTypes:
    - Ingress
    - Egress
  ingress:
    - from:
        - podSelector: {}  # Allow from all pods
      ports:
        - port: 9000
          protocol: TCP
        - port: 7777
          protocol: UDP
  egress:
    - to:
        - namespaceSelector: {}  # Allow to all namespaces
      ports:
        - port: 7777
          protocol: UDP
    - to:  # K8s API
        - namespaceSelector:
            matchLabels:
              name: kube-system
      ports:
        - port: 443
          protocol: TCP
```

---

## Advanced Configuration

### ConfigMap Selection

UDP Director provides pre-configured ConfigMaps for common use cases:

**`k8s/configmap-agones-gameserver.yaml`** - Agones GameServer (Recommended)
- Direct resource inspection with label and annotation filtering
- Extracts address and port directly from GameServer status
- Demonstrates both static (labels) and dynamic (annotations) filtering
- Default data port: 7777

**`k8s/configmap-pods-multiport.yaml`** - Multi-Port Pod Routing
- Single token provides access to multiple ports
- Includes label and annotation filtering examples
- Ideal for game servers with multiple service ports

**`k8s/configmap-pods.yaml`** - Single Port Pod Routing
- Simple single-port configuration
- Direct pod access with label and annotation filtering

**`k8s/configmap-advanced-annotations.yaml`** - Advanced Annotation Filtering
- Demonstrates dynamic filtering with annotations
- Combines with label-based arithmetic load balancing
- Shows best practices for capacity-aware routing

**`k8s/configmap-agones-service.yaml`** - Service-Based Routing (Legacy)
- Routes through Kubernetes Services
- Use when direct pod access is not available

Deploy only the ConfigMap you need:
```bash
# For Agones with direct resource inspection (recommended)
kubectl apply -f k8s/configmap-agones-gameserver.yaml

# OR for Agones with service-based lookup
kubectl apply -f k8s/configmap-agones-service.yaml

# OR for DNS routing
kubectl apply -f k8s/configmap-dns.yaml

# OR for NTP routing
kubectl apply -f k8s/configmap-ntp.yaml
```

### Resource Inspection Approaches

UDP Director supports two approaches for finding target addresses:

#### 1. Direct Resource Inspection (Recommended)
Extract address and port directly from the resource itself using JSONPath:

```yaml
gameserver:
  group: "agones.dev"
  version: "v1"
  resource: "gameservers"
  addressPath: "status.address"      # Extract IP from resource
  portName: "default"                 # Look up port by name
  # OR
  # portPath: "status.ports[0].port" # Extract port via JSONPath
```

**Benefits**:
- No service discovery needed
- Works with any CRD that exposes address/port in status
- Simpler configuration
- Direct access to resource metadata (labels, annotations, status)

**Use for**: GameServers, Pods, or any custom resources with status.address

#### 2. Service-Based Lookup (Legacy)
Find a Service linked to the resource via labels:

```yaml
dns:
  group: ""
  version: "v1"
  resource: "services"
  serviceSelectorLabel: "k8s-app"
  serviceTargetPortName: "dns"
```

**Use for**: Services, or when you need to route through a Service abstraction

### Multiple Resource Types

You can customize any ConfigMap to support multiple resource types:

```yaml
resourceQueryMapping:
  gameserver:
    group: "agones.dev"
    version: "v1"
    resource: "gameservers"
    serviceSelectorLabel: "agones.dev/gameserver"
    serviceTargetPortName: "default"
  
  lobby:
    group: "apps"
    version: "v1"
    resource: "deployments"
    serviceSelectorLabel: "app"
    serviceTargetPortName: "lobby"
  
  custom:
    group: "example.com"
    version: "v1alpha1"
    resource: "customgames"
    serviceSelectorLabel: "game.example.com/instance"
    serviceTargetPortName: "game-port"
```

### Environment Variables

```yaml
env:
  - name: CONFIG_PATH
    value: "/etc/udp-director/config.yaml"
  
  - name: RUST_LOG
    value: "udp_director=info"
    # Options: error, warn, info, debug, trace
  
  - name: RUST_BACKTRACE
    value: "1"  # Enable backtraces on panic
```

### Health Checks

**Liveness Probe**: TCP socket check on query port
```yaml
livenessProbe:
  tcpSocket:
    port: 9000
  initialDelaySeconds: 10
  periodSeconds: 10
```

**Readiness Probe**: Same as liveness
```yaml
readinessProbe:
  tcpSocket:
    port: 9000
  initialDelaySeconds: 5
  periodSeconds: 5
```

**Future**: HTTP health endpoint with metrics

---

## Monitoring and Observability

### Logging

**Levels**:
- `error`: Critical failures
- `warn`: Recoverable issues (invalid tokens, timeouts)
- `info`: Normal operations (session created, token generated)
- `debug`: Detailed flow (packet inspection, K8s queries)
- `trace`: Very verbose (every packet)

**Key Log Messages**:
```
INFO  Session established: 192.168.1.100:12345 -> 10.96.1.50:7777
INFO  Session reset: 192.168.1.100:12345 -> 10.96.1.51:7777
WARN  Invalid token in control packet from 192.168.1.100:12345
INFO  Cleaned up 5 timed-out sessions. Active sessions: 42
```

### Metrics (Future)

Planned Prometheus metrics:
- `udp_director_queries_total` - Total queries received
- `udp_director_queries_success` - Successful queries
- `udp_director_sessions_active` - Current active sessions
- `udp_director_sessions_created_total` - Total sessions created
- `udp_director_sessions_reset_total` - Total session resets
- `udp_director_packets_proxied_total` - Total packets proxied
- `udp_director_query_duration_seconds` - Query latency histogram

---

[← Back to README](../README.md)
