[← Back to README](../README.md)

# Changelog

All notable changes to the UDP Director project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] - 2026-09-07

Internal and product-specific dependencies are being removed from this public
open-source project. The routing ticket cache, reservation-controller interface,
and UDP token/reset exchange are removed in favor of TCP query followed by UDP
forwarding. Character-list lookup reads native Pod labels and disconnect
metadata. See [Query API](QueryAPI.md) for the current recovery contract and its
remaining integration decisions.

Older release entries below describe historical behavior, not the current API.

## [0.2.1] - 2025-11-03

### Added - Annotation Selector Support

#### Core Features
- **Annotation Filtering** (`src/k8s_client.rs`)
  - Added `annotationSelector` support to query system
  - Client-side filtering for dynamic operational data
  - Follows Kubernetes best practices: labels for static metadata, annotations for dynamic data
  - Exact match filtering on annotation key-value pairs

- **Configuration** (`src/config.rs`)
  - Added `annotation_selector` field to `DefaultEndpoint`
  - Optional HashMap for annotation-based filtering
  - Fully backward compatible with existing configurations

- **Query API** (`src/query_server.rs`)
  - Extended `QueryRequest::Query` with `annotationSelector` parameter
  - Supports filtering by both labels and annotations simultaneously
  - Client-side filtering applied after label and status filtering

#### Documentation
- **New Guide**: `Docs/AnnotationSupport.md`
  - Complete guide on label vs annotation usage
  - Best practices for Kubernetes metadata
  - Use cases and examples for game server matchmaking
  - Performance considerations

- **Updated ConfigMaps**:
  - `k8s/configmap-agones-gameserver.yaml` - Added annotation examples
  - `k8s/configmap-pods-multiport.yaml` - Added annotation examples
  - `k8s/configmap-pods.yaml` - Added annotation examples
  - `k8s/configmap-agones-service.yaml` - Added annotation examples
  - **New**: `k8s/configmap-advanced-annotations.yaml` - Advanced filtering with load balancing

- **Updated Technical Reference**:
  - Added "Label and Annotation Filtering" section
  - Documented filtering order and best practices
  - Updated ConfigMap selection guide

#### Testing
- Added `test_annotation_selector_matching()` unit test
- Tests exact match, multiple annotations, mismatches, and missing annotations
- All 25 tests passing

#### Use Cases
- **Game Server Matchmaking**: Filter by static config (labels) and dynamic state (annotations)
- **Capacity-Based Routing**: Use annotations for current player counts
- **Dynamic Selection**: Filter servers by real-time operational data

### Changed
- Query system now supports three filtering mechanisms: labels, status, and annotations
- Filtering order: labels (server-side) → status (JSONPath) → annotations (client-side)

## [0.1.0] - 2025-11-01

### Added - Initial Implementation

#### Core Components
- **Configuration Module** (`src/config.rs`)
  - YAML-based configuration with validation
  - Support for multiple resource query mappings
  - ConfigMap watching infrastructure (placeholder)
  - Environment variable overrides

- **Token Cache** (`src/token_cache.rs`)
  - TTL-based token storage using moka cache
  - UUID v4 token generation
  - Thread-safe concurrent access
  - Automatic expiration handling

- **Session Manager** (`src/session.rs`)
  - DashMap-based session state storage
  - Automatic timeout cleanup (background task)
  - Session upsert for live migration
  - Activity tracking and touch mechanism

- **Kubernetes Client** (`src/k8s_client.rs`)
  - In-cluster authentication support
  - Dynamic resource querying with GVR
  - JSONPath-based status filtering
  - Service discovery and resolution
  - Label selector support

- **Query Server** (`src/query_server.rs`)
  - TCP server on configurable port (default: 9000)
  - JSON request/response protocol
  - Resource matching with labels and status queries
  - Token generation and caching
  - Error handling and validation

- **Data Proxy** (`src/proxy.rs`)
  - UDP server on configurable port (default: 7777)
  - Magic byte control packet detection
  - Session establishment with token validation
  - Session reset via control packets
  - Default endpoint fallback
  - Client-to-target packet proxying

- **Main Application** (`src/main.rs`)
  - Tokio async runtime initialization
  - Component orchestration and lifecycle management
  - Structured logging with tracing
  - Graceful error handling

#### Kubernetes Resources
- **RBAC** (`k8s/rbac.yaml`)
  - Namespace creation
  - ServiceAccount for pod identity
  - ClusterRole with minimal permissions
  - ClusterRoleBinding

- **ConfigMaps** (`k8s/configmap-*.yaml`)
  - `configmap-games.yaml` - Agones GameServer and Fleet routing examples
  - `configmap-dns.yaml` - DNS service routing configuration (port 53)
  - `configmap-ntp.yaml` - NTP service routing configuration (port 123)
  - Each with focused, use-case-specific examples
  - Fully documented parameters

- **Deployment** (`k8s/deployment.yaml`)
  - Single replica deployment
  - Resource limits and requests
  - Liveness and readiness probes
  - ConfigMap volume mount
  - Environment variable configuration

- **Service** (`k8s/service.yaml`)
  - LoadBalancer type for external access
  - TCP port 9000 (query endpoint)
  - UDP port 7777 (data proxy)

#### Build & Development
- **Dockerfile**
  - Multi-stage build for minimal image size
  - Debian slim runtime base
  - Non-root user execution
  - Optimized layer caching

- **Cargo Configuration** (`Cargo.toml`)
  - Rust 2024 Edition
  - All required dependencies with versions
  - Release profile optimization (LTO, opt-level 3)
  - Example binary configuration

- **Clippy Configuration** (`clippy.toml`)
  - Pedantic lints enabled
  - Reasonable exceptions documented
  - CI-ready configuration

- **Makefile**
  - Build, test, format, lint targets
  - Docker build helper
  - Kubernetes deployment shortcuts

- **CI/CD** (`.github/workflows/ci.yml`)
  - Format checking with rustfmt
  - Linting with clippy
  - Unit test execution
  - Release build verification
  - Docker image build

#### Documentation
- **Technical Reference** (`Docs/TechnicalReference.md`)
  - Complete architecture documentation
  - Configuration specification
  - Deployment instructions
  - Control packet protocol details
  - Session management internals
  - Kubernetes API integration
  - Performance tuning and security

- **Coding Guidelines** (`Docs/CodingGuidelines.md`)
  - Rust best practices
  - Project structure conventions
  - Error handling patterns
  - Testing requirements
  - Documentation standards

- **README** (`README.md`)
  - Project overview and features
  - Quick start guide
  - Build instructions
  - Documentation links

- **Testing Guide** (`Docs/Testing.md`)
  - Unit test instructions
  - Local development setup
  - Integration testing procedures
  - Load testing examples
  - Debugging commands

- **Project Summary** (`Docs/ProjectSummary.md`)
  - Implementation status checklist
  - Architecture summary
  - Key implementation details
  - Compliance with PRD
  - Next steps for deployment

- **Quick Reference** (`Docs/QuickReference.md`)
  - Common commands
  - API reference
  - Configuration quick reference
  - Debug commands
  - Troubleshooting checklist

#### Examples & Tools
- **Client Example** (`examples/client_example.rs`)
  - Complete three-phase flow demonstration
  - Query, connect, and reset examples
  - Control packet construction
  - Error handling patterns

- **Example Configuration** (`config.example.yaml`)
  - Local development configuration
  - Multiple resource type examples
  - Fully commented for clarity

- **Docker Ignore** (`.dockerignore`)
  - Optimized for build context size

- **Git Ignore** (`.gitignore`)
  - Rust-specific patterns
  - IDE and OS files
  - Local configuration files

#### Testing
- Unit tests for all core modules:
  - Token cache TTL and invalidation
  - Session timeout and cleanup
  - Configuration parsing and validation
  - JSONPath extraction
  - Control packet detection
  - Query request/response serialization

### Technical Details

#### Dependencies
- **Runtime**: tokio 1.42 (async runtime)
- **Kubernetes**: kube 0.97, k8s-openapi 0.24
- **Serialization**: serde 1.0, serde_json 1.0, serde_yaml 0.9
- **Caching**: moka 0.12 (TTL cache)
- **Concurrency**: dashmap 6.1 (concurrent HashMap)
- **Error Handling**: anyhow 1.0, thiserror 2.0
- **Utilities**: uuid 1.11, hex 0.4, jsonpath-rust 0.7
- **Logging**: tracing 0.1, tracing-subscriber 0.3

#### Performance Optimizations
- Release profile with LTO and opt-level 3
- Lock-free data structures (moka, DashMap)
- Zero-copy packet forwarding where possible
- Async I/O throughout the stack

#### Compliance
- ✅ Rust 2024 Edition
- ✅ Clippy pedantic lints passing
- ✅ Comprehensive rustdoc comments
- ✅ Unit test coverage for core logic
- ✅ PRD requirements R-3.1.1 through R-8.3 (with noted exceptions)

### Known Limitations

1. **Bi-directional Proxying**: Currently implements client→target direction only. Target→client requires dedicated sockets per session.
2. **TCP Data Port**: Implementation is UDP-only; PRD mentions TCP/UDP support.
3. **ConfigMap Hot Reload**: Placeholder implementation; full watch API integration needed.
4. **Load Balancing**: Always selects first matching resource; no round-robin or weighted selection.
5. **Metrics**: Prometheus metrics not yet implemented.

### Security Considerations

- Tokens are UUIDv4 with 30-second TTL
- No client authentication (relies on network policies)
- RBAC follows principle of least privilege
- Non-root container execution
- No unsafe code blocks

### Future Roadmap

- [ ] Full bi-directional UDP proxying
- [ ] TCP support on data port
- [ ] ConfigMap hot-reload via K8s watch API
- [ ] Load balancing strategies (round-robin, least-connections)
- [ ] Prometheus metrics and health endpoints
- [ ] Session persistence and recovery
- [ ] Rate limiting and DDoS protection
- [ ] mTLS support for control plane
- [ ] Horizontal pod autoscaling support
- [ ] Multi-region support with geo-routing

---

## Version History

### [0.1.0] - 2025-11-01
- Initial implementation based on PRD v1.3
- Complete three-phase flow (Query, Connect, Reset)
- Kubernetes-native with full RBAC
- Comprehensive documentation and examples
- Production-ready for Cilium environments

---

## Contributing

Please read `Docs/CodingGuidelines.md` before contributing. All changes should:
- Follow Rust 2024 Edition standards
- Pass `cargo fmt` and `cargo clippy -- -D warnings`
- Include unit tests for new functionality
- Update relevant documentation
- Add changelog entry

## License

[Add license information]

[← Back to README](../README.md)
