# Linux Assistant

A Rust assistant for Linux and Windows with a fullscreen terminal interface and an embedded browser UI. Each platform has its own **single executable**; the Linux client links to the system's glibc and Windows statically links the MSVC runtime. A separate Rust backend owns accounts, application credits, payments, and AI access. Both interfaces show streaming progress and a task manager for local commands.

## Run locally

Install Rust through rustup and the platform's C/C++ build tools. Initialize the pinned Codex source once (the long-path option is needed on Windows), then build:

```sh
git -c core.longpaths=true submodule update --init --depth 1
cargo build --workspace --locked
./target/debug/assistant-server --dev
```

In another terminal:

```sh
./target/debug/linux-assistant --backend http://127.0.0.1:8787
# Prints a localhost URL; open it in your preferred browser.
# For the standalone terminal interface:
./target/debug/linux-assistant --backend http://127.0.0.1:8787 --cli
# Or explicitly open browser mode:
./target/debug/linux-assistant --backend http://127.0.0.1:8787 --desktop
```

Every launch requires a backend from `--backend` or the client config file; there is no built-in server address. A hostname without a scheme uses HTTPS; explicit HTTP is allowed only for loopback development servers. Development mode is **simulated AI** and **simulated email**, binds only to loopback, and rejects payments. Sign in with any test email and enter the displayed six-digit development code directly in the client. Commands and full access start enabled. Development mode simulates text responses; native tool calls, questions, plans and web search require a configured AI account.

For a deployed backend:

```sh
./linux-assistant --backend ai.slax.org
./linux-assistant --backend ai.slax.org --install-desktop
```

The optional Linux desktop launcher stores the backend argument and points to the executable's current location. Install the executable where it will remain before creating the launcher. Desktop/browser launch needs the system's normal default-browser opener (`xdg-open` or equivalent on Linux). There is no Node.js, Python, external web asset, or Codex runtime dependency on the **client**. Commands use the host's `/bin/sh` on Linux or built-in Windows PowerShell 5.1 on Windows, plus installed programs.

## Client behavior

- Launching with `--backend SERVER` starts or reuses the background server, prints its complete browser URL, and exits. `--desktop` explicitly opens the default browser. Closing a browser tab leaves the local service running.
- `--cli` runs the assistant directly in a monochrome fullscreen terminal, with no local web server. It shares the browser client's saved account, settings and chat, but only one mode may own that data directory at a time. Close the browser service before switching to terminal mode; `LINUX_ASSISTANT_HOME` can select a separate profile. Ctrl+C exits the console and stops its current task. Ratatui/Crossterm are compiled into the executable; ncurses or another runtime package is not required.
- The console uses Workbench layout B: chat and Tasks side by side in wide terminals, and a full-screen task view in narrow ones. Resizing is automatic. Full access and shortcut hints share a single bottom row. Enter sends or queues; Ctrl+Enter steers (F5 works in terminals that cannot distinguish Ctrl+Enter). Shift/Alt+Enter inserts a newline. F1 shows all shortcuts, F2 toggles Tasks, F3 changes Full access, F4 starts a new chat, F6 opens credits, F7 retries, F8 stops the selected command, and F9 expands completed steps. Questions accept a click or number without an extra confirmation; Tab selects a custom answer. In the queue, use Tab then arrows to select, E to edit in place, Delete to remove, and F5 to steer.
- Console sign-in uses email and a six-digit code. Credit packs open Stripe checkout in the browser; R in Account refreshes the balance afterward. Use `/attach /path/to/file` to attach original PNG/JPEG/WebP images or UTF-8 files, then type the prompt. `/clear-attachments` removes pending attachments. Inputs estimated to cost more than 8 credits require sending again to confirm. Existing image attachments appear as filenames in the console; visual thumbnails remain available in browser mode.
- State is stored under `$XDG_DATA_HOME/linux-assistant` (normally `~/.local/share/linux-assistant`) on Linux or `%LOCALAPPDATA%\linux-assistant` on Windows. `LINUX_ASSISTANT_HOME` overrides the directory for isolated tests. Session tokens, history, pending approvals, and unfinished request IDs are saved atomically. Linux files use private permissions; Windows files inherit the user directory's ACLs. The backend is configured by the launch argument or client config file. An active daemon can only be reused with the same backend; stop it before switching. Switching servers clears the previous session and chat.
- The ordinary localhost URL works without an authorization token. The local client trusts users and programs on this computer; commands run as the OS user that launched it. Local API routes enforce the exact loopback Host, same Origin, and browser fetch metadata to reject unrelated websites. The remote backend allows CORS from any origin for `/v1/*` and `/health`, without cookies; account APIs still require bearer authentication. Admin and OAuth routes retain their origin checks.
- Local commands and **Full access** start enabled. Full access authorizes automatic execution of validated proposals, including an already displayed proposal. Turning it off requires approval of each exact command and directory and takes effect before the next command. Stopping a task or process preserves the Full access choice. On client restart, full access starts enabled if local commands are allowed. Approval IDs remain one-use.
- The client detects its operating system automatically and supplies it to the backend with every chat request. The AI chooses shell syntax and paths for that client, regardless of the backend's own OS. Both interfaces display the selected shell.
- Commands run as the current user, **not in a sandbox**, with no automatic elevation. `exec_command` normally waits 10 seconds, then returns partial output and a session ID without stopping the command. The model can poll or send exact input/EOF with `write_stdin`, or cancel with `stop_process`. A separate optional `timeout_ms` sets a hard runtime limit; null leaves runtime unlimited. Input uses pipes, not a PTY. Stop terminates the owned Linux process group or Windows Job Object. Normal launcher exit preserves surviving children, which remain visible and stoppable in Tasks. Capture retains a 1 MiB prefix per stream plus a 64 KiB rolling tail for fresh polling output. Deliberately detached Linux sessions or work delegated to external services can outlive tracking; this is not a containment boundary.
- The floating **Task manager** shows currently running commands in a table with command text, PID, elapsed time, and Stop. Stop kills that command and its child processes and turns off full access. Completed commands move to a separate history section (latest 50 records). Expand a row for PID, timestamps, directory, exact command, executable arguments, live stdout/stderr, exit code, and execution errors. You can save either captured stream as text. Process polling and stopping remain responsive during command execution and AI requests.
- Completed process records are saved in the private client state directory under `processes/<id>.json`; older records remain on disk after leaving the panel. After a restart, unfinished records are marked as having an unknown outcome and are never rerun or attached to an old PID. Live output is persisted on completion; a hard crash can lose output from an unfinished command.
- Windows commands run without a profile, prompts, or a visible shell window. Output uses UTF-8, PowerShell errors stop the script, and native program exit codes are preserved. The adapter uses PowerShell 5.1 syntax; PowerShell 7 is not required.
- New questions, the client OS, current directory, and approved command output go to your backend and AI provider. The application does not otherwise read the local filesystem for the model.
- Each task is charged for its token usage, with one combined rounding across command follow-ups. Only new input is sent in each request; the visible conversation can grow beyond the transport budget. The browser and CLI receive live provider summaries and readable answer text. Model and reasoning settings remain backend-only.

Sign-in verifies automatically once all six code digits are entered or pasted. The greeting appears after sign-in. Settings shows the current credit balance and Purchase. A Recent purchases disclosure below the credit packs loads the five most recent completed purchases on demand; `/v1/purchases` requires the user’s bearer token and exposes only credit amounts and payment timestamps. Deploy the updated backend before using purchase history. The chosen brand name is rendered in the initial HTML, including the browser title.

The browser uses embedded SVG icons in both themes and keeps credits and account controls visible while scrolling. Expanded command output remains open as new replies arrive. The composer reserves space for queued messages and longer drafts. Task manager output is fetched only when its metadata changes; idle polling slows down and hidden tabs stop polling.

### Branding and client defaults

```sh
./linux-assistant --backend ai.slax.org --name "Slax AI"
./linux-assistant --backend ai.slax.org --name "Slax AI" --logo /usr/share/slax-ai/logo.png
```

`--name` changes the browser title, header, terminal greeting and generated Linux launcher. `--brand-mark /path/to/clover.png` shows a square PNG beside the name. `--logo` replaces both the icon and text in the browser header with a PNG; the name remains the accessible label and page title. Logo files are read once, at most 1 MiB and 4096 × 4096 pixels. Stop an existing daemon before changing its backend or branding.

`--assistant-image /path/to/assistant.png` replaces the sign-in character with a PNG from disk. The built-in character is used when omitted. The file is read on each request without browser caching: replace the file and refresh to update the picture without rebuilding or restarting. Restart the daemon to choose a different path.

The client reads `/etc/linux-assistant/config` on Linux and `%ProgramData%\linux-assistant\config` on Windows. Use `--config PATH` for another file. All operational argument names are accepted as TOML keys; unknown keys fail startup. Command-line values take precedence, including `--desktop=false`. An explicit launch mode overrides configured launch modes. Use absolute logo paths. See [the commented example](deploy/client-config.example).

### Attachments

Assistant responses recognize existing local file paths in inline code, ordinary text and Markdown links. Clicking a file opens an inline preview in a new browser tab through the loopback client. A small copy button next to the link copies the resolved filesystem path rather than the preview URL. The native client reads the file under its own OS account, so a browser running as `guest` can download a file created under `/root` when the client runs as root. Relative links resolve from the client's home directory. Directories, missing files, devices and pipes remain ordinary text. File previews use opaque local IDs and stream without loading the whole file into memory. PDFs and images use their browser content types; text files are detected from their content. HTML and SVG previews are sandboxed with scripts disabled, isolated from the local assistant API. Browser-unsupported binary formats may still be downloaded by the browser. No remote backend transfer is involved.

The paperclip opens the file picker directly. Drag files onto the composer or paste a screenshot into the prompt. PNG, JPEG, WebP and UTF-8 text/code files are supported; PDFs, Office files and archives are not yet supported. Images appear as small previews in drafts and chat; clicking a preview opens the original in a keyboard-accessible lightbox (Escape or the close button dismisses it). Agent-viewed images also appear in the conversation and remain visible after the work details collapse. Images retain their original bytes and dimensions: the application never resizes or recompresses them, and requests original image detail from Codex. Provider processing still follows the selected model's capabilities and limits.

Before sending or steering input estimated to cost **more than 8 credits**, a short confirmation shows the estimated input credits using the backend's current input rate. Cancel preserves the draft. Text uses roughly one token per three UTF-8 bytes; images use 32 × 32 patches with an approximate 1.2 multiplier. These are intentionally rough estimates, not authoritative billing: history, cached input, responses and command follow-ups are excluded. Actual task usage is billed by the backend and rounded up once.

Attachments follow queued messages through editing, cancellation, steering, restart and retry. Live browser snapshots contain names only; queue editing retrieves the original contents. Images use native multimodal input, never base64 text inside the prompt. Each attachment-bearing turn requests a recovery checkpoint; later account recovery uses that summary and text attachments, rather than automatically resending old image pixels. No separate analysis conversation is created.

Deploy the new backend before using these client features, including the current input-rate field used for the credit estimate.

Update and restart both backend and client together for the local-session and typed-progress protocol, then start a new chat; no configuration changes are required.

### Native tools, questions, plans and web search

The embedded App Server registers native dynamic tools for command execution, stdin/polling/Stop, file reads, listings, search, exact edits and file creation. It also provides `local_view_image`, `local_get_metadata`, `local_create_directory`, `local_copy_path`, `local_move_path`, `local_remove_path`, `local_create_symlink` and `local_set_permissions`. All local effects run in the existing Rust client, without extra Linux packages. File edits replace one exact unique UTF-8 block; file creation refuses to overwrite an existing path. Reads, searches, directory listings and returned tool output are bounded and report pagination or truncation. Copy handles files, symbolic links and recursive directory trees without following link targets. Copy/move refuse existing destinations; moves require the same filesystem. Removal is permanent, and nonempty directories require explicit recursive=true. POSIX permission modes are available on Linux; Windows supports readonly/writable, with OS ACL changes left to approved commands. Windows symlinks may require system privileges. File changes already committed are not rolled back by Stop. Full access controls automatic local operations; otherwise the browser or terminal asks for approval. Read-only polling needs no new approval.

`local_view_image` snapshots an approved PNG, JPEG or WebP file and sends its original pixels through native `inputImage` tool content, with normal token billing. Originals and lazily generated 192 × 144 maximum UI previews are cached privately under the client data directory’s `media/` folder. UI snapshots contain local image URLs instead of base64 content, and downloads do not require the conversation lock. The application tool-result journal stores image hashes rather than pixels for retry acknowledgements and recovery evidence. Original image data uses the existing 64 MiB JSON transport budget. Start a new chat after updating both backend and client so the provider thread advertises the new tools.

Native questions show selectable options and a custom answer, and plans show live pending, active and completed steps. Live web search runs through Codex and requests source links. MCP/connectors, subagents, persistent memories, image generation and realtime/voice remain outside this integration.

One application task now stays in one Codex turn across tool calls, including human decisions. Measured token usage is debited incrementally using the admitted rates; rounding remains cumulative across the task, and completion costs at least one credit. Zero or negative balances block new tasks and steering. An admitted task can finish. Provider tool charges outside reported token usage are not included in the configured token rates.

`/v1/chat/{id}/respond` accepts an authenticated answer only for its owning user and pending request. Native tool/question evidence is saved in `tool_calls` for recovery; private client acknowledgements prevent automatic replay of side effects after a disconnect. Retrying the same active chat request reconnects its SSE snapshot. Update backend and client together and start a new chat, since native tools are registered when a provider thread starts. No admin settings or extra packages are required. When upgrading a client with the previous saved-chat format, stop it and run the normal launch command once with `--reset-chat`, then launch normally; this backs up the chat while retaining sign-in and settings.

### Streaming and reconnecting

The client sends `Accept: text/event-stream` to `POST /v1/chat`. Events are bounded JSON snapshots tagged `progress`, `complete`, or `error`. Pending native tools and questions remain in progress snapshots until acknowledged. The final validated `complete` response contains only the final answer. The backend decodes the final root JSON answer text incrementally, forwards public reasoning summaries and commentary, and requests automatic summaries when supported. Raw reasoning is never forwarded. Some models omit summaries, so only the activity indicator appears until answer text arrives.

Native chat requests require the SSE Accept header, so client tools and questions can always be delivered. If the local client disconnects, the backend retains the paid turn for five minutes to allow reconnection, then interrupts it and settles recorded usage. Closing only the browser keeps the local client connected and does not start this timer. A retry with the same request ID reconnects an active turn or returns its settled result without charging again. Known settled failures receive a new task and request ID on the client. Quota failover resets the uncommitted preview so answers from different attempts cannot mix.

The local authenticated `GET /api/events` stream gives every tab the latest conversation, busy state, public progress and errors independently of the engine lock. A browser refresh reconnects to the current task; local tasks continue after the caller disconnects. Local command output remains in the independently accessible Task manager. Activity labels describe real provider or local execution stages; the backend host’s file and shell tools remain disabled. The client groups consecutive native command, file and web-search activity until visible commentary, a plan or user input breaks the group. Polling updates the original session row; nested command output and verified file changes remain expandable. Typed reasoning summaries render as Markdown, with the latest summary line supplying the activity title. Accepted steering appears at its position in the progress stream. Steering uses the active Codex turn, including while the client is executing a command; the normal backend credit checks apply.

Progress summaries are display-only and are not added to the model's conversation history. Display history is bounded to 128 parts and about 96 KB of text/metadata, with individual display strings shortened at UTF-8 boundaries. Old display entries are evicted instead of failing a running task; actual native tool arguments and model history are unchanged. A bounded recent set, completed compactions and command details are saved locally at task transitions and survive browser refreshes and daemon restarts. A new chat clears them. Conversation messages and process records keep their existing disk persistence. Neither mode has a user-facing model or reasoning selector.

The backend emits SSE heartbeats every ten seconds and `Cache-Control: no-store`. Deploy the backend before the new client. Apache's existing catch-all HTTP proxy can forward this response; verify incremental delivery through the actual HTTPS deployment. A third-party buffering/compression layer must not collect the response before forwarding it. The local tests do not prove the production proxy's behavior.


### Long conversations and controls

Each chat request contains `base` (the committed message count) and only newly added messages. The backend validates that revision and atomically commits new input, accepted steering, the final reply, and the next revision. Cached retries return their original revision even after later turns. Full transcripts remain in local state and backend storage; the 96 KB transport cap no longer applies to total history.

Codex retains the provider thread and performs its native token-based context compaction. The client displays **Compacting context…** when Codex reports it, then retains **Context automatically compacted** in the transcript. Terminal events include the final progress snapshot so a fast completion does not erase this marker. For account switching or recovery after an uncertain turn, the backend also requests a portable summary when recent recovery context plus new input reaches 64 KB. This summary is at most 8,000 UTF-8 bytes and is returned alongside the normal answer, in the same turn. It adds some token usage, but no separate conversation or application credit. Failover starts with that checkpoint and the recent messages, never the entire unbounded transcript. Summaries are lossy; the full transcript and captured command output remain available for inspection.

In the browser, **Enter** sends a message when idle and queues it during a task. **Ctrl+Enter** steers the currently running AI turn, including during local command execution. **Shift+Enter** inserts a newline. With an empty prompt during a task, the Send button becomes **Stop**. Stop interrupts generation, terminates the current local command if one is running, and leaves queued messages paused. Full access keeps its current setting. Completed model replies that raced with Stop may still arrive; new local operations are not executed after Stop. Confirmed interrupted generations settle their recorded token usage, potentially partial if the worker had to be terminated.

Streaming follows the latest output until the user scrolls away. Scrolling back to the bottom, clicking the floating three-dot button, or sending a prompt resumes following. The button sits above the composer and also returns to the end after generation finishes.

Finished and stopped tasks show a copy button and their local completion time. Copy uses the final answer, or the interrupted preview. Command activity expands to show the command, directory and output, with a link to its task-manager record. The current activity label has an animated highlight that respects reduced-motion settings. Completion times, summaries and command links are saved locally in `presentation.json` and never added to model input. Existing messages without this metadata do not receive invented timestamps. File edits are verified against before/after contents of model-declared paths. Nested action rows show command output or a text diff with added/removed line counts. The client inspects at most 16 explicitly declared files (up to 1 MiB each), never recursively scans directories, and only labels verified byte changes as edits. Text diffs compare up to 256 KiB combined, with a 32 KiB preview; unreadable, undeclared or larger files may not appear. Diff data stays local.

Every user message also has a copy button. Adjacent command events share one expandable indicator that keeps its position while commands run; conversation text separates later command groups. The low-credit warning includes a Buy credits button, available even while a task runs. Purchase buttons open a checkout tab during the click and navigate it after the backend returns the Stripe URL, without displaying an extra link. If the browser blocks the new tab, checkout opens in the current tab.

Backend connection failures receive up to five attempts, with five-second waits and a visible countdown. Connection establishment has a separate five-second timeout. A streamed response with no bytes or heartbeat for 45 seconds is treated as disconnected; there is no total generation-time limit. Chat requests retain their original IDs. Safe chat/control and read requests also retry gateway failures; application errors and ambiguous login/payment responses are not replayed. Interrupted response streams retain the existing manual Retry path. Exhausting retries shows an error without closing the client.

Queued messages survive browser refreshes and daemon restarts and wait behind pending command approvals. Each has Steer, Edit, and Delete actions. Editing holds its original slot and pauses dispatch at that slot until submitted or cancelled; submitting replaces it in place. Ctrl+Enter can steer an edited draft directly. Queued steering removes an entry only after backend acceptance, under the same lock used for dispatch. Steering is included in the authoritative completion or settled-error event. A lost acknowledgement can be retried with the same steering ID without sending it twice.

New steering requires a positive credit balance, including during an already admitted task. At zero or below, both Ctrl+Enter and queued Steer actions are unavailable; the backend also enforces this before forwarding input to Codex. Replaying an already accepted steering ID still recovers its acknowledgement. The running task can finish and Stop remains available.

Deploy the backend and client together; use the local chat reset instructions in `deploy/START-HERE.md` when replacing an earlier client. The token-billing update requires the documented one-time conversation reset on the backend. There is no old-client compatibility path.

### Command output limits

Command results go directly into the main conversation. The client does not split output into chunks or create separate analysis conversations. GUI and CLI use the same behavior.

The result preview is capped at 8,000 UTF-8 bytes, including its truncation notice. Longer results retain the beginning and end, omitting the middle at Unicode boundaries. The notice tells the assistant to inspect the saved output record rather than repeat a command with side effects. Dedicated file tools budget whole JSON fields and entries, and report byte offsets or pagination cursors for the data actually returned. Captured stdout/stderr remain available in the Task manager, capped at 1 MiB per stream. Commands keep running and their pipes keep draining after the capture limit.

Follow-up token usage is included in the current task bill; there are no separate output-analysis charges. Native filters remain preferable for large logs. These application byte limits are independent of the model's token context window. Ordinary message text remains bounded (16,000 bytes for a user prompt; 64,000 for other messages), and text/attachment metadata has a 96,000-byte request budget. Attachment payloads have no separate count, image-resolution, or credit-based size cap. The HTTP transport accepts up to 64 MiB of serialized JSON including base64 overhead; provider context and media limits still apply. Requests above the transport limit fail without starting AI work.

**Retry unfinished request** delivers the saved command result or retrieves the same pending response without executing the approved command again. An unfinished command is polled using its existing session if its record is available; uncertain edits and stdin delivery are reported as unknown, never automatically repeated. Stop permanently cancels the current local operation leases, including file work already dispatched to a blocking worker. A later task cannot revive them by resetting its own Stop flag. Dedicated edits stage replacements, check for changed content, and preserve Linux ownership, modes and extended attributes/ACLs, or Windows replacement metadata. Hard-linked files on Unix require an explicit command instead of atomic replacement. New files are staged and committed without overwriting existing paths.

CLI commands:

```text
/login                 Enter your email and six-digit code in the terminal
/commands on           Or off
/full on               Or off; requires commands enabled
/approve               Run the displayed pending command once
/deny                  Decline it
/credits               Refresh the authoritative balance
/buy                   Open backend-created Stripe Checkout
/retry                 Retry an unfinished request safely
/new                   Start a new conversation
/history               Refresh and show shared conversation state
/logout                Revoke the current backend session
/quit                  Leave the terminal prompt
/stop                  Stop the local daemon
```

## Backend setup

For a small server upload, run `python scripts/package_server.py` in the development checkout. Upload `dist/linux-assistant-server-source.tar.gz` and follow its `deploy/START-HERE.md`. The bundle excludes local tools, build output, credentials, and vendored sources; its Linux build script downloads the exact pinned Codex commit. Regenerate the archive after application or Codex updates.

Build `assistant-server` and run it under a dedicated OS user using the [systemd unit](deploy/linux-assistant.service). Production Linux binds `127.0.0.1:8787` and stores its database at `/var/lib/linux-assistant/assistant.db`. The backend embeds Codex App Server and its V8-based Code Mode host; no separate Codex installation is needed. Code Mode orchestrates the advertised local tools, while command execution and file access still happen exclusively in the client. Each account worker creates temporary executable aliases in its private data directory and communicates with the host through stdio pipes. Use the build wrapper (`python scripts/cargo_backend.py build --locked -p assistant-server`) to download and verify the pinned Codex V8 library. The first backend build is larger and slower than a client-only build; later builds reuse the library. The wrapper requires Python 3.11 or later.

Open `/admin` through your HTTPS reverse proxy. Until an administrator password exists, it redirects to the first-run password form. Only this setup form is available without authentication; the operational APIs remain protected. The first submitted valid password claims the backend. After setup, sign-in is required. Administrator passwords are salted and stretched with PBKDF2-HMAC-SHA256; only their hashes are stored.

All backend configuration is entered in **Admin → Settings**: Public URL, welcome credits, model, reasoning, token prices, assistant context, SMTP, sign-in providers and Stripe. There is no environment-file configuration, import, legacy option parser or account-directory list. Settings and integration secrets live in the private database. Secret values never return to the dashboard: blank fields keep saved values, while explicit clear controls remove them. Saving applies to new requests immediately; running AI requests retain their settings snapshot, and billing tasks retain their admitted rates.

**Admin → Settings → Assistant context** adds shared product background and instructions to every conversation, including account tests. Leave it blank to add nothing. The backend reapplies it as base instructions on every thread start or resume, so native compaction and checkpoint-based account switching do not remove it. Saving changes applies to subsequent requests, including existing conversations; active requests keep their settings snapshot. Context is stored in the database and included in settings backup and restore. Keep it concise because it contributes to model input.

Enable or disable email, Google, Microsoft and GitHub sign-in individually. Disabled providers are hidden in the client and blocked on the backend. SMTP uses implicit TLS; Gmail uses `smtp.gmail.com:465` with an app password. The admin includes short provider setup instructions and callback URLs; [the detailed registration guide](deploy/SOCIAL_SIGN_IN.md) covers `ai.slax.org`. Some first-time social identities require an email ownership code even if primary email login is disabled. Configure SMTP for those checks.

Stripe uses an editable list of credit packs, each with a one-time Price ID, displayed USD price and credit amount. The client displays the configured packs as purchase buttons side by side; the terminal lists the same choices. The configured USD amount is for display; the operator must keep it aligned with the Price in Stripe. Checkout charges that Price ID. Register `/webhooks/stripe` for `checkout.session.completed` and `checkout.session.async_payment_succeeded`. Signed webhooks grant credits after confirmed payment; redirects never grant credits. The receipt page waits for that recorded confirmation, thanks the user and closes after three seconds. The client independently watches its owned purchase and refreshes its balance and recent purchases automatically, including while an AI task runs. It keeps credit-pack cards in place during manual or automatic balance refreshes. Repeated events cannot grant credits twice. Refunds and disputes require operational reconciliation.

The [deployment guide](deploy/START-HERE.md) covers a fresh installation and updating the binary. Back up the database and `managed-accounts/` directory. One backend process owns each database.

The app's credits are independent of provider credits and subscription allowances. A user's balance is the sum of a backend ledger. New verified users receive 100 credits by default (editable in Settings), once per account; existing grants are unchanged. A positive balance permits starting a task. Zero or a negative balance blocks new tasks, but an admitted task and its command follow-ups can finish and take the balance below zero. Payment credit amounts are never accepted from the client.

Token pricing is configured in **Admin → Settings → AI and token prices**, in credits per million tokens. Defaults are 200 input, 10 cached input and 1000 output credits per million tokens. Rates accept up to six decimal places. Saved prices apply immediately to new tasks; existing tasks retain their admitted rates.

A task consists of one user prompt, accepted steering, and its automatic command follow-ups. Its cost is `ceil(((input-cached_input)*input_rate + cached_input*cached_rate + output*output_rate)/1,000,000)`, with a minimum of one credit for a successful task. Reasoning tokens are already included in output. Exact decimal arithmetic avoids floating-point rounding. Each completed request settles only the increase in the rounded task total, so multiple commands do not each incur another minimum charge. Failed attempts, account failover and interrupted requests include measured token usage; failures before any measured work cost nothing. Missing counters are never estimated; an unmeasured successful task costs the minimum. Development replies have no measured usage and use the same task minimum.

Successful responses are cached by user and request UUID, and replay never charges twice. Startup recovery settles recorded partial usage once. A retry after a settled failure starts a new task and requires a positive balance. Reusing a request ID with different content, reusing a finished task, and sending a new user prompt as a command continuation are rejected. Per-attempt credit attribution follows actual ledger debits, with shared task rounding allocated in attempt order.

## ChatGPT account pool

The backend links the official **Codex App Server Rust source**, with its release recorded in `Cargo.toml` and exact commit pinned by the submodule. Each account runs an isolated worker by starting the same backend executable with an internal flag. Workers communicate over the documented JSONL protocol and use upstream ChatGPT authentication. No provider credentials are distributed to clients. Account access and available models depend on the signed-in subscriptions.

Use **Admin → Accounts → Add account** to sign into ChatGPT with a device code or add an OpenAI API key. Credentials are saved as `/var/lib/linux-assistant/managed-accounts/<UUID>/auth.json` and rediscovered on restart. No account-directory configuration is needed. Pause/resume controls are available for each account.

New conversations choose the account whose most depleted usage window has the lowest percentage used: `max(primary.usedPercent, secondary.usedPercent)`. Ties favor the account with fewer assigned conversations. This compares reported percentages, not absolute token allowances across different subscription plans. Quotas are refreshed when the five-minute cache expires, including while conversations are running. Missing quota information is never treated as unused allowance.

Each conversation stays on its assigned account and persistent provider thread. Subsequent turns send only new messages. Assignments and committed history are scoped to the application user and survive backend restarts and account-list reordering. Independent conversations run concurrently through one shared worker per account, with no application limit on the number of concurrent conversations; provider limits and server resources still apply. Replies, progress, token counters, steering and cancellation are routed to their own conversation. Admin tests and quota refreshes can run alongside them. Conversation turns have no overall timeout while commands or human input are pending; Stop remains available. Administrator account tests retain their own timeout. Simultaneous requests in the same conversation are rejected without charging another credit.

An account can change when the operator pauses it, removes it from configuration, a preflight usage snapshot reports exhaustion, or a typed `usageLimitExceeded` error occurs. The exhausted account is skipped until its reset time (or briefly when no reset is supplied). Failover creates a **new thread on another account and receives the latest recovery checkpoint and only the messages since it**, consuming additional provider usage; it is not a free transfer of the original session. Failed or interrupted turns also discard the unconfirmed thread position and replay the checkpoint and recent committed messages on the same account on retry. Authentication and unrelated protocol errors do not trigger failover. If no account is available before any measured work, no credits are charged. Live account rotation still needs validation with your signed-in accounts.

## User memories

The authenticated **Memories** menu opens a resizable sidebar with an editable text area, Save and Reload. Saved notes belong to the application user across computers and provider accounts. They are stored in `user_memories` in the backend database, capped at 16000 UTF-8 bytes per user, and survive conversation-cache cleanup. Empty text clears memory. API routes are `GET /v1/memories` and `POST /v1/memories` with `{text,revision}`; stale revisions are rejected rather than overwriting concurrent edits.

New chats receive the current notes; subsequent turns carry their revision and use `memory_read` to fetch external edits or recover notes omitted by compaction. The memory policy stays in base instructions without rewriting them on every note edit, preserving provider prompt caching. The assistant can call `memory_read` and `memory_save` to remember useful preferences and verified task outcomes, consolidate notes, or honor requests to forget. These tools only access the authenticated user's memory text; they cannot execute programs or access the backend filesystem. They work independently of local Full access. Small updates append short notes instead of rewriting the whole memory. Consolidation runs within the ordinary billed AI turn, without separate background AI jobs. Native Codex account-wide memories stay disabled to prevent different application users sharing a memory store.

In the terminal, **F10** opens/closes Memories, **Ctrl+S** saves and **Ctrl+R** reloads. The prompt draft is preserved while editing memory. Updating the backend creates the memory table automatically. Start a new chat after this feature update so its native Codex thread advertises the new memory tools; existing threads keep their original tool registrations.

## Backend storage and shutdown

Conversation data is a cache, not a permanent server-side archive. Maintenance runs every five minutes:

- Inactive conversations expire after seven days. Application conversation/replay/tool payloads share a 256 MiB budget; older inactive chats can expire sooner when it is exceeded. The local client transcript is unaffected; an expired server conversation requires a new chat.
- Completed-response and steering replay payloads expire after 24 hours. Small request identities remain so retries cannot charge the same task twice. Quota snapshots retain 30 days; usage totals, token records, users, credits, payments, settings and AI credentials are retained.
- Native Codex conversation caches share a 1 GiB budget across all accounts and expire after seven days. Unreferenced interrupted threads are removed after a one-minute safety interval. Cleanup uses `thread/delete`, including its SQLite history. Native diagnostic logs retain at most seven days and target 16 MiB per account. An evicted native thread can recover from the application checkpoint and recent messages.
- Already checkpointed message bodies and redundant historical image pixels are not kept in application recovery history. Original image quality is unchanged. Native threads still retain the input needed to resume while they remain cached.
- Active turns are never evicted. Budgets are cleanup targets, not hard disk quotas: active inputs can temporarily exceed them. Durable accounting metadata continues to grow with actual usage. SQLite free pages are reused/reclaimed and WAL files are checkpointed.

The Linux installer puts the binary in `/usr/local/lib/linux-assistant/` and creates `codex-code-mode-host` beside it as a symlink to `assistant-server`. `/usr/local/bin/assistant-server` is also a symlink to that one binary. All account workers and the tool host then execute the original binary directly, with no runtime copies. Running without this installation layout uses one shared runtime image; a service unable to hard-link the executable copies it once for the whole backend, rather than once per account. Startup removes abandoned root/account `runtime-*` directories and automatically generated `assistant.db.before-conversation-reset-*` files. Credentials in `auth.json` are preserved.

SIGTERM and Ctrl+C stop admission, interrupt active AI turns, drain their final usage notifications, settle recorded charges and close account workers before exit. The systemd unit sends SIGTERM to the main process first and allows 45 seconds before killing remaining processes. Linux workers also terminate if their backend parent dies unexpectedly.

Updates, conversation resets and client chat resets no longer create automatic backups. Manual Admin backup/restore remains available; database backups are the operator's responsibility.

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

The updater fetches the official release, updates the submodule, Rust version and upstream dependency patches, seeds Cargo's lockfile from upstream's tested dependency graph, then builds and runs the application and integration tests. It tests actual embedded-runtime startup without credentials and fixture-based account selection, thread persistence, failover, user isolation and parallel execution. An upgrade is reported as verified only after all checks pass. Run it in a clean checkout, review the changes and commit the submodule pin, manifests, toolchain, and lockfile together. Run validation on both Linux and Windows before deploying an upgrade. Updates are deliberate build-time changes, not downloads into a running production backend.

Upstream Rust APIs and dependency constraints can change. A failing upgrade may require adjusting the isolated adapter or a dependency before rebuilding; the script leaves the candidate changes available for inspection. Do not deploy an unverified update. Roll back using the previous application commit and `git -c core.longpaths=true submodule update --init`. Use `assistant-server --codex-version` to inspect a built executable.

The upstream Apache 2.0 license and NOTICE are preserved in the submodule and embedded in the backend; `assistant-server --third-party-notices` prints them. See [third-party provenance](THIRD_PARTY_NOTICES.md).

## Operator dashboard and accounting

Model and reasoning are editable in **Admin → Settings → AI and token prices**. Reasoning names are `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` and `persistent`; the selected model must support the chosen level. Leave blank for the provider default. These controls are operator-only; clients cannot select models or reasoning.

Visit `/admin` and use the password created during first-run setup. The embedded dashboard provides account quotas, per-account tests, token charts and costs, a credit ledger, user management, session revocation, Stripe Checkout links and backend settings. Rotate the administrator password in Settings to invalidate other admin sessions.

The admin **Accounts → Add account** panel supports ChatGPT device-code sign-in and OpenAI API keys. ChatGPT sign-in displays the official OpenAI link and a code; complete it in your browser and the account becomes available automatically. API keys are stored by the embedded Codex server and are never returned by admin APIs. New account homes live in `managed-accounts/` beside `ASSISTANT_DATABASE`; back up that private directory with your existing account homes. They are loaded automatically on restart, so no environment-file editing is required. The backend may start without account homes when an admin password is configured, allowing the first account to be added through the dashboard. Use a short account test to verify actual model access after saving an API key.

Every account has Pause/Resume controls. Pauses persist in the application database, block new turns and manual tests, and allow running turns to finish. A later turn in a pinned conversation can move to another enabled account using the existing recovery checkpoint. When both authentication types are enabled, available ChatGPT subscription accounts take priority; API accounts are a fallback. To prefer API access, pause the ChatGPT accounts and resume an API account. API usage is separately billed by OpenAI, requires an API-accessible configured model, and does not have a ChatGPT subscription quota percentage. Normal token accounting and application credit deductions still apply. Account creation, sign-in codes, cancellation and pause mutations require the existing admin session and browser write protections. Live credential setup is disabled in development mode.

Each card also has **Delete account** or **Delete API key**. Pause busy accounts and wait for their active turns to finish before deleting. Removal stops the account worker, deletes its managed credential directory, and detaches saved chats so they can recover on another account; conversation messages, usage and credit history are kept. Removed credentials stay removed after restart. API cards show all-time input, cached input and output tokens recorded by this backend, plus an estimated token cost at the current configured rates. No documented endpoint exposes the remaining prepaid balance with a normal API key; the card links to OpenAI billing. Usage elsewhere is outside this local estimate.

**Admin → Settings → Backup and restore** downloads one JSON containing all saved settings, SMTP/OAuth/Stripe secrets, OpenAI API keys, ChatGPT authentication files and account pause settings. The administrator password/hash is excluded. Restore replaces saved configuration and AI accounts while preserving the current admin password, users, application sessions, credits, usage and conversation history. Active AI turns and pending account sign-ins must finish or be cancelled first. Files are staged and rolled back if applying settings fails; restored credentials are refreshed immediately. Keep backups private. When restoring on a different hostname, edit `settings.public_url` in the JSON to the destination's URL before uploading it. This settings backup is separate from a full database/history backup.

The **Diagnostics** section at `/admin#diagnostics` shows backend errors, quota-refresh failures and recovery, account-selection exclusions, provider error codes, and worker warnings/exits. Client AI failures include a request reference that can be searched here. Entries refresh every ten seconds without querying ChatGPT, preserve expanded details, and can be downloaded as JSON Lines. `/admin/api/logs` provides the same protected data with optional `search`, `level` (`info`, `warning`, `error`), and `before` cursor parameters; each page has at most 100 entries and a `next_before` cursor. Both views require the existing admin login; no diagnostic route is public or accessible with an ordinary user token.

Diagnostics persist in a SQLite sidecar beside `ASSISTANT_DATABASE`: `assistant.db` produces `assistant.logs.db`. Retention is limited to the latest 10,000 entries from 30 days, with about 8 KiB per message. The sidecar is independent of conversation transactions so database errors can also be logged. Known configured passwords/secrets and common credential formats are redacted before persistence and journal output. Request bodies, prompts, attachments and raw provider protocol payloads are not intentionally logged; upstream error messages can still mention input, so access remains restricted. Only warning/error message text is retained from structured worker stderr, without its attached fields. Diagnostics begin after deploying this version; old journal entries cannot be reconstructed. No database reset or extra configuration is needed.

Dashboard filters take effect with Apply; automatic refresh preserves filter drafts, account test prompts and unchanged expanded records. Detailed breakdowns and activity are expandable, and tables scroll independently on narrow screens. User access distinguishes blocked accounts from accounts that need credits. Display refreshes still use cached quotas; provider quota checks remain at five-minute intervals.

Usage breakdowns show input, cached-input, output and total costs in USD at the backend's current configured token rates, using 100 credits per dollar. Restarting with changed rates reprices these estimates without changing recorded credit deductions. Input cost excludes cached input, and output already includes reasoning. Missing token measurements are not estimated; mixed totals include only measured usage. These admin estimates have no task minimum or credit rounding.

ChatGPT quota polling runs every five minutes. Browser refreshes read cached state; normal provider selection also caches quota reads for five minutes, except confirmed quota failures. Busy workers are not interrupted by monitoring. Cached readings show their timestamp and errors rather than reporting unknown quota as zero.

Each real provider attempt records its account, request, conversation, purpose, model, timestamps, completion/error and available token counters. Failed attempts, failover and administrator tests are included. Cumulative token snapshots are persisted alongside usage records and differenced against the preceding turn, even when a warm thread resume omits old token notifications. Duplicate or older notifications are not summed. Start a new chat after deploying this change: an existing provider thread without a recorded baseline is rejected before starting a new AI turn, rather than silently undercharging it. Resumed history is not charged again in reports. Missing counters remain unknown, and interrupted attempts may be partial. Historical token usage from before this instrumentation is not backfilled. Cached input is included in input and reasoning is included in output. Application credits and subscription quota percentages remain separate quantities.

Token data is recorded for all normal real requests, even when the dashboard is disabled. Quota-history polling runs only when the dashboard is enabled. Admin tests use ephemeral threads, no tools, and no end-user credit charge. Their prompts/replies are shown to the operator but only usage metadata is retained by the application. This does not change provider-side data retention.

All dashboard APIs require a separate eight-hour administrator session; normal user tokens cannot grant access. Password changes invalidate existing sessions, mutations require a same-origin custom header, and cookie/response policies prevent cross-site embedding and caching. Credit changes require a reason and a stable idempotency key. Balance and Stripe fulfillment continue to use the authoritative ledger.

Protocol reference: [Codex app-server](https://learn.chatgpt.com/docs/app-server). Integration references: [Google OpenID Connect](https://developers.google.com/identity/openid-connect/openid-connect), [Stripe webhook verification](https://docs.stripe.com/events/manage-webhook-endpoints), and [Google app passwords](https://support.google.com/accounts/answer/185833).

## Build the Linux client executable

On a glibc-based Linux distribution, install Rust through rustup and the platform's C/C++ build tools, then run this from the Git checkout:

```sh
./scripts/build_client.sh
```

The script uses the host's default Rust target and initializes the pinned Codex submodule if missing, because Cargo reads the backend's workspace manifests even for a client-only build. It compiles only the client and its dependencies; Codex is not linked into the client. Musl tools are not needed.

Distribute `target/release/linux-assistant`. Browser HTML, JavaScript, and CSS are compiled into it. TLS uses Rustls and bundled trust roots, so the app does not require OpenSSL. The executable uses system libraries, including glibc; build on the oldest distribution you intend to support to avoid requiring a newer glibc. A separate build is needed for another CPU architecture.

Cross-compilation from Windows to x86-64 Linux can use Zig and `cargo-zigbuild`:

```sh
rustup target add x86_64-unknown-linux-gnu
cargo zigbuild --release --locked --target x86_64-unknown-linux-gnu -p linux-assistant --bin linux-assistant
```

The cross-compiled executable is at `target/x86_64-unknown-linux-gnu/release/linux-assistant`. On this Windows workspace, an isolated Rust toolchain and Zig were installed in ignored `.tools/`; no system PATH changes were made. Builds and tests are run manually; no GitHub Actions build workflow is configured.

## Build and run on Windows

Install Rust with the MSVC toolchain and Visual Studio C++ Build Tools, then use PowerShell:

```powershell
python scripts/cargo_backend.py test --workspace --locked
python scripts/cargo_backend.py build --release --locked --workspace
.\target\release\assistant-server.exe --dev
# In another terminal:
.\target\release\linux-assistant.exe --backend http://127.0.0.1:8787 --cli
# Or launch browser mode:
.\target\release\linux-assistant.exe --backend http://127.0.0.1:8787 --desktop
```

Distribute `target/release/linux-assistant.exe`. The repository's `.cargo/config.toml` statically links the MSVC runtime; only normal Windows system libraries and Windows PowerShell are needed. Backend accounts, credits, and settings work identically in both client modes. The `--install-desktop` option creates a Linux freedesktop launcher and is Linux-only.

Run the optional provider protocol fixture on Windows with:

```powershell
python scripts/cargo_backend.py build -p assistant-server --bin assistant-server --example provider-fixture --locked
$env:ASSISTANT_TEST_PROVIDER="$PWD/target/debug/examples/provider-fixture.exe"
$env:ASSISTANT_TEST_SERVER="$PWD/target/debug/assistant-server.exe"
python scripts/cargo_backend.py test -p assistant-server --locked -- --ignored
```

## Tests

```sh
python scripts/cargo_backend.py test --workspace --locked
python scripts/cargo_backend.py build -p assistant-server --bin assistant-server --example provider-fixture --locked
ASSISTANT_TEST_PROVIDER="$PWD/target/debug/examples/provider-fixture" \
  ASSISTANT_TEST_SERVER="$PWD/target/debug/assistant-server" \
  python scripts/cargo_backend.py test -p assistant-server --locked -- --ignored
```

Tests cover ledger idempotency and crash recovery, insufficient credits, device-session consumption, email-code attempt limits, session revocation, payment signature validation and replay, account rotation over JSONL, local Origin/Host/token checks, HTTPS enforcement, OS-specific proposals, and actual execution through the approval flow. Process tests check live output and stopping while the conversation is locked, saved results, Unicode-safe output truncation, results staying in the main conversation, provider thread isolation, and restart/retry without command reexecution. Windows tests also check Unicode and quoting, working directories, PowerShell/native failures, both output caps, and descendant cleanup on Stop and cancellation. Live credentials are unnecessary for these tests.

Memory tests cover user isolation, simultaneous saves, stale revisions, clearing, UTF-8 size limits, consolidation, persistence across cache cleanup, and edits during a running client task. The provider fixture also verifies automatic saves and resumed threads using two users on one AI account. Cache tests protect a native thread while its next turn is starting.

Native Linux execution must be checked on a Linux host because this Windows development host has no Linux runtime. The runtime contract test uses a fresh unsigned-in account directory and a local HTTP model stub to verify real App Server turns and thread resumption without contacting an AI provider. Protocol fixtures test quota behavior without using real subscriptions.

SMTP tests use a local TLS server to check authentication, configurable ports and senders, Unicode recipients, code delivery and verification, authentication failures, and rejection of untrusted certificates. They send no real email and require no mail account credentials.

Backend validation includes real embedded-runtime token counters, resumed threads, duplicate usage events, quota caching, account-specific tests, authentication, credit adjustment idempotency, filtering, and user access controls. Browser checks cover desktop/mobile layout, login, credit adjustment, blocking/unblocking, user filtering, and escaped international text. Earlier client checks covered live PowerShell output, stopping while busy, executable arguments, stderr, and stdout downloads. The embedded backend imports only Windows system DLLs; a separate Codex executable is not needed. Production email login and a real AI reply were verified on ai.slax.org before this dashboard update; live Stripe payments and multi-account quota exhaustion have not been tested with real credentials.

The existing backend is deployed on ai.slax.org; local source changes still require a server build and installation. Google and Stripe require their own configuration. The client UI remains a functional preview pending the separate design decision.

Conversation protocol validation also covers transcripts above the old 96 KB limit, checkpoint-based account failover, idempotent steering, native Codex compaction and interruption, queued-message crash recovery, and stopping real PowerShell commands from the task manager. The database reset test checks preserved users, sessions, credits, purchases, and usage without creating a backup. Browser checks exercise queue/steer/Stop, paused queues after interruption, mobile layout, and dark mode with a local simulated backend; these do not consume real AI usage.
