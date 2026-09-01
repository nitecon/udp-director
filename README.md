# UDP Director

[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-orange)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/docker/v/nitecon/udp-director?label=Docker%20Hub)](https://hub.docker.com/r/nitecon/udp-director)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue)](LICENSE)
[![Status](https://img.shields.io/badge/Status-Production%20Ready-green)]()

A Kubernetes-native, high-performance stateful UDP/TCP proxy for dynamic routing with token-based sessions and live migration support. Perfect for game server matchmaking, load balancing, and zero-downtime server switching.

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

UDP Director uses a query-based session establishment flow:

```
1. QUERY (TCP :9000)
   Client → Director: "Find me a game server with map=de_dust2"
   Director → K8s API: Query resources, find matching service
   Director → Client: Return token + establish session immediately
   Route: Client IP → Target mapping created

2. CONNECT (TCP/UDP :7777+)
   Client → Director: Connect and send data (no token needed)
   Director: Route based on existing session
   Director: Isolate each client UDP source port in its own upstream flow
   Client ↔ Target: All traffic proxied (TCP or UDP)

3. RESET (UDP :7777) - Optional
   Client → Director: Send control packet with new token
   Director: Update session to point to new target
   Client ↔ New Target: Traffic seamlessly redirected
```

**Key Features:**
- **True Layer 3 Load Balancing**: Sessions established via query port, not first packet inspection
- **Intelligent Load Balancing**: Least sessions or label-based arithmetic strategies
- **TCP & UDP Support**: Full support for both protocols on data ports
- **No Packet Loss**: All data packets forwarded immediately
- **Multi-Port Sessions**: Single query establishes access to all configured ports
- **Capacity-Aware Routing**: Prevent overloading backends with label-based load balancing

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
# Phase 1: Query for backend (session established automatically)
echo '{"resourceType":"gameserver","namespace":"starx","labelSelector":{"agones.dev/fleet":"m-tutorial"},"statusQuery":{"jsonPath":"status.state","expectedValue":"Ready"}}' | nc <LoadBalancer-IP> 9000
# Response: {"token":"550e8400-...","address":"10.244.1.44","ports":{"game-udp":7777,"game-tcp":7777}}
# Session is now established for your client IP:Port

# Phase 2: Connect and send data immediately (no token needed)
# UDP example
echo "GAME_DATA_PACKET" | nc -u <LoadBalancer-IP> 7777

# TCP example
nc <LoadBalancer-IP> 7777
# Start sending data immediately

# Phase 3: Reset to new server (optional)
# Send control packet: [MagicBytes][NewToken]
echo -n -e "\xFF\xFF\xFF\xFF\x52\x45\x53\x45\x54${NEW_TOKEN}" | nc -u <LoadBalancer-IP> 7777
```

**Note**: Session is established when you query, not when you send the first packet. This means:
- No need to send token as first packet
- All data packets are forwarded immediately
- Works with standard TCP/UDP clients

See [Technical Reference](Docs/TechnicalReference.md) and [Testing Guide](Docs/Testing.md) for complete examples.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Kubernetes Cluster                        │
│                                                               │
│  ┌────────────────────────────────────────────────────┐     │
│  │              UDP Director Pod                       │     │
│  │                                                      │     │
│  │  ┌─────────────┐         ┌──────────────────┐     │     │
│  │  │Query Server │         │   Data Proxy     │     │     │
│  │  │  (TCP:9000) │         │   (UDP:7777)     │     │     │
│  │  └──────┬──────┘         └────────┬─────────┘     │     │
│  │         │                          │                │     │
│  │  ┌──────▼────────┐      ┌─────────▼─────────┐     │     │
│  │  │ Token Cache   │◄─────┤ Session Manager   │     │     │
│  │  │  (TTL: 30s)   │      │ (Timeout: 300s)   │     │     │
│  │  └───────────────┘      └───────────────────┘     │     │
│  │         │                                           │     │
│  │  ┌──────▼─────────────────────────────────┐       │     │
│  │  │    Kubernetes API Client                │       │     │
│  │  └─────────────────────────────────────────┘       │     │
│  └────────────────────────────────────────────────────┘     │
│                                                               │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                  │
│  │GameSvr 1 │  │GameSvr 2 │  │GameSvr N │                  │
│  └──────────┘  └──────────┘  └──────────┘                  │
└─────────────────────────────────────────────────────────────┘
         ▲
         │ UDP/TCP Traffic
         │
   External Client
```

## Configuration

Choose the appropriate ConfigMap for your use case:

- **`k8s/configmap-pods-multiport.yaml`** - Multi-port pod routing (recommended) - Single token for multiple ports
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
tokenTTLSeconds: 30                # Token validity
sessionTimeoutSeconds: 300         # Session timeout
controlPacketMagicBytes: "FFFFFFFF5245534554"  # Control packet ID

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
