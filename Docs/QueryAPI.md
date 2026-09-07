# TCP Query and Connection API

Published contract for v2.0.0. This release restores query and forwarding and
adds read-only character lookup. Initial character-assignment integration and
shared-NAT connection identity remain unresolved; the limitations below apply.

## Transport

Connect to the regional director's TCP query port (default 9000). Send one UTF-8
JSON document, optionally followed by a newline. Request fields use camelCase.
The director reads across TCP fragments, returns one JSON response, and closes
the connection. Requests may be up to 65536 bytes.

## Server query

```json
{"type":"query","resourceType":"pod","namespace":"game-servers","labelSelector":{"map":"tutorial"}}
```

`resourceType` names a configured resource mapping. `namespace` is required.
Optional `labelSelector` and `annotationSelector` are exact-match string maps.
Optional `statusQuery` contains `jsonPath` and `expectedValues` (an array).
Core Pod queries additionally require a Running, Ready, nonterminating Pod with
a Pod IP. No match returns an error and installs no new route.

```json
{"status":"ready","server":"tutorial-0","ports":{"default":7777}}
```

`ready` means the forwarding route was installed, not that the player connected.
`ports` contains director data ports, not private backend ports. Send gameplay
directly to the same regional director after this response. No token or UDP setup
exchange is used. A subsequent successful query changes the forwarding target.

## Character-list lookup

```json
{"type":"characterList","namespace":"game-servers","characterIds":["friend-1","friend-2"],"labelSelector":{"map":"tutorial"}}
```

This read-only lookup returns Ready Pods containing any requested character.
An empty `characterIds` array lists all visible characters in matching Pods.
`labelSelector` is optional. Lookup does not install or change a route.

```json
{"servers":[{"namespace":"game-servers","name":"tutorial-0","characters":[{"characterId":"friend-1","status":"Used"}]}]}
```

Results are sorted by Pod name and character ID. No matches returns
`{"servers":[]}`. To query a friend's current server using existing label
selection, include its returned character label and status in `labelSelector`.

## Pod character metadata

Configure `characterLabelPrefix` (default `characters.udp-director.io`). Each
character uses one label, for example:

```yaml
metadata:
  labels:
    characters.udp-director.io/character-1: Allocated
    characters.udp-director.io/character-2: Used
    characters.udp-director.io/character-3: Disconnected
  annotations:
    udp-director.io/disconnected-at: '{"character-3":"2026-09-07T12:00:00Z"}'
```

Character IDs must fit a Kubernetes label name: 1–63 ASCII characters, start and
end with an alphanumeric character, and otherwise contain alphanumerics, `-`,
`_`, or `.`. The disconnect annotation key is configurable with
`disconnectAnnotation` (default `udp-director.io/disconnected-at`). Its value is
a JSON object mapping character IDs to RFC3339 timestamps.

- `Allocated`: query/initial connection requested; not counted as a connected player.
- `Used`: the game server reported that the player actually connected.
- `Disconnected`: record disconnect time and retain the label for reconnect for
  up to 120 seconds. At 120 seconds it is no longer visible to lookup; the
  controller removes the expired label and timestamp.

A missing or invalid disconnect timestamp makes that disconnected character
ineligible for lookup. A new actual connection restores `Used` and clears its
disconnect timestamp. Query activity and proxy timeouts do not update occupancy.
This director currently reads this metadata; controller mutation is separate.

## Current connection identity

The restored original query route is keyed by the TCP peer IP. Its UDP source
port may differ. Upstream UDP sockets are isolated by client source port, but
two players behind one public IP cannot select different targets with this
IP-keyed query contract. This limitation requires an explicit connection identity
decision; it must not be hidden behind a replacement ticket mechanism.

## Errors and removed requests

Errors use `{"error":"message"}`. `allocation` and `sessionReset` request types
are rejected. There is no routing-token cache, consume callback, reservation
admin API, or special UDP control datagram. All UDP payload bytes are gameplay.
