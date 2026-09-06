[← Back to README](../README.md)

# Project X exact-Pod routing

Project X uses StarXAPI-authoritative, single-use allocations instead of
Agones or client-selected Kubernetes resources. Before StarXAPI publishes a
usable client token, the regional capacity controller installs the exact
allocation through the cluster-private admin listener:

```json
{
  "apiVersion":"runtime.games.nitecon.org/v1alpha1",
  "installId":"director:allocation-id",
  "ticketId":"ticket-id",
  "allocationId":"allocation-id",
  "routingToken":"opaque-routing-token",
  "podUid":"exact-pod-uid",
  "namespace":"project-x",
  "podName":"new-dawn-01-abcd",
  "map":"new-dawn-01",
  "build":"078e1fdc",
  "expiresAt":"2026-09-07T00:00:00Z"
}
```

The route is installed with `POST /v1alpha1/allocations`. Canonical request and
response fixtures live in StarXAPI under
`Docs/Fixtures/runtime-v1alpha1/director-install.json` and
`allocation-installed-response.json`. Repeating an identical `installId` is
idempotent. A conflicting `installId` or `routingToken` returns HTTP 409.

The director reads the named Pod and independently requires all of the
following before accepting the install:

- the current Kubernetes object has the exact reserved Pod UID;
- the Pod is `Running`, has a `Ready=True` condition, and is not terminating;
- the map and immutable build labels match the reservation; and
- the Pod has an assigned Pod IP.

Set `projectXAllocationOnly: true` in the mounted configuration to reject
legacy resource queries, token resets, and first-packet fallback routing. This
is required for Project X deployments.

## Gameplay-socket setup datagram

Project X clients bind a reservation to the public gameplay UDP socket by
sending one setup datagram to the director's normal UDP data endpoint (for
example, the advertised director address on port `7777`). The setup datagram
must originate from the exact socket that will send Unreal gameplay packets:

```text
FF FF FF FF 52 45 53 45 54 <raw UTF-8 routing token>
|--------- 9-byte magic ---------| |--- no JSON, NUL, or newline ---|
```

The default magic is configured by
`controlPacketMagicBytes: "FFFFFFFF5245534554"`. The routing token starts
at byte 9 and occupies the remainder of the datagram. The director consumes
this control datagram; it is never forwarded to Unreal.

Send the setup datagram immediately before the first Unreal handshake packet.
The director orders subsequent packets from that same `SocketAddr` behind the
controller reservation check, so the first handshake cannot overtake setup.
The director allows the local allocation and Pod checks up to 10 seconds. There
is no UDP acknowledgement. Repeating the same setup from the same gameplay
`SocketAddr` is idempotent, which covers a lost setup datagram. Replaying it
from another source address or port fails closed.

StarXAPI defines allocation expiry. A malformed, expired, replayed, or
unavailable-target reservation installs no route. If the gameplay socket
already had an exact route, that route remains unchanged; otherwise subsequent
gameplay is rejected while `projectXAllocationOnly` is enabled. Logs identify
the client socket and outcome but never include the allocation token.

Exact Project X routes are keyed by the complete public `SocketAddr`, so two
players behind one NAT address remain independent. Legacy query and reset
clients continue to use IP-keyed sessions when `projectXAllocationOnly` is
false. In that compatibility mode, the same magic packet carries a director
cache token and retains the historical IP-keyed reset behavior. The TCP
`{"type":"allocation"}` request also remains available as an IP-keyed legacy
bridge, but new Project X clients must use the gameplay-socket datagram.

## Drain and route observations

Draining is enforced before StarXAPI grants an allocation. Once a route exists,
changing the server to draining does not interrupt it; the normal inactivity
timeout retires it.

The private admin listener exposes:

- `GET /livez` and `GET /readyz`;
- `POST /v1alpha1/allocations`, accepting the authoritative install fixture;
- `GET /v1alpha1/pods/{podUID}/routes`, returning
  `{"activeRoutes":0,"lastNewRouteAt":"RFC3339"}`.

Route history remains available after the final route ends, so the controller
can observe a genuine zero and enforce its cooldown. Director install and route
state are intentionally memory-local; StarXAPI owns the durable allocation
ledger and the controller retries unfinished installs. After a director
restart, unknown tokens and unobserved Pod UIDs fail closed, and the latter
returns HTTP 503 instead of guessing zero. The Project X deployment therefore
uses one replica until shared route state is implemented.

The regional image is published as
`us-east4-docker.pkg.dev/nitecon-datacenter/starx/udp-director:<git-hash>`.
Environment promotion assigns aliases outside this repository without changing
the immutable image.

[← Back to README](../README.md)
