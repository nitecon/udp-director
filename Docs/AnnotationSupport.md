# Operator Annotation Filtering

Annotation selectors are director configuration, never client request fields.
Clients use [map and character requests](QueryAPI.md).

Operators may set `defaultEndpoint.annotationSelector` to an exact-match string
map. The director applies it after Kubernetes label filtering. Keep workload
policy in deployment configuration rather than embedding it in client code.

```yaml
defaultEndpoint:
  resourceType: pod
  namespace: game-servers
  labelSelector:
    app: game-server
  annotationSelector:
    acceptingConnections: "true"
```

The deployment must maintain these annotations. Character occupancy uses the
separate per-character labels and disconnect timestamp contract in
[Query API](QueryAPI.md). No client supplies Kubernetes selectors.
