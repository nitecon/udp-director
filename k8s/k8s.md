# Kubernetes Deployment Guide

This directory contains Kubernetes manifests for deploying UDP Director.

## Quick Start

1. **Apply RBAC** (⚠️ REQUIRED for pod access):
   ```bash
   kubectl apply -f rbac.yaml -n <your-namespace>
   ```

2. **Choose a ConfigMap** based on your use case:
   - `configmap-pods.yaml` - Single-port pod routing
   - `configmap-pods-multiport.yaml` - **Multi-port pod routing (recommended)**
   - `configmap-agones-gameserver.yaml` - Agones GameServer routing
   - `configmap-agones-service.yaml` - Service-based routing (legacy)

3. **Apply ConfigMap**:
   ```bash
   kubectl apply -f configmap-pods-multiport.yaml -n <your-namespace>
   ```

4. **Deploy UDP Director**:
   ```bash
   kubectl apply -f deployment.yaml -n <your-namespace>
   ```

## Configuration Examples

### 📦 configmap-pods.yaml
**Use for**: Direct pod routing with single UDP port

**Features**:
- Routes directly to pod IPs (no service needed)
- Supports label-based pod selection
- Filters by pod phase (Running only)
- Port lookup by name (e.g., "game-udp")
- Default data port: 7777

**Example Pod Spec**:
```yaml
ports:
  - name: game-udp
    containerPort: 7777
    protocol: UDP
```

**Example Query**:
```json
{"resourceType": "game-server-pod", "namespace": "game-servers", "labelSelector": {"app": "demo-game-server", "map": "tutorial"}}
```

---

### 🚀 configmap-pods-multiport.yaml (Recommended)
**Use for**: Direct pod routing with multiple ports (UDP/TCP)

**Features**:
- **One TCP query installs mappings for all configured ports**
- Supports both UDP and TCP protocols
- Intelligent port-based routing
- Perfect for game servers with multiple ports

**Example Pod Spec**:
```yaml
ports:
  - name: game-udp
    containerPort: 7777
    protocol: UDP
  - name: game-tcp
    containerPort: 7777
    protocol: TCP
  - name: query
    containerPort: 27015
    protocol: UDP
```

**Example Query Response**:
```json
{
  "status": "Allocated",
  "server": "game-server-0",
  "ports": {
    "game-udp": 7777,
    "game-tcp": 7777,
    "query": 27015
  }
}
```

---

### 🎮 configmap-agones-gameserver.yaml
**Use for**: Agones GameServer routing

**Features**:
- Direct GameServer resource access
- No service discovery needed
- Supports state filtering (Ready, Allocated)
- Uses PodIP from addresses array

**Example Query**:
```json
{"resourceType": "gameserver", "namespace": "default", "labelSelector": {"agones.dev/fleet": "my-fleet"}}
```

---

### 🔧 configmap-agones-service.yaml (Legacy)
**Use for**: Service-based routing (not recommended)

**Note**: Direct pod/resource routing is faster and more reliable.

---

## Deployment Steps

### Complete Example (game-servers namespace)

```bash
# 1. Create namespace
kubectl create namespace game-servers

# 2. Apply RBAC (REQUIRED - grants pod access permissions)
kubectl apply -f rbac.yaml -n game-servers

# 3. Apply ConfigMap (choose one based on your needs)
kubectl apply -f configmap-pods-multiport.yaml -n game-servers

# 4. Deploy UDP Director
kubectl apply -f deployment.yaml -n game-servers

# 5. Verify deployment
kubectl get pods -n game-servers
kubectl logs -f deployment/udp-director -n game-servers
```

### Important: RBAC Configuration

The RBAC configuration **must** be applied before deployment. It grants UDP Director permissions to:
- List and watch Pods
- List and watch Services  
- List and watch ConfigMaps
- List and watch Agones GameServers (if using Agones)

**Update the namespace** in `rbac.yaml` ClusterRoleBinding to match your deployment namespace:
```yaml
subjects:
  - kind: ServiceAccount
    name: udp-director
    namespace: game-servers  # Change this to your namespace
```

### Switching ConfigMaps

To change configuration:

```bash
# Delete current configmap
kubectl delete configmap udp-director-config -n <namespace>

# Apply new configmap
kubectl apply -f configmap-pods.yaml -n <namespace>

# Restart deployment to pick up changes
kubectl rollout restart deployment/udp-director -n <namespace>
```

## Troubleshooting

### "Failed to list resources: pods"

**Cause**: Missing RBAC permissions for pod access.

**Solution**: Apply the RBAC configuration:
```bash
kubectl apply -f rbac.yaml -n <your-namespace>
```

Ensure the namespace in `rbac.yaml` ClusterRoleBinding matches your deployment namespace.

### "No matching resources found"

**Causes**:
1. No pods match the label selector
2. Pods are not in "Running" state
3. Wrong namespace specified

**Solution**: Check your pods and labels:
```bash
kubectl get pods -n <namespace> --show-labels
kubectl describe pod <pod-name> -n <namespace>
```

### "Port not found in resource"

**Cause**: Port name in config doesn't match pod spec.

**Solution**: Verify port names match:
```bash
# Check pod port names
kubectl get pod <pod-name> -n <namespace> -o jsonpath='{.spec.containers[*].ports[*].name}'

# Update configmap to match
```

## Example Pod Deployment

See `example-pod-deployment.yaml` for a complete example of a game server pod with proper port naming and labels.
