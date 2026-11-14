#!/bin/sh
# Build the standalone client for the current Linux system.
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'Run this build script on Linux. See README.md for Windows builds.' >&2
    exit 1
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --locked -p linux-assistant --bin linux-assistant
printf '%s\n' 'Built target/release/linux-assistant.'
