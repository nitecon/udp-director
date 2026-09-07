[← Back to README](../README.md)

# Reservation-controller exact-Pod routing

Deployments use controller-issued, single-use allocation reservations instead of
Agones or client-selected Kubernetes resources. The public query listener
accepts this request:

```json
{"type":"allocation","token":"controller-reservation-token"}
```

The director consumes the reservation through the regional capacity controller
using its projected Kubernetes service-account token. The controller confirms
that the reservation is unexpired and that its server agent still reports
`Ready` and `Accepting`. The director then reads the named Pod and independently
requires all of the following before creating the route:

- the current Kubernetes object has the exact reserved Pod UID;
- the Pod is `Running`, has a `Ready=True` condition, and is not terminating;
- the map and immutable build labels match the reservation; and
- the Pod has an assigned Pod IP.

Set `reservationOnly: true` in the mounted configuration to reject
legacy resource queries, token resets, and first-packet fallback routing. This
is required for reservation-controller deployments.

Configure the controller integration with environment variables:

- `RESERVATION_CONTROLLER_URL` enables the integration and sets the controller
  base URL.
- `RESERVATION_CONTROLLER_TOKEN_FILE` selects the projected identity-token
  file. It defaults to `/var/run/secrets/reservation-controller/token`.
- `RESERVATION_ADMIN_PORT` selects the private health and route-status port. It
  defaults to `8080`.
- `RESERVATION_MAP_LABEL` and `RESERVATION_BUILD_LABEL` select the Pod label
  keys used for exact-target verification. They default to `map` and `build`.

## Gameplay-socket setup datagram

Clients bind a reservation to the public gameplay UDP socket by
sending one setup datagram to the director's normal UDP data endpoint (for
example, the advertised director address on port `7777`). The setup datagram
must originate from the exact socket that will send gameplay packets:

```text
FF FF FF FF 52 45 53 45 54 <raw UTF-8 allocation token>
|--------- 9-byte magic ---------| |--- no JSON, NUL, or newline ---|
```

The default magic is configured by
`controlPacketMagicBytes: "FFFFFFFF5245534554"`. The allocation token starts
at byte 9 and occupies the remainder of the datagram. The director consumes
this control datagram; it is never forwarded to the backend.

Send the setup datagram immediately before the first gameplay handshake packet.
The director orders subsequent packets from that same `SocketAddr` behind the
controller reservation check, so the first handshake cannot overtake setup.
The director allows the controller and Pod checks up to 10 seconds. There is no
UDP acknowledgement. A token is single-use at the capacity controller; clients
must not retry the same setup datagram. On a send error or connection timeout,
request a fresh reservation instead.

The controller defines reservation expiry. A malformed, expired, replayed, or
unavailable-target reservation installs no route. If the gameplay socket
already had an exact route, that route remains unchanged; otherwise subsequent
gameplay is rejected while `reservationOnly` is enabled. Logs identify
the client socket and outcome but never include the allocation token.

Exact reservation routes are keyed by the complete public `SocketAddr`, so two
players behind one NAT address remain independent. Legacy query and reset
clients continue to use IP-keyed sessions when `reservationOnly` is
false. In that compatibility mode, the same magic packet carries a director
cache token and retains the historical IP-keyed reset behavior. The TCP
`{"type":"allocation"}` request also remains available as an IP-keyed legacy
bridge, but new reservation clients must use the gameplay-socket datagram.

## Drain and route observations

Draining is enforced when the controller atomically consumes a reservation.
Once a route exists, changing the agent to draining does not interrupt it; the
normal inactivity timeout retires it.

The private admin listener exposes:

- `GET /livez` and `GET /readyz`;
- `GET /v1alpha1/pods/{podUID}/routes`, returning
  `{"activeRoutes":0,"lastNewRouteAt":"RFC3339"}`.

Route history remains available after the final route ends, so the controller
can observe a genuine zero and enforce its cooldown. State is intentionally
memory-local. After a director restart, an unobserved Pod UID returns HTTP 503
instead of guessing zero, which makes automatic Pod deletion fail closed. The
deployment should therefore use one replica until shared route state is
implemented.

Build and publish immutable images through the deployment's own release process.

[← Back to README](../README.md)
