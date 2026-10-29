#!/bin/sh
# Build the Linux client for the host system from a Git checkout.
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
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --locked -p linux-assistant --bin linux-assistant
printf '%s\n' 'Built target/release/linux-assistant.'
