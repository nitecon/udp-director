[← Back to README](../README.md)

# UDP Director - Quick Reference Card

## 🚀 Quick Commands

### Development
```bash
cargo fmt                          # Format code
cargo clippy -- -D warnings        # Lint code
cargo test                         # Run tests
cargo build --release              # Build release binary
cargo run --example client_example # Run example client
```

### Docker
```bash
docker build -t udp-director:latest .
docker run -v $(pwd)/config.yaml:/etc/udp-director/config.yaml udp-director:latest
```

### Kubernetes
```bash
# Deploy
make k8s-deploy
# or
kubectl apply -f k8s/rbac.yaml
# Choose the appropriate configmap:
kubectl apply -f k8s/configmap-games.yaml  # For game servers
# OR kubectl apply -f k8s/configmap-dns.yaml    # For DNS
# OR kubectl apply -f k8s/configmap-ntp.yaml    # For NTP
kubectl apply -f k8s/deployment.yaml

# Check status
kubectl get pods -n udp-director
kubectl get svc -n udp-director
kubectl logs -n udp-director -l app=udp-director -f

# Delete
make k8s-delete
```

---

## API and configuration

See [Query API](QueryAPI.md) for TCP request framing, `query`, `characterList`,
Pod character labels, and the current shared-NAT limitation. Use
[config.example.yaml](../config.example.yaml) for the minimal Pod configuration.

```json
{"type":"query","map":"tutorial","characterId":"me","friendIds":["friend-1"]}
```

```json
{"status":"Allocated","server":"opaque-server-id","ports":{"default":7777}}
```

After `Allocated`, send application datagrams directly to the director's returned
UDP port. Query success is not proof of actual player connection. There is no
routing token, UDP setup packet, or reset command.

## Testing

```bash
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

See [Testing](Testing.md) for the real-socket query/forwarding test and manual
verification with a running UDP backend.

## 🐛 Debug Commands

```bash
# View logs with debug level
kubectl set env deployment/udp-director -n udp-director RUST_LOG=udp_director=debug
kubectl logs -n udp-director -l app=udp-director -f

# Check connectivity
nc -zv <ip> 9000  # TCP query port
nc -zuv <ip> 7777 # UDP data port

# Verify RBAC
kubectl auth can-i list gameservers.agones.dev \
  --as=system:serviceaccount:udp-director:udp-director

# Check resources
kubectl get gameservers -n game-servers --show-labels
kubectl get svc -n game-servers

# Packet capture
kubectl exec -n udp-director <pod> -- tcpdump -i any -n port 7777
```

---

## 📊 Module Overview

| Module | Purpose | Key Types |
|--------|---------|-----------|
| `config.rs` | Configuration management | `Config`, `ResourceMapping` |
| `characters.rs` | Pod character metadata lookup | `Character`, `CharacterStatus` |
| `session.rs` | Session state management | `SessionManager`, `Session` |
| `k8s_client.rs` | Kubernetes API client | `K8sClient`, `StatusQuery` |
| `query_server.rs` | TCP query endpoint | `QueryServer`, `QueryRequest` |
| `proxy.rs` | UDP data proxy | `DataProxy` |
| `main.rs` | Application entry point | - |

---

## 🔐 RBAC Permissions Required

```yaml
- apiGroups: [""]
  resources: ["services", "configmaps"]
  verbs: ["get", "list", "watch"]

- apiGroups: ["agones.dev"]
  resources: ["gameservers"]
  verbs: ["get", "list", "watch"]
```

---

## 📦 Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| tokio | 1.42 | Async runtime |
| kube | 0.97 | Kubernetes client |
| moka | 0.12 | TTL cache |
| dashmap | 6.1 | Concurrent HashMap |
| serde_json | 1.0 | JSON serialization |
| tracing | 0.1 | Logging |

---

## Client example

See [client_example.rs](../examples/client_example.rs) for the TCP query and
subsequent UDP connection sequence. A successful new query changes the selected
forwarding target.

## 📈 Performance Targets

- **Query Latency**: < 10ms
- **Proxy Latency**: < 1ms
- **Throughput**: > 10k packets/sec
- **Sessions**: > 1k concurrent
- **Memory**: ~128MB + 1KB/session

---

## 🚨 Troubleshooting Checklist

- [ ] Rust toolchain installed (2024 edition)
- [ ] Kubernetes cluster accessible
- [ ] Cilium CNI installed
- [ ] RBAC resources applied
- [ ] ConfigMap deployed
- [ ] Backend resources exist with correct labels
- [ ] Services have matching selector labels
- [ ] LoadBalancer has external IP
- [ ] Firewall allows UDP 7777 and TCP 9000

---

## 📚 Documentation Links

- **Technical Reference**: `Docs/TechnicalReference.md`
- **Coding Guidelines**: `Docs/CodingGuidelines.md`
- **Testing Guide**: `Docs/Testing.md`
- **Project Summary**: `Docs/ProjectSummary.md`
- **Quick Reference**: `Docs/QuickReference.md`
- **Example Client**: `examples/client_example.rs`

[← Back to README](../README.md)

---

## 🎓 Learning Resources

### Rust Concepts Used
- Async/await with Tokio
- Error handling (Result, anyhow, thiserror)
- Ownership and borrowing
- Trait implementations
- Pattern matching
- Concurrent data structures

### Kubernetes Concepts
- Custom Resource Definitions (CRDs)
- Service discovery
- RBAC (ServiceAccount, Role, RoleBinding)
- ConfigMaps
- LoadBalancer Services

### Networking Concepts
- UDP stateful proxying
- Session management
- Control vs data plane separation
