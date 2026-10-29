#!/bin/sh
# Build the source bundle on Linux using its exact Codex revision and dependency lockfile.
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'Run this build script on your Linux server.' >&2
    exit 1
fi
revision=$(cat CODEX_REVISION)
if [ ! -d vendor/codex/.git ]; then
    git init vendor/codex
fi
git -C vendor/codex fetch --depth 1 https://github.com/openai/codex.git "$revision"
git -C vendor/codex checkout --detach "$revision"
test "$(git -C vendor/codex rev-parse HEAD)" = "$revision"

# Limit parallel compilation by default; the embedded Codex build is substantial.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --locked -p assistant-server --bin assistant-server
./target/release/assistant-server --codex-version
printf '%s\n' 'Built target/release/assistant-server. Continue with START-HERE.md to install and configure it.'
