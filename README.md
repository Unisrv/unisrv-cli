# Cloud CLI

A Rust-based CLI for provisioning and managing Unisrv resources.

## Build and run

```bash
cargo build --release
./target/release/cli --help
```

## Authentication

Authentication sessions are stored in the platforms keyring storage.
To log in, use the following command:

```bash
cli login --username <username>
```

