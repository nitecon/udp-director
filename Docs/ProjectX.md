[← Back to README](../README.md)

# Project X exact-Pod routing

Project X uses controller-issued, single-use allocation reservations instead of
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

Set `projectXAllocationOnly: true` in the mounted configuration to reject
legacy resource queries, token resets, and first-packet fallback routing. This
is required for Project X deployments.

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
Project X deployment therefore uses one replica until shared route state is
implemented.

The regional image is published as
`us-east4-docker.pkg.dev/nitecon-datacenter/starx/udp-director:<git-hash>`.
Environment promotion assigns aliases outside this repository without changing
the immutable image.

[← Back to README](../README.md)
