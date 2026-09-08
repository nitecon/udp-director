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
response ports are public director ports.

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

## Current network limitation

The restored route associates TCP and UDP by the client's source IP. They must
reach the director with the same source IP. Different UDP source ports preserve
separate reply sockets, but clients behind one public IP cannot independently
select different servers. This release does not change that network limitation.
