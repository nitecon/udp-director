# UDP Director Project Summary

UDP Director is a public Kubernetes-native UDP/TCP router. The recovery removes
internal product dependencies and routing ticket machinery and restores TCP
query followed by direct gameplay forwarding through the regional director.

## Connection and character lookup

A server query accepts map, joining character ID, and optional friend IDs. The
director uses its own configured Kubernetes mapping, chooses available capacity
with friend preference, writes `Allocated`, installs forwarding, and returns a
status with public director ports. Clients never supply Kubernetes fields.

The `characterList` query reads native Pod labels to find servers containing
requested characters. Labels represent `Allocated`, `Used`, and `Disconnected`;
a disconnect timestamp annotation provides the 120-second reconnect window.
The director filters expired disconnected characters from lookup. The controller
owns occupancy updates and label removal based on actual game-server events.

The backend API supplies persistent game data and region discovery. It does not
own per-player routing. Kubernetes owns workload lifecycle and reconciliation.
See [AGENTS.md](../AGENTS.md) for the binding architecture boundaries.

## Implementation map

| File | Responsibility |
|---|---|
| `src/query_server.rs` | TCP JSON query, resource selection, character lookup |
| `src/characters.rs` | Character label parsing and disconnect visibility |
| `src/k8s_client.rs` | Kubernetes resource discovery and Pod readiness |
| `src/session.rs` | Forwarding routes and upstream UDP sockets |
| `src/proxy.rs` | Application traffic forwarding and replies |
| `src/config.rs` | Resource mappings and port configuration |
| `src/resource_monitor.rs` | Backend resource monitoring |
| `src/metrics.rs` | Proxy and query metrics |

## Recovery status and verification

The routing token cache, reservation-controller interface, allocation request,
and UDP reset/setup exchange have been removed from the working implementation.
The TCP socket test exercises label query through a mocked Kubernetes API and
UDP forwarding to an actual local backend socket, including the reply. Character
and request-framing tests cover filtering, disconnect expiry, and fragmented
requests. See [Testing](Testing.md) for commands and verification limits.

The v3.0.0 contract replaces the raw infrastructure query. The socket test checks
friend lookup, preference over an empty server, the initial label patch, and
bidirectional gameplay forwarding. The existing shared-NAT route limitation
remains documented; live controller occupancy integration is separate.

## References

- [TCP Query and Connection API](QueryAPI.md)
- [Minimal Pod configuration](../config.example.yaml)
- [Client example](../examples/client_example.rs)
- [Multi-port configuration](MultiPortSupport.md)
- [Kubernetes deployment](../k8s/k8s.md)
- [Metrics](Metrics.md)

[Back to README](../README.md)
