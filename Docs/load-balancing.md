[← Back to README](../README.md)

# Load Balancing

UDP Director supports intelligent load balancing to distribute client connections across a pool of backend resources (pods, game servers, etc.).

## Overview

When clients connect without a query-established route (using the default endpoint), the proxy needs to select which backend to route them to. Load balancing strategies determine how this selection is made to ensure optimal distribution of traffic.

## Strategies

### 1. Least Sessions (Default)

Routes new connections to the backend with the fewest active sessions.

**Configuration:**
```yaml
loadBalancing:
  type: "leastSessions"
```

**How it works:**
- The proxy tracks the number of active sessions per backend IP address
- When a new client connects, it queries all available backends
- Selects the backend with the lowest session count
- Automatically balances load as clients connect and disconnect

**Use cases:**
- Simple, effective load balancing for most scenarios
- Works without any special labels on your resources
- Ideal when all backends have similar capacity

**Example:**
If you have 3 game servers with 5, 10, and 3 active sessions respectively, new connections will be routed to the server with 3 sessions.

### 2. Label-Based Arithmetic

Routes connections based on arithmetic evaluation of resource labels, preventing backends from exceeding capacity.

**Configuration:**
```yaml
loadBalancing:
  type: "labelArithmetic"
  currentLabel: "currentUsers"
  maxLabel: "maxUsers"
  overlap: 2
```

**Parameters:**
- `currentLabel`: Label containing the current user/player count on the backend
- `maxLabel`: Label containing the maximum capacity of the backend
- `overlap`: (Optional, default: 0) Extra capacity buffer for concurrent proxy instances

**How it works:**
1. Reads `currentLabel` and `maxLabel` from each backend resource
2. Calculates available capacity: `available = max - current - sessions - overlap`
3. Only considers backends with `available > 0`
4. Selects the backend with the most available capacity
5. Ties are broken by choosing the backend with the lowest current load

**Formula:**
```
Backend is eligible if: current + sessions + overlap <= max
```

Where:
- `current` = value from the resource label (e.g., players already on the server)
- `sessions` = active proxy sessions to this backend
- `overlap` = configured overlap allowance
- `max` = maximum capacity from the resource label

**Use cases:**
- Game servers that track their own player count
- Backends with varying capacity
- Preventing servers from becoming overloaded
- Allowing "friends joining" scenarios with the overlap buffer
- Multi-proxy deployments where race conditions may occur

**Example:**

Given 3 game servers with labels:

| Server | currentUsers | maxUsers | Proxy Sessions | Available Capacity |
|--------|--------------|----------|----------------|-------------------|
| A      | 45           | 50       | 2              | 50-45-2-2 = 1     |
| B      | 30           | 50       | 5              | 50-30-5-2 = 13    |
| C      | 48           | 50       | 1              | 50-48-1-2 = -1    |

With `overlap: 2`:
- Server A: Available capacity = 1 (eligible)
- Server B: Available capacity = 13 (eligible, **selected**)
- Server C: Available capacity = -1 (not eligible, at capacity)

New connections will be routed to Server B.

## Related Documentation

- [Configuration Reference](TechnicalReference.md) - Complete configuration options
- [Multi-Port Support](MultiPortSupport.md) - Multi-port routing configuration

## Configuration

### Default Behavior

If no `loadBalancing` configuration is specified, the proxy defaults to the "leastSessions" strategy:

```yaml
# No loadBalancing section = defaults to leastSessions
queryPort: 9000
dataPort: 7777
# ... rest of config
```

### Least Sessions Example

```yaml
loadBalancing:
  type: "leastSessions"
```

### Label Arithmetic Example

```yaml
loadBalancing:
  type: "labelArithmetic"
  currentLabel: "currentUsers"
  maxLabel: "maxUsers"
  overlap: 2
```

## Resource Label Requirements

For label-based arithmetic load balancing, your backend resources must have the appropriate labels.

### Agones GameServer Example

```yaml
apiVersion: agones.dev/v1
kind: GameServer
metadata:
  name: game-server-1
  labels:
    currentUsers: "45"
    maxUsers: "50"
spec:
  # ... gameserver spec
status:
  state: Ready
  address: 10.0.0.1
  ports:
  - name: default
    port: 7777
```

### Kubernetes Pod Example

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: game-pod-1
  labels:
    currentUsers: "12"
    maxUsers: "20"
spec:
  # ... pod spec
```

**Important:** 
- Labels must be strings containing valid integers
- Missing `currentLabel` defaults to 0
- Missing `maxLabel` causes the backend to be skipped
- The game server/application is responsible for updating these labels

## Session Tracking

The load balancer tracks active sessions per backend:

### Session Lifecycle

1. **Session Creation**: When a client connects, the proxy:
   - Queries available backends
   - Applies the load balancing strategy
   - Selects the best backend
   - Increments the session count for that backend

2. **Session Activity**: While the session is active:
   - The session count remains incremented
   - The backend is considered "loaded" by this amount

3. **Session Cleanup**: When a session times out or is removed:
   - The session count is automatically decremented
   - The backend's available capacity increases

### Multi-Proxy Deployments

When running multiple UDP Director instances:

- Each proxy maintains its own session counts
- The `overlap` parameter helps prevent race conditions
- Backends should update their `currentLabel` to reflect actual load
- Some over-subscription may occur temporarily

**Example:**
```yaml
loadBalancing:
  type: "labelArithmetic"
  currentLabel: "currentUsers"
  maxLabel: "maxUsers"
  overlap: 3  # Allow 3 extra connections for 3 concurrent proxies
```

## Monitoring

The load balancer provides logging for debugging:

```
INFO Selected backend 'game-server-2' (10.0.0.2) with 15 available capacity (current=30, 5 candidates)
DEBUG Backend 'game-server-1' (10.0.0.1): current=45, max=50, sessions=2, overlap=2, available=1
DEBUG Backend 'game-server-3' (10.0.0.3) is at capacity (available=-1)
```

## Best Practices

### For Least Sessions Strategy

1. **Homogeneous Backends**: Works best when all backends have similar capacity
2. **Simple Setup**: No label management required
3. **Fast Selection**: Minimal computation overhead

### For Label Arithmetic Strategy

1. **Update Labels Regularly**: Game servers should update their `currentUsers` label frequently
2. **Set Realistic Max**: The `maxUsers` label should reflect true capacity
3. **Tune Overlap**: 
   - Single proxy: `overlap: 0`
   - Multiple proxies: `overlap: <number of proxies>`
   - Allow friends: `overlap: <friend group size>`
4. **Monitor Capacity**: Watch for backends consistently at max capacity
5. **Handle Missing Labels**: Ensure all backends have required labels

### General

1. **Session Timeout**: Configure appropriate `sessionTimeoutSeconds` to free capacity
2. **Health Checks**: Use `statusQuery` to only route to healthy backends
3. **Scaling**: Add more backends when all are near capacity
4. **Testing**: Test load balancing with multiple concurrent connections

## Troubleshooting

### No backends available

**Error:** `No backends available with capacity`

**Causes:**
- All backends are at max capacity
- Missing required labels (`maxLabel`)
- Invalid label values (non-integer)

**Solutions:**
- Scale up your backend pool
- Increase `maxUsers` labels on backends
- Verify labels are present and valid
- Reduce `overlap` if too conservative

### Uneven distribution

**Symptom:** Some backends have many more sessions than others

**Causes:**
- Backends added/removed during operation
- Long-lived sessions on specific backends
- Label updates not reflecting actual load

**Solutions:**
- Ensure backends update their `currentLabel` regularly
- Monitor session timeout settings
- Consider shorter session timeouts if appropriate

### Race conditions in multi-proxy setup

**Symptom:** Backends occasionally exceed max capacity

**Causes:**
- Multiple proxies selecting the same backend simultaneously
- `overlap` parameter too small

**Solutions:**
- Increase `overlap` to match number of proxy instances
- Ensure backends update labels quickly
- Accept minor over-subscription as normal

## Migration

### From No Load Balancing

If you're currently using UDP Director without load balancing:

1. No configuration changes required (defaults to leastSessions)
2. Or explicitly configure:
   ```yaml
   loadBalancing:
     type: "leastSessions"
   ```

### To Label Arithmetic

1. Add labels to your backend resources:
   ```yaml
   labels:
     currentUsers: "0"
     maxUsers: "50"
   ```

2. Update your application to maintain the `currentUsers` label

3. Configure UDP Director:
   ```yaml
   loadBalancing:
     type: "labelArithmetic"
     currentLabel: "currentUsers"
     maxLabel: "maxUsers"
     overlap: 2
   ```

4. Deploy and monitor

## Examples

See:
- `config.example.yaml` - Basic configuration with leastSessions
- `config.loadbalancing.example.yaml` - Advanced label arithmetic configuration
