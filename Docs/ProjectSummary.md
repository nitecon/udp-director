# UDP Director Project Summary

UDP Director is a public Kubernetes-native UDP/TCP router. The recovery removes
internal product dependencies and routing ticket machinery and restores TCP
query followed by direct gameplay forwarding through the regional director.

## Connection and character lookup

A server query uses configured Kubernetes resource mappings and selectors,
installs a forwarding route, and returns `ready` with the selected server name
and public director ports. Gameplay packets then flow through the director
without token redemption, setup datagrams, or reset packets.

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

The v2.0.0 gateway contract documents the implemented behavior. Initial
`Allocated` label integration and shared-NAT route identity remain unresolved. Current query routing is
keyed by TCP peer IP, so separate players sharing a public IP cannot independently
select different targets. Local tests do not establish live cluster readiness or
controller lifecycle integration. Recovery is not yet complete.

## References

- [TCP Query and Connection API](QueryAPI.md)
- [Minimal Pod configuration](../config.example.yaml)
- [Client example](../examples/client_example.rs)
- [Multi-port configuration](MultiPortSupport.md)
- [Kubernetes deployment](../k8s/k8s.md)
- [Metrics](Metrics.md)

[Back to README](../README.md)
