#!/bin/bash

set -e

# CD to the script's directory
cd "$(dirname "$0")"

# If --debug is passed, build in debug mode
mkdir -p ~/.local/bin
if [[ "$1" == "--debug" ]]; then
    echo "Building in debug mode..."
    cargo build
    ls -h -l target/debug/cli
    cp target/debug/cli ~/.local/bin/cli
else
    echo "Building in release mode..."
    cargo build --release
    ls -h -l target/release/cli
    mkdir -p ~/.local/bin
    cp target/release/cli ~/.local/bin/cli
fi

chmod +x ~/.local/bin/cli
echo "cli installed to ~/.local/bin/cli."
echo "Add ~/.local/bin to your PATH to use it. export PATH=\$PATH:\$HOME/.local/bin"
