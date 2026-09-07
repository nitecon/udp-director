# UDP Director

[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-orange)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/docker/v/nitecon/udp-director?label=Docker%20Hub)](https://hub.docker.com/r/nitecon/udp-director)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue)](LICENSE)
[![Status](https://img.shields.io/badge/Status-Production%20Ready-green)]()

A Kubernetes-native UDP/TCP proxy. Query a server over TCP using Pod labels, then connect through the same regional director.

## Quick Links

- **[Load Balancing](Docs/load-balancing.md)** - Load balancing strategies and configuration
- **[Annotation Support](Docs/AnnotationSupport.md)** - Filter by labels and annotations (best practices)
- **[Technical Reference](Docs/TechnicalReference.md)** - Complete deployment and technical guide
- **[Multi-Port Support](Docs/MultiPortSupport.md)** - Multi-port configuration guide
- **[Pod Routing Guide](Docs/PodRoutingGuide.md)** - Pod-based routing configuration
- **[Metrics Documentation](Docs/Metrics.md)** - Prometheus metrics and monitoring
- **[Coding Guidelines](Docs/CodingGuidelines.md)** - Standards for contributors
- **[Testing Guide](Docs/Testing.md)** - Unit, integration, and load testing
- **[Quick Reference](Docs/QuickReference.md)** - Commands and API reference
- **[Project Summary](Docs/ProjectSummary.md)** - Implementation status
- **[Query and Connection API](Docs/QueryAPI.md)** - TCP queries, character lookup, and UDP routing
- **[Changelog](Docs/Changelog.md)** - Version history

## What Problem Does This Solve?

Traditional UDP load balancers are stateless and can't intelligently route clients based on Kubernetes resource state (like Agones GameServers). UDP Director solves this by:

- **Querying Kubernetes resources** to find available game servers based on labels, status, and capacity
- **Establishing stateful sessions** so clients maintain connections to specific backends
- **Enabling live migration** where clients can seamlessly switch servers without reconnecting
- **Integrating with K8s** to automatically discover services and route traffic

**Use Cases:**
- Game server matchmaking (Agones, custom CRDs)
- Dynamic UDP load balancing based on resource state
- Zero-downtime server migration for players
- Multi-tenant UDP routing

## How It Works

1. Send a TCP query to the regional director on port 9000 with Kubernetes label selectors.
2. The director selects a matching Ready Pod and establishes the forwarding route.
3. Wait for the query response, then send gameplay directly over UDP through the same director.
4. Use TCP character-list queries to find Pods containing friends. Character status lives on Pod labels; actual server connection events drive occupancy.

No routing ticket, token redemption, or UDP setup exchange is required.
See [Query and Connection API](Docs/QueryAPI.md) for the wire format and routing constraints.

## Quick Start

### Prerequisites

- Kubernetes cluster with Cilium CNI
- `kubectl` configured

### Docker Images

Docker images are automatically built and published to Docker Hub via GitHub Actions:

- **Docker Hub**: https://hub.docker.com/r/nitecon/udp-director
- **Latest**: `nitecon/udp-director:latest` (updated on every push to `main`)
- **Tagged Releases**: `nitecon/udp-director:v1.0.0` (created when version tags are pushed)

```bash
# Pull the latest image
docker pull nitecon/udp-director:latest

# Pull a specific version
docker pull nitecon/udp-director:v1.0.0
```

### Deploy to Kubernetes

```bash
# Clone repository for K8s manifests
git clone <repository-url>
cd udp-director

# Deploy
kubectl apply -f k8s/rbac.yaml
# Choose the appropriate configmap for your use case:
kubectl apply -f k8s/configmap-pods-multiport.yaml     # For multi-port pod routing (recommended)
# OR kubectl apply -f k8s/configmap-pods.yaml            # For single-port pod routing
# OR kubectl apply -f k8s/configmap-agones-gameserver.yaml  # For Agones GameServers
kubectl apply -f k8s/deployment.yaml

# Verify
kubectl get pods -n udp-director
kubectl logs -n udp-director -l app=udp-director -f
```

### Using Specific Versions

```bash
# Use a specific version tag
kubectl set image deployment/udp-director \
  udp-director=nitecon/udp-director:v1.0.0 \
  -n udp-director

# Or edit deployment.yaml before applying
# image: nitecon/udp-director:v1.0.0
```

### Client Integration Example

```bash
printf '%s\n' '{"type":"query","resourceType":"pod","namespace":"game-servers","labelSelector":{"map":"tutorial"}}' | nc <regional-director> 9000
# {"status":"ready","server":"tutorial-0","ports":{"default":7777}}

# After the query succeeds, send gameplay using the same director endpoint.
printf 'gameplay' | nc -u <regional-director> 7777
```

See [Query and Connection API](Docs/QueryAPI.md) and the [Rust client example](examples/client_example.rs).

## Architecture

```text
Client -- TCP query --> Director -- label selection --> Kubernetes Pods
Client <-- route ready -- Director
Client <====== UDP through Director ======> Selected Pod
Server -- actual connect/disconnect --> Capacity controller -- Pod character labels
```

## Configuration

Choose the appropriate ConfigMap for your use case:

- **`k8s/configmap-pods-multiport.yaml`** - Multi-port pod routing (recommended) - One query for multiple ports
- **`k8s/configmap-pods.yaml`** - Single-port pod routing - For standard Kubernetes pods
- **`k8s/configmap-agones-gameserver.yaml`** - For Agones GameServers (direct resource inspection)
- **`k8s/configmap-agones-service.yaml`** - For Agones GameServers (service-based routing, legacy)

Each ConfigMap includes:
```yaml
queryPort: 9000                    # TCP query endpoint
dataPorts:                         # Multiple data ports (multi-port config)
  - port: 7777
    protocol: "udp"
    name: "game-udp"
sessionTimeoutSeconds: 300         # Session timeout

# Load balancing (optional)
loadBalancing:
  type: "leastSessions"            # or "labelArithmetic"
  # For labelArithmetic:
  # currentLabel: "currentUsers"
  # maxLabel: "maxUsers"
  # overlap: 2

resourceQueryMapping:
  # Resource-specific mappings (see individual files)
```

See:
- [Multi-Port Support](Docs/MultiPortSupport.md) for multi-port configuration
- [Load Balancing](Docs/load-balancing.md) for load balancing strategies

Edit the chosen ConfigMap to customize for your environment.

## Performance

- **Query Latency**: < 10ms (+ K8s API latency)
- **Proxy Overhead**: < 1ms per packet
- **Throughput**: > 10,000 packets/second
- **Concurrent Sessions**: > 1,000 per instance
- **Memory**: ~128MB + ~1KB per session

## Development

### Local Development

```bash
# Format and lint
cargo fmt
cargo clippy -- -D warnings

# Test
cargo test

# Build
cargo build --release

# Or use make
make help
```

## Technology Stack

- **Language**: Rust 2024 Edition
- **Runtime**: Tokio (async I/O)
- **K8s Client**: kube-rs
- **Caching**: moka (TTL cache)
- **Concurrency**: DashMap
- **Target**: Cilium Service Mesh on Kubernetes

## Contributing

1. Follow [Coding Guidelines](Docs/CodingGuidelines.md)
2. Ensure `cargo fmt` and `cargo clippy` pass
3. Add tests for new functionality
4. Update documentation in `Docs/`
5. Add changelog entry

## License

This project is licensed under the Apache 2.0 License - see the [LICENSE](LICENSE) file for details.
