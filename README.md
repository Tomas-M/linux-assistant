# Linux Assistant

A native assistant client for Linux and Windows. Use a browser interface or a fullscreen terminal interface to ask questions and perform tasks on your computer. Local commands and file operations run on your computer with your permission.

## Build on Linux

Install Rust through rustup and your distribution's C build tools (for example, `build-essential` on Debian).

```sh
git clone https://github.com/Tomas-M/linux-assistant.git
cd linux-assistant
./scripts/build_client.sh
```

The result is `target/release/linux-assistant`, a single executable linked to the system's glibc. Build it on the oldest Linux distribution you intend to support. The pinned Rust toolchain is selected automatically by rustup.

## Run

Specify the backend provided by your administrator:

```sh
./target/release/linux-assistant --backend ai.slax.org
```

The client starts its local server and prints a localhost URL. Open that URL in your preferred browser. Launching it again reuses the running server. The browser interface and branding are supplied by the backend.

For the interactive terminal interface:

```sh
./target/release/linux-assistant --backend ai.slax.org --cli
```

You can set the default backend in `/etc/linux-assistant/config`:

```toml
backend = "ai.slax.org"
```

The command-line `--backend` option overrides that value. Hostnames default to HTTPS. HTTP is permitted only for loopback development servers.

## Build on Windows

Install Rust through rustup and Visual Studio C++ Build Tools, then run:

```powershell
cargo build --release --locked -p linux-assistant --bin linux-assistant
.\target\release\linux-assistant.exe --backend ai.slax.org
```

The Windows executable includes the MSVC runtime. The optional configuration file is `%ProgramData%\linux-assistant\config`.

## Source updates

This repository contains the client and its shared application protocol. It builds independently and does not include a backend implementation. Client updates are published here as complete source snapshots; changes should be contributed through the application's maintainer.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for bundled icon attribution. Rust dependencies retain their respective licenses.
