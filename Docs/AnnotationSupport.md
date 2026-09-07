[← Back to README](../README.md)

# Annotation Selector Support

UDP Director now supports filtering resources by **both labels and annotations**, following Kubernetes best practices for metadata organization.

## Kubernetes Best Practices

According to Kubernetes conventions:

- **Labels** = Static/identifying metadata (e.g., `maxPlayers: "64"`, `map: "de_dust2"`, `tier: "production"`)
  - Used for resource selection and organization
  - Server-side filtering via Kubernetes API
  - Indexed for efficient queries

- **Annotations** = Dynamic/operational data (e.g., `currentPlayers: "32"`, `playerList: "[...]"`, `lastUpdated: "2025-11-03T15:30:00Z"`)
  - Used for non-identifying metadata
  - Client-side filtering (retrieved then filtered)
  - Not indexed, can contain larger values

## Feature Overview

UDP Director supports both filtering mechanisms:

1. **Label Selector** - Server-side filtering (efficient, indexed)
2. **Annotation Selector** - Client-side filtering (flexible, supports dynamic data)

## Configuration

### Query Request Format

```json
{
  "type": "query",
  "resourceType": "gameserver",
  "namespace": "game-servers",
  "labelSelector": {
    "agones.dev/fleet": "tutorial",
    "map": "de_dust2",
    "maxPlayers": "64"
  },
  "annotationSelector": {
    "currentPlayers": "32",
    "status": "available"
  },
  "statusQuery": {
    "jsonPath": "status.state",
    "expectedValues": ["Ready", "Allocated"]
  }
}
```

### Default Endpoint Configuration

```yaml
defaultEndpoint:
  resourceType: "gameserver"
  namespace: "game-servers"
  
  # Static metadata - server-side filtering
  labelSelector:
    agones.dev/fleet: "tutorial"
    map: "de_dust2"
    maxPlayers: "64"
  
  # Dynamic metadata - client-side filtering
  annotationSelector:
    currentPlayers: "32"
    status: "available"
  
  # Status filtering
  statusQuery:
    jsonPath: "status.state"
    expectedValues:
      - "Ready"
      - "Allocated"
```

## Use Cases

### Game Server Matchmaking

**Scenario**: Find a game server with specific capacity requirements

```yaml
# Static server configuration (labels)
labelSelector:
  map: "de_dust2"
  maxPlayers: "64"
  gameMode: "competitive"

# Dynamic server state (annotations)
annotationSelector:
  currentPlayers: "48"  # Server has exactly 48 players
  status: "accepting"   # Server is accepting new players
```

### Capacity-Based Routing

**Scenario**: Route to servers with available capacity

```json
{
  "labelSelector": {
    "tier": "premium",
    "region": "us-east"
  },
  "annotationSelector": {
    "loadPercentage": "75"  // Server at 75% capacity
  }
}
```

### Dynamic Player Matching

**Scenario**: Find servers with specific player counts

```yaml
labelSelector:
  gameType: "battle-royale"
  
annotationSelector:
  playersInLobby: "95"  # Waiting for 100 players
  lobbyStatus: "filling"
```

## Implementation Details

### Filtering Order

1. **Label Selector** - Applied server-side by Kubernetes API (most efficient)
2. **Status Query** - Applied client-side via JSONPath evaluation
3. **Annotation Selector** - Applied client-side after status filtering

### Performance Considerations

- **Labels**: Indexed by Kubernetes, very fast
- **Annotations**: Not indexed, requires full resource retrieval
- **Best Practice**: Use labels for primary filtering, annotations for fine-grained selection

### Example Flow

```
1. Client Query:
   - labelSelector: {map: "de_dust2", maxPlayers: "64"}
   - annotationSelector: {currentPlayers: "32"}

2. Kubernetes API Query:
   - Filters by labels (server-side)
   - Returns ~10 matching servers

3. Client-Side Filtering:
   - Filters by annotations
   - Returns 2 servers with exactly 32 players

4. Query Route Selection:
   - Selects the first matching resource
   - Installs forwarding and returns ready with public director ports
```

## API Examples

### Query with Both Selectors

```bash
# Query for game server
echo '{
  "type": "query",
  "resourceType": "gameserver",
  "namespace": "game-servers",
  "labelSelector": {
    "agones.dev/fleet": "tutorial",
    "map": "de_dust2"
  },
  "annotationSelector": {
    "currentPlayers": "32",
    "status": "available"
  }
}' | nc <PROXY_IP> 9000
```

### Response

```json
{
  "status": "ready",
  "server": "game-server-0",
  "ports": {
    "game-udp": 7777,
    "game-tcp": 7777
  }
}
```

## GameServer Example

### Resource Definition

```yaml
apiVersion: agones.dev/v1
kind: GameServer
metadata:
  name: game-server-abc123
  labels:
    # Static configuration
    agones.dev/fleet: "tutorial"
    map: "de_dust2"
    maxPlayers: "64"
    gameMode: "competitive"
  annotations:
    # Dynamic operational data
    currentPlayers: "32"
    playerList: '["player1","player2","player3"]'
    status: "available"
    lastUpdated: "2025-11-03T15:30:00Z"
spec:
  # ... server spec ...
status:
  state: "Ready"
  address: "10.244.1.44"
  ports:
    - name: "default"
      port: 7777
```

### Query Configuration

```yaml
resourceQueryMapping:
  gameserver:
    group: "agones.dev"
    version: "v1"
    resource: "gameservers"
    addressPath: "status.address"
    portName: "default"
```

## Best Practices

### When to Use Labels

✅ **Use labels for**:
- Server configuration (maxPlayers, map, gameMode)
- Resource organization (tier, region, environment)
- Static metadata that doesn't change frequently
- Primary filtering criteria

### When to Use Annotations

✅ **Use annotations for**:
- Dynamic operational data (currentPlayers, loadPercentage)
- Frequently changing values
- Large data structures (playerList, configuration JSON)
- Fine-grained filtering after label selection

### Performance Tips

1. **Start with labels** - Filter the majority of resources server-side
2. **Use annotations sparingly** - Only for dynamic data that can't be labels
3. **Combine with status queries** - Use JSONPath for status field filtering
4. **Consider load balancing** - Let the load balancer handle final selection

## Limitations

- **Exact Match Only**: Both label and annotation selectors require exact string matches
- **No Operators**: Cannot use operators like `>`, `<`, `!=` (use statusQuery for complex logic)
- **Client-Side Filtering**: Annotations are filtered after retrieval, not by Kubernetes API
- **String Values Only**: All annotation values must be strings

## Related Documentation

- [Load Balancing](load-balancing.md) - Load balancing strategies
- [Technical Reference](TechnicalReference.md) - Complete configuration guide
- [Multi-Port Support](MultiPortSupport.md) - Multi-port configuration

[← Back to README](../README.md)
