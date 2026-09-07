# Testing Guide for UDP Director

Verification focuses on TCP query, Kubernetes label matching, and UDP forwarding.
See [Query API](QueryAPI.md) for the request contract and unresolved integration
boundaries.

## Local checks

```bash
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

The `tcp_label_query_then_unmodified_udp_reaches_selected_pod` test uses real
local TCP and UDP sockets with a mocked Kubernetes Pod-list response. It checks
label selection, the `ready` response, forwarding of unmodified gameplay bytes,
and the backend reply. It does not prove live Kubernetes deployment or controller
occupancy behavior.

Character tests cover valid label names, the three character states, requested
ID filtering, and the 120-second disconnect visibility boundary. Query tests
cover fragmented character lists larger than one TCP read, incomplete JSON, and
rejection of removed allocation and session-reset requests.

## Manual query and connection

Use a reachable director and a Ready backend Pod with a real UDP application.
Configure its named container port and labels in the resource mapping; merely
declaring a UDP container port does not start a UDP listener.

From the gameplay client's host, query the director:

```bash
printf '%s\n' '{"type":"query","resourceType":"pod","namespace":"game-servers","labelSelector":{"map":"tutorial"}}' | nc <DIRECTOR_IP> 9000
```

Expect a response such as:

```json
{"status":"ready","server":"tutorial-0","ports":{"default":7777}}
```

Send normal application datagrams to that same director's returned UDP port and
verify the application reply. Do not send a token or reset packet first. Use an
actual UDP network path; TCP query port forwarding alone does not provide it.
The current IP-keyed route requires the query and gameplay to arrive from the
same source IP. See the API document's shared-NAT limitation.

The repository's example client demonstrates this sequence:

```bash
cargo run --example client_example
```

## Character lookup

Apply the metadata representation documented in [Query API](QueryAPI.md) to a
Ready Pod, then send:

```json
{"type":"characterList","namespace":"game-servers","characterIds":["friend-1"]}
```

Verify that only matching visible characters are returned. A disconnected
character is visible before 120 seconds and excluded at 120 seconds. This lookup
does not remove Pod labels or change occupancy; the owning controller handles
those mutations from actual game-server events.

## Troubleshooting

- No matching resource: check namespace, configured resource type, selectors,
  Pod readiness, Pod IP, and the named container port.
- No gameplay reply: check the `ready` response, director UDP exposure, backend
  listener, source IP, and forwarding logs.
- Missing character: check the label prefix, exact status spelling, requested ID,
  and valid disconnect timestamp within the reconnect window.

[Back to README](../README.md)
