#!/bin/sh
# Build the standalone x86-64 Linux client from a Git checkout.
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'Run this build script on Linux. See README.md for Windows builds.' >&2
    exit 1
fi

# Cargo reads every workspace manifest, including the backend's pinned source dependencies.
if [ ! -f vendor/codex/codex-rs/Cargo.toml ]; then
    git -c core.longpaths=true submodule update --init --depth 1 -- vendor/codex
fi
rustup target add x86_64-unknown-linux-musl
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --locked --target x86_64-unknown-linux-musl -p linux-assistant --bin linux-assistant
printf '%s\n' 'Built target/x86_64-unknown-linux-musl/release/linux-assistant.'
