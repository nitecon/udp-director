[← Back to README](../README.md)

# UDP Director - Technical Reference

This document provides in-depth technical details for developers and operators working with UDP Director internals.

**Status**: Routing recovery in progress
**Status**: Production Ready  
**Target**: Cilium Service Mesh on Kubernetes

---

## Query and routing

[Query API](QueryAPI.md) defines the TCP JSON contract, character lookup, Pod
metadata, and unresolved integration boundaries. A successful query installs a
forwarding route and returns `ready` with public director ports. Application
traffic then goes directly through those ports without a control datagram.

## Session Management Internals

The restored query path keys the route by TCP peer IP. For UDP, each director
port and client source port pair receives its own upstream socket, preserving
the return path to the originating client socket. This does not provide distinct
selected targets for players sharing one public IP; see the API document.

A subsequent successful query replaces the selected route. Background session
cleanup expires inactive forwarding state according to `sessionTimeoutSeconds`.
Forwarding inactivity is not an authoritative player disconnect and must not
change character occupancy labels.

Character lookup reads Pod labels and disconnect timestamps. It excludes expired
disconnected entries at 120 seconds; label mutation and removal belong to the
controller's lifecycle contract.

## Kubernetes API Integration

### Resource Query Flow

1. Parse the TCP JSON query and resolve its configured resource mapping.
2. Query Kubernetes with the label selector and apply annotation/status filters.
3. For core Pods, require Running, Ready, nonterminating state and a Pod IP.
4. Select the first matching resource and extract its configured backend ports.
5. Install forwarding for the client and return the resource name with public
   director ports. A failed query does not install a new route.

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
- One TCP query installs the configured port mappings
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

See [Metrics](Metrics.md) for the implemented Prometheus metrics. Use forwarding
and query logs to diagnose route selection and network failures. These proxy
metrics do not establish authoritative player occupancy.

[Back to README](../README.md)
