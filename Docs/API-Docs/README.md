# API documentation

API contracts live in this directory as repository Markdown. Read and update
these files directly; they do not require agent-tools API artifacts or
gateway-backed docs lookup or publishing.

- [TCP Query and Connection API](QueryAPI.md): client requests and responses,
  TCP framing and errors, UDP routing, Pod assignment labels, occupancy and
  disconnect retention, capacity-demand HTTP callbacks, readiness and draining,
  and the current shared-NAT limitation.
- [Metrics](../Metrics.md): related Prometheus and health HTTP endpoints.

Before API work, read the relevant contract and the
[architecture law](../../AGENTS.md). Update the contract alongside behavior
changes and keep examples and links current. The architecture law takes
precedence; documented implementation limitations do not authorize architectural
changes.

## Migration inventory

On 2026-10-13, the gateway docs list and hierarchy for `scope=all` exposed one
document for this project. Its full content matched both the local publishing
artifact and the existing Markdown exactly; no additional populated content
fields required separate files.

| Previous source | Repository destination |
| --- | --- |
| `Docs/QueryAPI.md` | [QueryAPI.md](QueryAPI.md) |
| `.agent/api/udp-director.yaml` (removed duplicate publishing artifact) | [QueryAPI.md](QueryAPI.md) |
| Gateway document `01a07e39-c8f5-7060-abeb-6736e375aa45`, “UDP Director TCP Query and Connection API”, version `3.1.2` | [QueryAPI.md](QueryAPI.md) |

The gateway entry used slug `tcp-query-and-connection`, kind `agent_context`,
and labels `routing`, `tcp-query`, and `capacity-demand`. The full contract was
preserved unchanged, including its network limitation.

[Back to README](../../README.md)
