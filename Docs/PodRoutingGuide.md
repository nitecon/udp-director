[← Back to README](../README.md)

# Pod-Based Routing Guide

This guide explains how to configure UDP Director to route traffic directly to Kubernetes pods, bypassing services entirely.

## Overview

Pod-based routing allows UDP Director to:
- Route directly to pod IPs (no service overhead)
- Select pods using label selectors
- Filter pods by status (e.g., only "Running" pods)
- Extract ports by name or array index
- Support multi-container pods

## When to Use Pod-Based Routing

**Use pod-based routing when:**
- You have standard Kubernetes Deployments or StatefulSets
- You want to bypass service load balancing
- You need direct pod-to-pod communication
- You're running game servers without Agones
- You want fine-grained control over pod selection

**Don't use pod-based routing when:**
- You need service-level load balancing
- You're using Agones (use `configmap-agones-gameserver.yaml` instead)
- You need stable DNS names

## Configuration

### Basic Example

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: udp-director-config
data:
  config.yaml: |
    queryPort: 9000
    dataPort: 7777
    sessionTimeoutSeconds: 300
    controlPacketMagicBytes: "FFFFFFFF5245534554"

    defaultEndpoint:
      resourceType: "game-pod"
      namespace: "game-servers"
      labelSelector:
        app: "game-server"
        map: "de_dust2"
      statusQuery:
        jsonPath: "status.phase"
        expectedValues:
          - "Running"

    resourceQueryMapping:
      game-pod:
        group: ""
        version: "v1"
        resource: "pods"
        addressPath: "status.podIP"
        portName: "game-udp"
```

## Port Extraction Methods

UDP Director supports two methods for extracting ports from pods:

### Method 1: Port Name Lookup (Recommended)

The simplest and most maintainable approach. UDP Director searches all containers for a port with the specified name.

```yaml
resourceQueryMapping:
  game-pod:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portName: "game-udp"  # Searches all containers for this port name
```

**Pod spec example:**
```yaml
containers:
  - name: game-server
    ports:
      - name: game-udp
        containerPort: 7777
        protocol: UDP
      - name: game-tcp
        containerPort: 7777
        protocol: TCP
```

### Method 2: JSONPath with Array Indexing

More explicit but less flexible. Useful when you need to target specific containers or ports by position.

```yaml
resourceQueryMapping:
  game-pod:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portPath: "spec.containers[0].ports[0].containerPort"
```

**Syntax:**
- `spec.containers[0]` - First container
- `spec.containers[1]` - Second container
- `ports[0]` - First port
- `ports[1]` - Second port

## Complete Example

Here's a complete example with a game server deployment:

```yaml
---
# 1. Game Server Deployment
apiVersion: apps/v1
kind: Deployment
metadata:
  name: game-server
  namespace: game-servers
spec:
  replicas: 3
  selector:
    matchLabels:
      app: game-server
  template:
    metadata:
      labels:
        app: game-server
        map: de_dust2
    spec:
      containers:
        - name: game
          image: my-game-server:latest
          ports:
            - name: game-udp
              containerPort: 7777
              protocol: UDP
            - name: game-tcp
              containerPort: 7777
              protocol: TCP

---
# 2. UDP Director ConfigMap
apiVersion: v1
kind: ConfigMap
metadata:
  name: udp-director-config
  namespace: game-servers
data:
  config.yaml: |
    queryPort: 9000
    dataPort: 7777
    sessionTimeoutSeconds: 300
    controlPacketMagicBytes: "FFFFFFFF5245534554"

    defaultEndpoint:
      resourceType: "game-pod"
      namespace: "game-servers"
      labelSelector:
        app: "game-server"
      statusQuery:
        jsonPath: "status.phase"
        expectedValues:
          - "Running"

    resourceQueryMapping:
      game-pod:
        group: ""
        version: "v1"
        resource: "pods"
        addressPath: "status.podIP"
        portName: "game-udp"
```

## Advanced Configurations

### Multiple Resource Types

You can define multiple resource types for different query patterns:

```yaml
resourceQueryMapping:
  # UDP game traffic
  game-pod-udp:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portName: "game-udp"

  # TCP game traffic
  game-pod-tcp:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portName: "game-tcp"

  # Specific map routing
  game-pod-dust2:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portName: "game-udp"
```

### Multi-Container Pods

For pods with multiple containers, use array indexing:

```yaml
resourceQueryMapping:
  # Target first container
  game-pod-main:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portPath: "spec.containers[0].ports[0].containerPort"

  # Target second container (sidecar)
  game-pod-metrics:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    portPath: "spec.containers[1].ports[0].containerPort"
```

### Status Filtering

Filter pods by various status fields:

```yaml
# Only Running pods
statusQuery:
  jsonPath: "status.phase"
  expectedValues:
    - "Running"

# Pods on specific node
statusQuery:
  jsonPath: "spec.nodeName"
  expectedValues:
    - "node-1"
    - "node-2"

# Pods with specific condition
statusQuery:
  jsonPath: "status.conditions[0].type"
  expectedValues:
    - "Ready"
```

## Client Usage

Query the regional director over TCP:

```bash
printf '%s\n' '{"type":"query","map":"tutorial","characterId":"me","friendIds":["friend-1"]}' | nc <UDP-DIRECTOR-IP> 9000
```

A successful response identifies the server and public director port:

```json
{"status":"Allocated","server":"opaque-server-id","ports":{"default":7777}}
```

Send application datagrams directly to that director's UDP port. See
[Query API](QueryAPI.md) for character lookup, framing, and the current
connection identity limitation. No routing token or setup packet is required.

## Troubleshooting

### Pods Not Found

**Problem:** Query returns "No resources found"

**Solutions:**
1. Verify pod labels match the label selector
2. Check pod status (must be "Running" if using statusQuery)
3. Ensure RBAC permissions allow pod access
4. Verify namespace is correct

```bash
# Check pod labels
kubectl get pods -n game-servers --show-labels

# Check pod status
kubectl get pods -n game-servers -o wide

# Test RBAC
kubectl auth can-i list pods --as=system:serviceaccount:game-servers:udp-director
```

### Port Not Found

**Problem:** "Port with name 'game-udp' not found in resource"

**Solutions:**
1. Verify port name in pod spec matches portName in config
2. Check if port is defined in container spec
3. Try using portPath with array indexing instead

```bash
# Inspect pod ports
kubectl get pod <POD-NAME> -n game-servers -o jsonpath='{.spec.containers[*].ports[*]}'
```

### Wrong Pod IP

**Problem:** Traffic not reaching pod

**Solutions:**
1. Verify pod IP is accessible from UDP Director pod
2. Check network policies
3. Ensure CNI supports pod-to-pod communication

```bash
# Get pod IP
kubectl get pod <POD-NAME> -n game-servers -o jsonpath='{.status.podIP}'

# Test connectivity from UDP Director pod
kubectl exec -n game-servers <UDP-DIRECTOR-POD> -- ping <POD-IP>
```

## Performance Considerations

### Pod IP Stability

Pod IPs change when pods are recreated. UDP Director handles this by:
- Querying fresh pod IPs for each server query
- Maintaining session state even if pod IP changes mid-session
- Using control packets to migrate sessions to new pods

### Label Selector Efficiency

- Use specific labels to reduce query scope
- Avoid overly broad selectors that match many pods
- Consider using indexed labels for faster queries

### Caching

The director retains forwarding sessions until their configured timeout and
monitors backend resources. A proxy timeout does not report a player disconnect
or remove the controller-owned character label.

## Security Considerations

### RBAC Requirements

UDP Director needs permission to list and watch pods:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: udp-director-role
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "watch"]
```

### Network Policies

Ensure network policies allow:
- Client → UDP Director (ports 9000, 7777)
- UDP Director → Pods (game ports)

### Pod Security

- Use Pod Security Standards (restricted profile recommended)
- Run UDP Director with minimal privileges
- Use read-only root filesystem where possible

## See Also

- [configmap-pods.yaml](../k8s/configmap-pods.yaml) - Full ConfigMap example
- [example-pod-deployment.yaml](../k8s/example-pod-deployment.yaml) - Complete deployment example
- [Multi-Port Support](MultiPortSupport.md) - Configure multiple ports per pod
- [Technical Reference](TechnicalReference.md) - Advanced configuration options

[← Back to README](../README.md)
