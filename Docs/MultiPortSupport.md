# Multi-Port Support

One TCP server query installs the selected backend's configured port mappings.
Clients then send application traffic directly to the regional director's data
ports. See [Query API](QueryAPI.md) for request and response details and the
current connection identity limitation.

## Configuration

### Basic Multi-Port Setup

```yaml
queryPort: 9000
sessionTimeoutSeconds: 300

# Define multiple data ports that the proxy will listen on
dataPorts:
  - port: 7777
    protocol: "udp"
    name: "game-udp"
  - port: 7777
    protocol: "tcp"
    name: "game-tcp"
  - port: 27015
    protocol: "udp"
    name: "query"

# Default endpoint configuration
defaultEndpoint:
  resourceType: "game-pod"
  namespace: "game-servers"
  labelSelector:
    app: "game-server"

# Map proxy ports to container ports
resourceQueryMapping:
  game-pod:
    group: ""
    version: "v1"
    resource: "pods"
    addressPath: "status.podIP"
    # Multi-port mapping - each proxy port maps to a container port
    ports:
      - name: "game-udp"
        portName: "game-udp"  # Matches container port name
      - name: "game-tcp"
        portName: "game-tcp"  # Matches container port name
      - name: "query"
        portName: "query"     # Matches container port name
```

### Pod Specification

Your game server pods must expose the corresponding ports:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: game-server
  labels:
    app: game-server
spec:
  containers:
    - name: game
      image: game-server:latest
      ports:
        - name: game-udp      # Must match config
          containerPort: 7777
          protocol: UDP
        - name: game-tcp      # Must match config
          containerPort: 7777
          protocol: TCP
        - name: query         # Must match config
          containerPort: 27015
          protocol: UDP
```

## Client Usage

Query the director's TCP query port:

```bash
printf '%s\n' '{"type":"query","map":"tutorial","characterId":"me","friendIds":["friend-1"]}' | nc <DIRECTOR_IP> 9000
```

A successful response identifies the selected server and public director ports:

```json
{"status":"Allocated","server":"opaque-server-id","ports":{"game-udp":7777,"game-tcp":7777,"query":27015}}
```

After `Allocated`, send normal game, RCON, or query protocol traffic to the
corresponding director port. There is no routing token or setup packet.
`Allocated` confirms forwarding setup; actual server connection events determine
player occupancy.

## How It Works

1. The TCP query matches a backend using the configured Kubernetes selectors.
2. The director installs the backend port mappings for the client's route.
3. Traffic arriving on each configured data port forwards to its mapped backend
   port, and replies return to the originating client socket.

## Backwards Compatibility

Single-port configurations still work:

```yaml
# Old config (still supported)
dataPort: 7777

# Automatically converted to:
dataPorts:
  - port: 7777
    protocol: "udp"
    name: "default"
```

## Complete Example

See `k8s/configmap-pods-multiport.yaml` for a complete working example.

## Troubleshooting

### Missing port mapping

Ensure each named data port has a corresponding resource port mapping and that
the selected Pod exposes that named container port. Connect to the director's
returned public port, not directly to the container port.

### Port Name Mismatch

**Issue:** "Port with name 'X' not found in resource"

**Solution:** Verify port names in your pod spec match the configuration:
```bash
kubectl get pod <pod-name> -o jsonpath='{.spec.containers[*].ports[*].name}'
```

## Related Documentation

- [Pod Routing Guide](PodRoutingGuide.md) - Pod-based routing configuration
- [Kubernetes Deployment](../k8s/k8s.md) - Deployment examples

[← Back to README](../README.md)
