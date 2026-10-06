# TCP Query and Connection API

Version 3.0.0 replaces the infrastructure-facing v2 request contract. Clients
send map and character IDs; all Kubernetes details stay inside the director.

## Connect to a server

Connect to the regional director's TCP query port (default 9000), send one UTF-8
JSON document, and read the response until the connection closes:

```json
{"type":"query","map":"tutorial","characterId":"my-character","friendIds":["friend-1","friend-2"]}
```

`map` and the joining `characterId` are required. `friendIds` is optional and
may be empty. Selection first preserves an existing visible assignment for the
joining character, otherwise prefers the available server containing the most
requested friends, then the fewest assigned characters. If no friend is present
or their server is full, another available server on the map is selected.

```json
{"status":"Allocated","server":"opaque-server-id","ports":{"default":7777}}
```

On success, send normal gameplay datagrams to the **same regional director** at
the returned data port. `server` is an opaque identity for grouping lookup
results; it is not an address or credential. No ticket, redemption, UDP setup,
or additional backend API request is needed.

`Allocated` means the character assignment and forwarding route are ready.
It does not mean the player has connected. An already `Used` character remains
`Used` when repeating a query. Actual server events drive occupancy.

## Find friends

```json
{"type":"characterList","map":"tutorial","characterIds":["friend-1","friend-2"]}
```

```json
{"servers":[{"server":"opaque-server-id","characters":[{"characterId":"friend-1","status":"Used"}]}]}
```

This lookup does not allocate or change a route. It returns matching characters
grouped by server; missing or empty `characterIds` lists all visible characters
on the map. No matches returns `{"servers":[]}`. To join the group, send a
`query` with those friends' character IDs. Availability may change between lookup
and connection. Server IDs and character IDs are sorted in lookup responses.

## Transport and errors

Use camelCase fields and one JSON document per TCP connection. A trailing
newline is optional. Fragmented requests are supported up to 65536 bytes.
Map and character IDs accept 1–63 ASCII characters: start/end alphanumeric,
with alphanumerics, `-`, `_`, and `.` inside. Unknown fields are rejected.

Errors have the form `{"error":"message"}`. No capacity returns
`{"error":"No server available"}`. A failed assignment does not install a new
route or report success. Kubernetes error details stay in director logs.

## Operator configuration — not client inputs

The director uses `defaultEndpoint` for namespace, base selectors, and resource
mapping. Access requires a core Pod mapping. `mapLabel` defaults to `map` and
maps the client's map value to the internal label. `maxCharactersPerServer`
defaults to 128. Available capacity counts visible `Allocated`, `Used`, and
unexpired `Disconnected` characters. Only Ready, Running, nonterminating Pods
with an IP are eligible. Backend ports come from the configured resource mapping;
response ports are public director ports. Pods labeled
`udp-director.io/draining: "true"` are excluded from access selection, including
repeated access queries. Character lookup can still show their occupants.

Each assignment uses one label under `characterLabelPrefix` (default
`characters.udp-director.io`): `<prefix>/<characterId>: Allocated`. The director
patches this label using the Pod resource version before installing forwarding;
a concurrent update fails the request instead of overwriting the newer state.
The service account requires Pod get/list/watch/patch permissions.

The occupancy controller changes the label to `Used` on actual connection and
`Disconnected` on disconnect. It stores the RFC3339 disconnect time in the JSON
ID-to-timestamp annotation `udp-director.io/disconnected-at` (configurable via
`disconnectAnnotation`). Disconnected entries stay visible for less than 120
seconds; the controller removes expired labels and timestamps. A new connection
restores `Used` and clears the timestamp. Proxy inactivity never changes occupancy.

## Capacity demand and cold starts

Capacity demand is available in director version 3.1.0 and later. Automatic map
pass-through and optional aliases require version 3.1.1 or later.

By default, an access query with no eligible server returns `No server available`.
Operators can enable aggregate demand signaling:

```yaml
capacityDemand:
  endpoint: http://capacity-controller.control.svc:8080/v1alpha1/demand
  bootTimeoutSeconds: 300
  backendGroups: {}  # Optional aliases; can also be omitted.
```

When no eligible capacity exists, the requested map ID is sent as `backendGroup`
by default. The controller's HTTP 404 is the authoritative unpublished/unknown
response, so newly published cold maps need no director allowlist or configuration
update. Optional `backendGroups` entries override individual map IDs with aliases:

```yaml
backendGroups:
  tutorial: example-backend
```

Both `backendGroups: {}` and omitting `backendGroups` enable default pass-through.
Aliases and the internal HTTP endpoint are director configuration, never client
fields. Map/group IDs use the same 1–63 character format as map IDs. The timeout
must be positive and defaults to 300 seconds. Capacity demand requires a core Pod
mapping.

Only a valid access `query` with no eligible capacity triggers the callback;
`characterList`, ready access, and UDP traffic do not trigger it. The director
sends one HTTP POST with `Content-Type: application/json`:

```json
{"backendGroup":"tutorial"}
```

The configured endpoint includes its path. No player identity, assignment,
credential, desired replica count, or callback response body is part of this
contract. The controller independently reconciles capacity through Kubernetes.
Its status code determines the next step:

| HTTP status | Director behavior |
| --- | --- |
| 202 | Demand accepted; wait for eligible Pods. This is not a startup acknowledgement. |
| 404 | Return `Unknown backend group`. |
| 503 | Return `Backend group is under maintenance`. |
| Other status or transport failure | Return `Capacity demand unavailable`. |

Concurrent cold queries for the same group share one POST and the startup
deadline **within one director process**. Each query keeps its normal independent
character selection and label assignment. Separate director replicas can each
send a POST; the controller must treat repeated aggregate signals idempotently.
There are no callback retries or durable request records. Once all waiting
queries finish or disconnect, a later query may signal the group again.

After 202, the director follows a native Pod watch from the resource version of
its previous list. On changes it re-reads matching Pods, applies configured
selectors and readiness/capacity rules, then performs normal friend-aware
selection and the resource-version-guarded `Allocated` label write. Watch expiry
or a Kubernetes 410 causes a fresh list/watch; other API errors fail the query.
No route is installed before successful selection and assignment. A 409
assignment conflict fails safely as `Unable to establish route`; it is not
retried. Startup timing includes the POST and subsequent watch/list work. Expiry
returns `Server startup timed out` without an assignment from the wait.

Clients must keep the TCP connection, including its write side, open while a
cold query waits. EOF or a read error cancels that query's cold wait. Cancelling
one query does not cancel other waiters. Cancelling the last waiter releases
local demand work; a POST already delivered to the controller cannot be recalled.
Assignment processing that has already begun completes normally. Client read
timeouts should allow the configured startup timeout plus response processing.

Director, controller, Kubernetes API, regional network endpoint, DNS, and the
worker capacity reconciler must remain reachable when backend workers are zero.
Place their required services on the always-on control capacity. Pod/node startup,
image pulls, published groups, warm capacity, and idle periods are controller or
cluster configuration. Enabling the director callback alone does not provide a
controller or configure a node pool.

## Draining and idle shutdown

The generic Pod label `udp-director.io/draining: "true"` excludes the Pod from
access assignment, including reconnect queries. It does not stop existing UDP
forwarding or hide its occupants from `characterList`. Removing the marker makes
the Pod eligible again if it meets the other readiness/capacity rules.

Native Pod Ready can precede application/server-process readiness. The controller
must create starting Pods with this drain marker already set and retain it until
verified application readiness, then clear it. The director watches that label
change and applies its normal Ready/capacity checks. Application readiness reports
and their transport remain controller-owned; the director requires no private
report protocol or dependencies. This startup gate uses the same generic marker
as idle draining.

The controller must mark a candidate draining using its current resource version,
then re-read the Pod's character labels and authoritative occupancy before
removing capacity. The director's assignment patch includes its listed resource
version: an intervening drain update makes that patch fail. Conversely, an
intervening assignment must make the controller's stale drain patch fail, so it
can re-evaluate the Pod. The controller must preserve pending `Allocated`, actual
`Used`, and retained `Disconnected` assignments, and guard its final deletion
against intervening changes. Do not delete based on an earlier empty snapshot.

Actual player connect/disconnect events remain the occupancy authority. Neither
this callback nor TCP queries, proxy sessions, packet counts, or proxy inactivity
prove a server is empty. The director does not emit an idle/empty signal; the
controller owns verified-empty policy and disconnect-retention cleanup.

## Current network limitation

The restored route associates TCP and UDP by the client's source IP. They must
reach the director with the same source IP. Different UDP source ports preserve
separate reply sockets, but clients behind one public IP cannot independently
select different servers. This release does not change that network limitation.
