# Unisrv CLI

A Rust-based command-line interface for provisioning and managing Unisrv resources.
A nice wrapper around the [Unisrv REST API](https://api.unisrv.io/swagger-ui/).

## Features

- **Instance Management**: Create, stop, list, and monitor VM instances with container images
- **Service Management**: Manage HTTP services with load balancing, path-based routing, and target configuration
- **Network Management**: Create and manage private networks for instance-to-instance communication
- **Secure Authentication**: Authenticatoin token management with automatic refresh, stored in system keyring
- **UUID Resolution**: Accept full UUIDs, UUID prefixes, or names for resource identification
- **Real-time Monitoring**: Stream logs from running instances via WebSocket
- **User-friendly Experience**: Progress spinners, colored output, and helpful error messages

## Command Structure

```
cli
├── instance (vm, instances) - Manage VM instances
│   ├── run - Create new instance with container image
│   ├── stop (rm) - Terminate instance
│   ├── list (ls) - List instances
│   └── logs (log) - Stream instance logs
├── service (srv, services) - Manage HTTP services with load balancing
│   ├── list (ls) - List services
│   ├── show (get) - Get service details
│   ├── delete (rm) - Delete service
│   ├── new - Create HTTP service
│   ├── target - Manage service targets (add/delete instance targets)
│   └── location (loc) - Manage service locations (routing rules)
│       ├── list (ls) - List locations (default when no subcommand)
│       ├── add - Add/update location routing rule
│       └── delete (rm) - Delete location
├── network (net, networks) - Manage private networks
│   ├── new - Create network with CIDR
│   ├── show (get) - Get network details
│   ├── delete (rm) - Delete network
│   └── list (ls) - List networks
├── login - Authenticate with username/password
└── auth - Authentication utilities
    └── token - Get current auth token
```

## Build and Run

```bash
# Build the project
cargo build --release

# Run the CLI
./target/release/unisrv --help

# During development
cargo run -- --help
```

## Development Commands

```bash
# Check code
cargo check

# Run tests
cargo test

# Format code
cargo fmt

# Lint code
cargo clippy
```

## Authentication

Authentication sessions are stored in the platform's keyring storage for security.
To log in, use the following command:

```bash
unisrv login --username <username>
```

## Configuration

- **API Host**: Configure via `API_HOST` environment variable
  - Debug builds default to `http://localhost:8080`
  - Release builds default to `https://api.unisrv.io`
- **Logging**: Set `RUST_LOG=debug` for detailed debug output
