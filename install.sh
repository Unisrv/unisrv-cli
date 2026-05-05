#!/bin/bash

set -e

# CD to the script's directory
cd "$(dirname "$0")"

# If --debug is passed, build in debug mode
mkdir -p ~/.local/bin
if [[ "$1" == "--debug" ]]; then
    echo "Building in debug mode..."
    cargo build
    ls -h -l target/debug/unisrv
    cp target/debug/unisrv ~/.local/bin/unisrv
else
    echo "Building in release mode..."
    cargo build --release
    ls -h -l target/release/unisrv
    mkdir -p ~/.local/bin
    cp target/release/unisrv ~/.local/bin/unisrv
fi

chmod +x ~/.local/bin/unisrv
echo "unisrv installed to ~/.local/bin/unisrv."
echo "Add ~/.local/bin to your PATH to use it. export PATH=\$PATH:\$HOME/.local/bin"
