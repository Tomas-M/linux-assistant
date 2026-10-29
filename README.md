# Linux Assistant

A Rust assistant for Linux and Windows with a terminal prompt and an embedded browser UI. Each platform has its own **single executable**; Linux uses static musl and Windows statically links the MSVC runtime. A separate Rust backend owns accounts, application credits, payments, and AI access. The browser UI has a light/dark conversation view, streaming progress, and a floating local process inspector.

## Run locally

Install Rust through rustup and the platform's C/C++ build tools. Initialize the pinned Codex source once (the long-path option is needed on Windows), then build:

```sh
git -c core.longpaths=true submodule update --init --depth 1
cargo build --workspace --locked
./target/debug/assistant-server --dev
```

In another terminal:

```sh
./target/debug/linux-assistant
# Prints a localhost URL; open it in your preferred browser.
# For an interactive terminal prompt use --cli.
# Or explicitly open browser mode:
./target/debug/linux-assistant --desktop
```

The default backend is `http://127.0.0.1:8787`. Development mode is **simulated AI** and **simulated email**, binds only to loopback, and rejects payments. The sign-in page displays a development verification code. Sign in with any test email, choose a character, and send a message. Enable commands and ask for `demo command` to test a harmless `printf` proposal on Linux or `Write-Output` on Windows.

For a deployed backend:

```sh
./linux-assistant --backend https://assistant.example.com
./linux-assistant --install-desktop
```

The optional Linux desktop launcher points to the executable's current location. Install the executable where it will remain before creating the launcher. Desktop/browser launch needs the system's normal default-browser opener (`xdg-open` or equivalent on Linux). There is no Node.js, Python, external web asset, or Codex runtime dependency on the **client**. Commands use the host's `/bin/sh` on Linux or built-in Windows PowerShell 5.1 on Windows, plus installed programs.

## Client behavior

- Launching without flags starts or reuses the background server, prints its complete browser URL, and exits. The fragment is a private access token: keep it in the URL when opening your chosen browser. `--cli` opens the interactive terminal prompt; `--desktop` explicitly opens the default browser.
- Both frontends attach to one background daemon on an ephemeral `127.0.0.1` port. Closing the browser or leaving the prompt keeps it running. Use `/stop` or the browser's stop button to stop it.
- State is stored under `$XDG_DATA_HOME/linux-assistant` (normally `~/.local/share/linux-assistant`) on Linux or `%LOCALAPPDATA%\linux-assistant` on Windows. `LINUX_ASSISTANT_HOME` overrides the directory for isolated tests. Session tokens, history, pending approvals, and unfinished request IDs are saved atomically. Linux files use private permissions; Windows files inherit the user directory's ACLs. `LINUX_ASSISTANT_BACKEND` supplies the initial backend before state exists.
- The browser receives a local capability token in a URL fragment, removes it from the address bar, and keeps it in tab-scoped session storage. API routes require that token plus the exact loopback Host and same Origin. There is no permissive CORS.
- Commands are disabled initially. Enabling them permits **proposals**, with approval of each exact command and directory. **Full access** authorizes automatic execution of validated proposals, including an already displayed proposal. Turning it off takes effect before the next command; stopping a process also turns it off. Full access resets when the daemon restarts. Approval IDs remain one-use, and disabling commands invalidates pending approvals.
- The client detects its operating system automatically and supplies it to the backend with every chat request. The AI chooses shell syntax and paths for that client, regardless of the backend's own OS. Both interfaces display the selected shell.
- Commands run as the current user, **not in a sandbox**, with no automatic elevation and no execution time limit. They run until completion, a Stop request, or application shutdown. Stop terminates the Linux process group or Windows Job Object, including ordinary descendants. Cleanup also runs when execution is cancelled or the shell exits. Capture retains up to 1 MiB per stdout/stderr stream, keeps draining after that limit, and marks truncation. Deliberately detached Linux sessions or work delegated to external services can outlive execution; this is not a containment boundary.
- The floating **Task manager** shows currently running commands in a table with command text, PID, elapsed time, and Stop. Stop kills that command and its child processes and turns off full access. Completed commands move to a separate history section (latest 50 records). Expand a row for PID, timestamps, directory, exact command, executable arguments, live stdout/stderr, exit code, and execution errors. You can save either captured stream as text. Process polling and stopping remain responsive during command execution and AI requests.
- Completed process records are saved in the private client state directory under `processes/<id>.json`; older records remain on disk after leaving the panel. After a restart, unfinished records are marked as having an unknown outcome and are never rerun or attached to an old PID. Live output is persisted on completion; a hard crash can lose output from an unfinished command.
- Windows commands run without a profile, prompts, or a visible shell window. Output uses UTF-8, PowerShell errors stop the script, and native program exit codes are preserved. The adapter uses PowerShell 5.1 syntax; PowerShell 7 is not required.
- New questions, the client OS, current directory, and approved command output go to your backend and AI provider. The application does not otherwise read the local filesystem for the model.
- Each model response costs one application credit, including follow-up responses interpreting a command result. Only new input is sent in each request, capped at 96 KB; the visible conversation can grow beyond that size. The browser and CLI receive live provider summaries and readable answer text. Model and reasoning settings remain backend-only.

### Streaming and reconnecting

The client sends `Accept: text/event-stream` to `POST /v1/chat`. Events are bounded JSON snapshots tagged `progress`, `complete`, or `error`; only the final validated `complete` response can create a command proposal. The backend decodes the root JSON answer text incrementally without exposing partial command JSON, forwards public reasoning summaries and commentary, and requests automatic summaries when supported. Raw reasoning is never forwarded. Some models omit summaries, so only the activity indicator appears until answer text arrives.

Every representation uses the same reservations, usage accounting, account affinity, final validation and settlement. Disconnecting does not cancel a paid request. A retry with the same request ID returns its cached answer, or a conflict while it is still running, without charging again. Known refunded failures receive a new request ID on the client. Quota failover resets the uncommitted preview so answers from different attempts cannot mix.

The local authenticated `GET /api/events` stream gives every tab the latest conversation, busy state, public progress and errors independently of the engine lock. A browser refresh reconnects to the current task; local tasks continue after the caller disconnects. Local command output remains in the independently accessible Task manager. Activity labels describe real provider or local execution stages; server-side file and shell tools remain disabled. The client displays the command reason while it runs.

Progress summaries are display-only and are not added to the model's conversation history. A bounded recent set survives browser refreshes in daemon memory, but is cleared by a new chat or daemon restart. Conversation messages and process records keep their existing disk persistence. Neither mode has a user-facing model or reasoning selector.

The backend emits SSE heartbeats every ten seconds and `Cache-Control: no-store`. Deploy the backend before the new client. Apache's existing catch-all HTTP proxy can forward this response; verify incremental delivery through the actual HTTPS deployment. A third-party buffering/compression layer must not collect the response before forwarding it. The local tests do not prove the production proxy's behavior.


### Long conversations and controls

Each chat request contains `base` (the committed message count) and only newly added messages. The backend validates that revision and atomically commits new input, accepted steering, the final reply, and the next revision. Cached retries return their original revision even after later turns. Full transcripts remain in local state and backend storage; the 96 KB transport cap no longer applies to total history.

Codex retains the provider thread and performs its native token-based context compaction. The client displays **Compacting context…** when Codex reports it. For account switching or recovery after an uncertain turn, the backend also requests a portable summary when recent recovery context plus new input reaches 64 KB. This summary is at most 8,000 UTF-8 bytes and is returned alongside the normal answer, in the same turn. It adds some token usage, but no separate conversation or application credit. Failover starts with that checkpoint and the recent messages, never the entire unbounded transcript. Summaries are lossy; the full transcript and captured command output remain available for inspection.

In the browser, **Enter** sends a message when idle and queues it during a task. **Ctrl+Enter** steers the currently running AI turn; during local command execution, use the queue instead. **Shift+Enter** inserts a newline. With an empty prompt during a task, the Send button becomes **Stop**. Stop interrupts generation, terminates the current local command if one is running, disables automatic execution, and leaves queued messages paused. Completed model replies that raced with Stop may still arrive, but their command proposals are not executed. Confirmed interrupted generations are refunded in application credits; consumed provider tokens remain recorded, potentially partially if the worker had to be terminated.

Queued messages survive browser refreshes and daemon restarts, can be removed before dispatch, and wait behind pending command approvals. Steering is sent to Codex's active turn, stored after acknowledgement, and included in the authoritative completion or refunded-error event. A lost acknowledgement can be retried with the same steering ID without sending it twice.

This is a deliberate protocol/schema break. Deploy the backend and client together; use the conversation reset instructions in `deploy/START-HERE.md`. There is no old-client compatibility path.

### Command output limits

Command results go directly into the main conversation. The client does not split output into chunks or create separate analysis conversations. GUI and CLI use the same behavior.

The result preview is capped at 8,000 UTF-8 bytes, including its truncation notice. Longer results retain the beginning and end, omitting the middle at Unicode boundaries. The notice tells the assistant to request filtered output if missing details matter. Captured stdout/stderr remain available in the Task manager, capped at 1 MiB per stream. Commands keep running and their pipes keep draining after the capture limit.

Only the normal follow-up reply consumes an application credit; there are no separate output-analysis charges. Native filters remain preferable for large logs. These application byte limits are independent of the model's token context window. The backend still enforces 64,000 bytes per message and 96,000 bytes for the complete serialized request; these are per-request limits, independent of total conversation length.

**Retry unfinished request** delivers the saved command result or retrieves the same pending response without executing the approved command again. An unfinished command from a daemon crash is reported as unknown, never automatically repeated.

CLI commands:

```text
/login                 Sign in through the hosted email/Google page
/character guide       Or companion / specialist
/commands on           Or off
/full on               Or off; requires commands enabled
/approve               Run the displayed pending command once
/deny                  Decline it
/credits               Refresh the authoritative balance
/buy                   Open backend-created Stripe Checkout
/retry                 Retry an unfinished request safely
/new                   Start a new conversation
/history               Refresh and show shared conversation state
/backend https://…     Change server and clear the old local session
/logout                Revoke the current backend session
/quit                  Leave the terminal prompt
/stop                  Stop the local daemon
```

## Backend setup

For a small server upload, run `python scripts/package_server.py` in the development checkout. Upload `dist/linux-assistant-server-source.tar.gz` and follow its `START-HERE.md`. The bundle excludes local tools, build output, credentials, and vendored sources; its Linux build script downloads the exact pinned Codex commit. Regenerate the archive after application or Codex updates.

Build `assistant-server` and run under a dedicated OS user. Codex App Server is compiled from source into this executable; no separate Codex installation is required on either the backend or client. Configuration is documented in [deploy/server.env.example](deploy/server.env.example); all options are also in `assistant-server --help`. Google and Stripe can be left entirely unconfigured until needed. Production mode requires HTTPS in the public URL, email delivery, and at least one operator account home.

1. Put a TLS reverse proxy in front of `127.0.0.1:8787`. Set `ASSISTANT_PUBLIC_URL` to the external origin. Use the example [systemd unit](deploy/linux-assistant.service), a private environment file, and a private state directory. Configure proxy request limits and authentication endpoint rate limits for an internet-facing deployment.
2. Configure SMTP delivery with `ASSISTANT_SMTP_HOST` (hostname without `ssl://`), `ASSISTANT_SMTP_PORT`, `ASSISTANT_SMTP_USERNAME`, `ASSISTANT_SMTP_PASSWORD`, and `ASSISTANT_EMAIL_FROM`. Connections use implicit TLS with certificate validation; Gmail uses `smtp.gmail.com:465` and an app password for the configured account. Store the password in the private server environment file, not in source control. SMTP settings are excluded from provider worker environments. Email login issues expiring, single-use codes with five verification attempts per device and a one-minute resend limit per email. The backend stores hashes of codes and session tokens. Sessions expire after 30 days and can be revoked by sign-out. Development mode does not connect to SMTP.
3. Configure a Google web OAuth client. Register exactly `https://YOUR-HOST/auth/google/callback`. The flow uses browser-bound state and PKCE, exchanges codes server-side, and queries Google's UserInfo endpoint for the verified email and stable `sub` identity.
4. Create a one-time Stripe Price and set the three Stripe variables plus `ASSISTANT_PACK_CREDITS`. Register `/webhooks/stripe` for `checkout.session.completed` and `checkout.session.async_payment_succeeded`. The backend verifies the raw-body signature and five-minute timestamp tolerance, requires `payment_status=paid`, and matches a server-created purchase. Redirects never grant credits. Repeated events cannot grant credits twice. Refunds/disputes are not automated in this MVP; reconcile them operationally before enabling public sales.
5. Back up the SQLite database using SQLite's online backup mechanism. Run one backend process per database; a file lock prevents two instances from refunding each other's active requests. Horizontal scaling would require a different queue/lease and database design.

The app's credits are independent of provider credits and subscription allowances. A user's balance is the sum of a backend ledger. Signup grants are unique per verified account. A request reserves one credit transactionally; successful responses are cached by user and request UUID, while provider failures are refunded. In-flight reservations are refunded after a backend restart. Reusing a request ID with different content is rejected. Payment credit amounts are never accepted from the client.

## ChatGPT account pool

The backend links the official **Codex App Server Rust source**, with its release recorded in `Cargo.toml` and exact commit pinned by the submodule. Each account runs an isolated worker by starting the same backend executable with an internal flag. Workers communicate over the documented JSONL protocol and use upstream ChatGPT authentication. No provider credentials are distributed to clients. Account access and available models depend on the signed-in subscriptions.

Create dedicated account directories under the backend user's state directory. Sign into each explicitly, for example:

```sh
./assistant-server --login-account /var/lib/linux-assistant/account-1
./assistant-server --login-account /var/lib/linux-assistant/account-2
```

Follow each printed sign-in URL and code, then list the absolute directories in `ASSISTANT_CODEX_HOMES`, separated by commas. On Windows use the same flag with a Windows directory. File storage keeps each account's credentials and provider threads in its own home. Treat these directories as credentials and include them in private backups. Do not copy personal Codex settings, plugins, or skills into them. The adapter rejects `config.toml` in account homes, ignores user/project configuration, disables command and integration tools, selects no host environment, and uses a read-only provider sandbox. Proposed commands are returned as structured JSON. Only the client executes local commands.

New conversations choose the account whose most depleted usage window has the lowest percentage used: `max(primary.usedPercent, secondary.usedPercent)`. Ties favor the account with fewer assigned conversations. This compares reported percentages, not absolute token allowances across different subscription plans. Idle accounts are refreshed before selection; busy accounts use their latest snapshot. Missing quota information is never treated as unused allowance.

Each conversation stays on its assigned account and persistent provider thread. Subsequent turns send only new messages. Assignments and committed history are scoped to the application user and survive backend restarts and account-list reordering. Different accounts generate concurrently; requests on one account serialize, with a five-second account-lock wait and a ten-minute overall generation timeout. Simultaneous requests in the same conversation are rejected without charging another credit.

An account changes only when a preflight usage snapshot reports exhaustion or a typed `usageLimitExceeded` error occurs. The exhausted account is skipped until its reset time (or briefly when no reset is supplied). Failover creates a **new thread on another account and receives the latest recovery checkpoint and only the messages since it**, consuming additional provider usage; it is not a free transfer of the original session. Failed or interrupted turns also discard the unconfirmed thread position and replay the checkpoint and recent committed messages on the same account on retry. Authentication and unrelated protocol errors do not trigger failover. If no account is available, the request is refunded. Live account rotation still needs validation with your signed-in accounts.

## Updating embedded Codex

Upstream source lives untouched in the `vendor/codex` Git submodule. Its exact commit, the root `Cargo.lock`, dependency patches, and `rust-toolchain.toml` make builds reproducible. The small Rust API adapter is in `crates/server/src/embedded.rs`; our JSONL integration and tests live under `crates/server/src/provider/`. The backend's first source build is substantially larger and slower than the client build.

Release builds use `lto = "off"` and 16 code-generation units to favor build time over the smallest possible executable. Normal release optimization and symbol stripping remain enabled. Changing these settings causes a full rebuild once; the Codex source and update workflow remain unmodified.

With Python 3.11+, Git, Rust/rustup and C/C++ build tools available:

```sh
python scripts/update_codex.py --check        # Read-only release check
python scripts/update_codex.py latest         # Update and verify latest stable release
# Or choose a specific stable release:
python scripts/update_codex.py rust-v0.164.0
```

The updater fetches the official release, updates the submodule, Rust version and upstream dependency patches, seeds Cargo's lockfile from upstream's tested dependency graph, then builds and runs the application and integration tests. It tests actual embedded-runtime startup without credentials and fixture-based account selection, thread persistence, failover, user isolation and parallel execution. An upgrade is reported as verified only after all checks pass. Run it in a clean checkout, review the changes and commit the submodule pin, manifests, toolchain, and lockfile together. CI verifies both operating systems. Updates are deliberate build-time changes, not downloads into a running production backend.

Upstream Rust APIs and dependency constraints can change. A failing upgrade may require adjusting the isolated adapter or a dependency before rebuilding; the script leaves the candidate changes available for inspection. Do not deploy an unverified update. Roll back using the previous application commit and `git -c core.longpaths=true submodule update --init`. Use `assistant-server --codex-version` to inspect a built executable.

The upstream Apache 2.0 license and NOTICE are preserved in the submodule and embedded in the backend; `assistant-server --third-party-notices` prints them. See [third-party provenance](THIRD_PARTY_NOTICES.md).

The operator dashboard adds independent tables to the existing schema. Updating the currently deployed backend preserves users, credits and conversations; it does not require a database reset.

## Operator dashboard and accounting

The backend model and reasoning are configured with `ASSISTANT_MODEL` and `ASSISTANT_REASONING_EFFORT` in the service environment. The reasoning setting also accepts `--reasoning-effort` on the command line. Accepted names are `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`, and `persistent`; the selected model must support the chosen level. An invalid name prevents startup. A configured effort is sent on every turn, including resumed conversations and administrator tests. Restart the backend after changing settings. Omitting it leaves Codex's thread default unchanged; an existing thread can retain its previous effort. These are server settings, not editable dashboard controls.

Set a private `ASSISTANT_ADMIN_PASSWORD` (at least 16 bytes) in the service environment and visit `/admin`. The dashboard is embedded in the single backend binary. It provides account quota windows and resets, per-account prompt tests, token charts and breakdowns, a credit ledger, searchable users, manual credit adjustments, blocking, session revocation and Stripe Checkout links for individual users. Complete root-shell deployment and Stripe setup instructions are in [deploy/START-HERE.md](deploy/START-HERE.md).

ChatGPT quota polling runs every five minutes. Browser refreshes read cached state; normal provider selection also caches quota reads for five minutes, except confirmed quota failures. Busy workers are not interrupted by monitoring. Cached readings show their timestamp and errors rather than reporting unknown quota as zero.

Each real provider attempt records its account, request, conversation, purpose, model, timestamps, completion/error and available token counters. Failed attempts, failover and administrator tests are included. Cumulative token snapshots are differenced against the preceding turn and duplicate notifications are not summed. Resumed history is not charged again in reports. Missing counters remain unknown, and interrupted attempts may be partial. Historical token usage from before this instrumentation is not backfilled. Cached input is included in input and reasoning is included in output. Application credits and subscription quota percentages remain separate quantities.

Token data is recorded for all normal real requests, even when the dashboard is disabled. Quota-history polling runs only when the dashboard is enabled. Admin tests use ephemeral threads, no tools, and no end-user credit charge. Their prompts/replies are shown to the operator but only usage metadata is retained by the application. This does not change provider-side data retention.

All dashboard APIs require a separate eight-hour administrator session; normal user tokens cannot grant access. Password changes invalidate existing sessions, mutations require a same-origin custom header, and cookie/response policies prevent cross-site embedding and caching. Credit changes require a reason and a stable idempotency key. Balance and Stripe fulfillment continue to use the authoritative ledger.

Protocol reference: [Codex app-server](https://learn.chatgpt.com/docs/app-server). Integration references: [Google OpenID Connect](https://developers.google.com/identity/openid-connect/openid-connect), [Stripe webhook verification](https://docs.stripe.com/events/manage-webhook-endpoints), and [Google app passwords](https://support.google.com/accounts/answer/185833).

## Build the standalone Linux executable

On Linux, install `musl-tools`, then run this from the Git checkout:

```sh
./scripts/build_client.sh
```

The script installs the Rust musl target and initializes the pinned Codex submodule if missing, because Cargo reads the backend's workspace manifests even for a client-only build. It compiles only the client and its dependencies; Codex is not linked into the client.

Distribute `target/x86_64-unknown-linux-musl/release/linux-assistant`. Browser HTML, JavaScript, and CSS are compiled into it. TLS uses Rustls and bundled trust roots, so the app does not require OpenSSL. A separate build is needed for another CPU architecture.

Cross-compilation from Windows can use Zig and `cargo-zigbuild`:

```sh
cargo zigbuild --release --locked --target x86_64-unknown-linux-musl -p linux-assistant
```

On this Windows workspace, an isolated Rust toolchain and Zig were installed in ignored `.tools/`; no system PATH changes were made. [CI](.github/workflows/check.yml) tests native command execution and builds artifacts on both Linux and Windows.

## Build and run on Windows

Install Rust with the MSVC toolchain and Visual Studio C++ Build Tools, then use PowerShell:

```powershell
cargo test --workspace --locked
cargo build --release --locked --workspace
.\target\release\assistant-server.exe --dev
# In another terminal:
.\target\release\linux-assistant.exe --cli
# Or launch browser mode:
.\target\release\linux-assistant.exe --desktop
```

Distribute `target/release/linux-assistant.exe`. The repository's `.cargo/config.toml` statically links the MSVC runtime; only normal Windows system libraries and Windows PowerShell are needed. Backend accounts, credits, and settings work identically in both client modes. The `--install-desktop` option creates a Linux freedesktop launcher and is Linux-only.

Run the optional provider protocol fixture on Windows with:

```powershell
cargo build -p assistant-server --bin assistant-server --example provider-fixture --locked
$env:ASSISTANT_TEST_PROVIDER="$PWD/target/debug/examples/provider-fixture.exe"
$env:ASSISTANT_TEST_SERVER="$PWD/target/debug/assistant-server.exe"
cargo test -p assistant-server integration_ --locked -- --ignored
```

## Tests

```sh
cargo test --workspace --locked
cargo build -p assistant-server --bin assistant-server --example provider-fixture --locked
ASSISTANT_TEST_PROVIDER="$PWD/target/debug/examples/provider-fixture" \
  ASSISTANT_TEST_SERVER="$PWD/target/debug/assistant-server" \
  cargo test -p assistant-server integration_ --locked -- --ignored
```

Tests cover ledger idempotency and crash recovery, insufficient credits, device-session consumption, email-code attempt limits, session revocation, payment signature validation and replay, account rotation over JSONL, local Origin/Host/token checks, HTTPS enforcement, OS-specific proposals, and actual execution through the approval flow. Process tests check live output and stopping while the conversation is locked, saved results, Unicode-safe output truncation, results staying in the main conversation, provider thread isolation, and restart/retry without command reexecution. Windows tests also check Unicode and quoting, working directories, PowerShell/native failures, both output caps, and descendant cleanup on Stop and cancellation. Live credentials are unnecessary for these tests.

Native Linux execution is checked by CI because this Windows development host has no Linux runtime. The runtime contract test uses a fresh unsigned-in account directory and a local HTTP model stub to verify real App Server turns and thread resumption without contacting an AI provider. Protocol fixtures test quota behavior without using real subscriptions.

SMTP tests use a local TLS server to check authentication, configurable ports and senders, Unicode recipients, code delivery and verification, authentication failures, and rejection of untrusted certificates. They send no real email and require no mail account credentials.

Backend validation includes real embedded-runtime token counters, resumed threads, duplicate usage events, quota caching, account-specific tests, authentication, credit adjustment idempotency, filtering, and user access controls. Browser checks cover desktop/mobile layout, login, credit adjustment, blocking/unblocking, user filtering, and escaped international text. Earlier client checks covered live PowerShell output, stopping while busy, executable arguments, stderr, and stdout downloads. The embedded backend imports only Windows system DLLs; a separate Codex executable is not needed. Production email login and a real AI reply were verified on ai.slax.org before this dashboard update; live Stripe payments and multi-account quota exhaustion have not been tested with real credentials.

The existing backend is deployed on ai.slax.org; local source changes still require a server build and installation. Google and Stripe require their own configuration. The client UI remains a functional preview pending the separate design decision.

Conversation protocol validation also covers transcripts above the old 96 KB limit, checkpoint-based account failover, idempotent steering, native Codex compaction and interruption, queued-message crash recovery, and stopping real PowerShell commands from the task manager. The database reset test checks preserved users, sessions, credits, purchases, and usage plus a readable backup. Browser checks exercise queue/steer/Stop, paused queues after interruption, mobile layout, and dark mode with a local simulated backend; these do not consume real AI usage.
