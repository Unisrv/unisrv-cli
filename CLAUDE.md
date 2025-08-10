# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is **Unisrv CLI** - a Rust-based command-line interface for provisioning and managing cloud resources. The CLI provides functionality for managing instances (VMs), networks, and services through a REST API.

## Build and Development Commands

```bash
# Build the project
cargo build --release

# Run the CLI
./target/release/cli --help
# or during development:
cargo run -- --help

# Check code
cargo check

# Run tests (if any)
cargo test

# Format code
cargo fmt

# Lint code
cargo clippy
```

## Architecture

### Core Components

- **Main Entry Point** (`src/main.rs`): CLI argument parsing using `clap`, routing to subcommands
- **Configuration** (`src/config.rs`): Handles API host configuration, authentication sessions stored in system keyring
- **Authentication** (`src/auth/mod.rs`): JWT token management with automatic refresh, keyring storage
- **Command Modules**: Each major feature has its own module with subcommands

### Command Structure

The CLI uses a hierarchical command structure:

```
cli
├── instance (alias: vm, instances) - Manage VM instances
│   ├── run - Create new instance with container image
│   ├── stop (alias: rm) - Terminate instance
│   ├── list (alias: ls) - List instances
│   └── logs (alias: log) - Stream instance logs
├── service (alias: srv, services) - Manage load balancer services  
│   ├── list (alias: ls) - List services
│   ├── show (alias: get) - Get service details
│   ├── delete (alias: rm) - Delete service
│   ├── new tcp - Create TCP service
│   └── target - Manage service targets (add/delete)
├── network (alias: net, networks) - Manage private networks
│   ├── new - Create network with CIDR
│   ├── show (alias: get) - Get network details  
│   ├── delete (alias: rm) - Delete network
│   └── list (alias: ls) - List networks
├── login - Authenticate with username/password
└── auth - Authentication utilities
    └── token - Get current auth token
```

### Key Features

- **UUID Resolution**: All commands accept full UUIDs, UUID prefixes, or names (where applicable)
- **Authentication**: JWT tokens with automatic refresh, stored securely in system keyring
- **Environment Configuration**: API host configurable via `API_HOST` environment variable
- **Debugging**: Uses `env_logger` - set `RUST_LOG=debug` for detailed logs
- **User Experience**: Progress spinners, colored output, helpful error messages

### API Integration

- **Base URL**: Defaults to `http://localhost:8080` in debug builds, `https://api.unisrv.io` in release
- **Authentication**: Bearer token authentication with automatic token refresh
- **WebSocket**: Real-time log streaming for instances

### Error Handling

- Comprehensive error handling using `anyhow`
- User-friendly error messages with styling
- Automatic cleanup of expired authentication sessions
