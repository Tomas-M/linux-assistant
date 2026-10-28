# Server source bundle

## Required reset for the conversation protocol update

This build replaces full-history requests with new-message requests and adds steering/interruption. Update both backend and client. Existing conversation/request tables must be reset; no migration or compatibility path is included.

```sh
# Build the new binary before stopping the running service.
cd /var/www/html/linux-assistant
sh scripts/build_server.sh

# Stop the backend, back up its database, and clear only conversation/request state.
# Users, sign-in sessions, credits, purchases, usage records and ChatGPT account logins remain.
# Pending request credits are refunded once. The service stays stopped until installation.
sh scripts/reset_conversations.sh

# Install the new backend; it recreates the conversation tables with the new schema.
sh scripts/install_server_update.sh

# Verify startup. Then test a streamed conversation, steering, and Stop through Apache.
curl --fail https://ai.slax.org/health
```

The reset script defaults to `/var/lib/linux-assistant/assistant.db`; pass a different database path as its first argument if needed. Its backup contains private account data and is created beside the database with mode 600. Existing provider rollout files are retained, but the new application conversation state does not resume them.

Stop the local assistant through Settings → Stop local assistant (closing its browser is insufficient). Run the **new** executable once with `--reset-chat`, then launch normally or with `--desktop`. The explicit reset preserves sign-in and settings, backs up local state, and clears only the chat, pending approvals/requests, and queued input. It refuses to reset while a daemon is running.

```powershell
# Windows: after stopping the old local assistant, reset its chat for the new protocol.
.\linux-assistant.exe --reset-chat
# Start the new client and open its browser.
.\linux-assistant.exe --desktop
```

```sh
# Linux: after stopping the old daemon, reset the chat and print the new local URL.
./linux-assistant --reset-chat
./linux-assistant
```


Upload only `linux-assistant-server-source.tar.gz`. This is a source bundle to build on your Linux server. It contains the application sources, locked dependencies, and deployment files. It excludes `.tools`, `vendor`, `target`, Git history, databases, and account credentials.

The build downloads the exact Codex commit recorded in `CODEX_REVISION` from the official repository, then compiles Codex into `assistant-server`. There is no separate Codex installation. Internet access and substantial free disk space are needed for the first build; the small archive does not make compilation small. Build as your normal user. Two compilation jobs are used by default; use `CARGO_BUILD_JOBS=1 sh scripts/build_server.sh` to reduce parallel memory use if needed.

## 1. Build on Ubuntu or Debian

Extract into a fresh directory:

```sh
tar -xzf linux-assistant-server-source.tar.gz
cd linux-assistant
sudo apt-get update
sudo apt-get install -y build-essential git curl ca-certificates pkg-config libssl-dev libcap-dev cmake
```

If Rust/rustup is not already installed, install it as your normal user:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/linux-assistant-rustup.sh
sh /tmp/linux-assistant-rustup.sh -y --profile minimal --default-toolchain none
. "$HOME/.cargo/env"
```

Build the backend. Rustup selects the version in `rust-toolchain.toml` automatically:

```sh
sh scripts/build_server.sh
sudo install -m 755 target/release/assistant-server /usr/local/bin/assistant-server
```

After installation, this source directory and downloaded build dependencies are not needed to run the backend. Keep the installed executable, configuration, and private data directory. Build future updates in a fresh directory from a newly generated archive; the repository's Codex updater is run in the development Git checkout, not in this bundle.

## 2. Create the service user and sign into your ChatGPT accounts

On a new server:

```sh
sudo useradd --system --user-group --create-home --home-dir /var/lib/linux-assistant --shell /usr/sbin/nologin linux-assistant
sudo chmod 700 /var/lib/linux-assistant
sudo install -d -m 750 -o root -g linux-assistant /etc/linux-assistant
```

Sign in separately for each account you want to use:

```sh
sudo -H -u linux-assistant /usr/local/bin/assistant-server --login-account /var/lib/linux-assistant/account-1
sudo -H -u linux-assistant /usr/local/bin/assistant-server --login-account /var/lib/linux-assistant/account-2
```

Open each printed URL on your own computer, enter its code, and sign into the intended ChatGPT account. Wait for the successful save message before continuing. These directories contain private login credentials. Add more accounts with the same command and another directory name.

## 3. Configure the backend

```sh
sudo install -m 600 deploy/server.env.example /etc/linux-assistant/server.env
sudo nano /etc/linux-assistant/server.env
```

Set these values to your public domain and SMTP account. The host, port, username, password, and sender are all configurable:

```ini
ASSISTANT_LISTEN=127.0.0.1:8787
ASSISTANT_PUBLIC_URL=https://assistant.example.com
ASSISTANT_DATABASE=/var/lib/linux-assistant/assistant.db
ASSISTANT_FREE_CREDITS=25
ASSISTANT_CODEX_HOMES=/var/lib/linux-assistant/account-1
ASSISTANT_SMTP_HOST=smtp.gmail.com
ASSISTANT_SMTP_PORT=465
ASSISTANT_SMTP_USERNAME=tomas@slax.org
ASSISTANT_SMTP_PASSWORD=replace-on-server
ASSISTANT_EMAIL_FROM="Linux Assistant <tomas@slax.org>"
```

SMTP uses implicit TLS from connection start with certificate validation. Enter the hostname without `ssl://` or `smtps://`; port 465 is the default. Port 587 with STARTTLS is not supported by this configuration. For Gmail, put the account's [app password](https://support.google.com/accounts/answer/185833) in `ASSISTANT_SMTP_PASSWORD`; app passwords require 2-Step Verification and availability depends on the account's settings. Keep this password only in `/etc/linux-assistant/server.env` with mode `600`. No external mail-sending service is required.

Remove the example `GOOGLE_*` and `STRIPE_*` lines until you configure those services. Working SMTP credentials and a sender address are required in real mode. One ChatGPT account is sufficient; list only directories you have signed into. The ChatGPT operator logins above are separate from users signing into this application by email or Google. Leave `ASSISTANT_MODEL` unset to use the provider's default model. Do not add `--dev` to the service: that mode uses simulated AI.

Point your domain to this server and configure your HTTPS reverse proxy to forward to `127.0.0.1:8787`. For an existing Caddy installation, the site block is:

```caddyfile
assistant.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

Use your actual domain. Make ports 80 and 443 reachable for HTTPS setup. Keep the backend port on loopback.

## 4. Start the service

```sh
sudo install -m 644 deploy/linux-assistant.service /etc/systemd/system/linux-assistant.service
sudo systemctl daemon-reload
sudo systemctl enable --now linux-assistant
curl --fail http://127.0.0.1:8787/health
```

The health response should be `{"ok":true}`. Check failures with `sudo journalctl -u linux-assistant -n 100 --no-pager`. Set the backend URL in the client to your HTTPS domain.

To add another account later, sign into a new account directory, append its absolute path to `ASSISTANT_CODEX_HOMES`, and run `sudo systemctl restart linux-assistant`. New chats select an available account with the lowest reported usage and stay assigned to it. Exhaustion can trigger a new provider thread on another account with history replay, which consumes additional usage.

See `README.md` for account selection, application credits, Google/Stripe setup, and known implementation limits. Its Git/submodule and development update commands apply to the full development checkout; use this guide for the source bundle.

## Operator dashboard on an existing ai.slax.org deployment

Run as root after extracting the updated source bundle into `/var/www/html/linux-assistant`:

```sh
# Build first while the existing backend continues serving users.
cd /var/www/html/linux-assistant
sh scripts/build_server.sh

# Back up the installed binary, private settings, database and WAL, then install and restart.
# This script uses the standard /var/lib/linux-assistant/assistant.db deployment path.
# Existing users, credits, conversations and ChatGPT sign-ins are preserved.
sh scripts/install_server_update.sh

# Read the generated dashboard password locally. Keep it private.
grep '^ASSISTANT_ADMIN_PASSWORD=' /etc/linux-assistant/server.env

# Check that the updated service is serving requests.
curl --fail https://ai.slax.org/health
```

Open `https://ai.slax.org/admin` and enter that password. Apache's existing catch-all proxy already forwards `/admin`; no additional virtual host is needed. The HTML, JavaScript and CSS are embedded in the backend executable.

`ASSISTANT_ADMIN_PASSWORD` enables the dashboard; omit it to disable access. It must contain at least 16 bytes. The installer generates 32 random bytes encoded as hex when the setting is absent. Changing it and restarting the service invalidates old admin sessions. Login cookies expire after eight hours and are HttpOnly, Secure in production, and SameSite=Strict. Admin credentials are never passed into Codex workers.

Account quota checks run every five minutes; the display refreshes cached readings every ten seconds. Busy accounts retain their last reading with its timestamp until the worker is available. Regular selection uses the same five-minute quota cache; a typed quota failure can force an immediate refresh and account switch. Quota history is recorded while dashboard monitoring is enabled. These percentages cover the provider account, including use outside this service.

Token accounting starts with this build. Reports show input, cached input, output, reasoning and total tokens, grouped by time, account, purpose and model, with individual attempts and failures. Cached input and reasoning are subsets, not extra tokens. Missing measurements remain unknown; interrupted turns may be partial. Reports cover this application's measured requests, not other activity on the ChatGPT account. Existing credit ledger entries remain available; old token totals cannot be reconstructed from the previous application database.

Per-account tests use short-lived provider threads, with no tools or local execution. They stay on the selected account, consume provider allowance, and are recorded separately without a user credit deduction. User controls support email search, balances, recorded usage, blocking, session revocation and signed credit adjustments with a required reason. Repeating the same adjustment after a network error is idempotent.

## Streaming client update

Deploy the new backend before using the rebuilt client. No new environment variables or database reset are needed. The client requests SSE from the existing `/v1/chat` route; authentication, quota selection, credit reservations and usage reporting keep their existing behavior. Output analysis continues to use the JSON representation.

```sh
# Run from the extracted update directory to build the pinned backend sources.
cd /var/www/html/linux-assistant
sh scripts/build_server.sh

# Back up the installed binary, settings and database, then install and restart.
sh scripts/install_server_update.sh

# Confirm the service is available after the update.
curl --fail https://ai.slax.org/health
```

The existing Apache catch-all proxy forwards the same route. The backend sends ten-second heartbeats during idle generation. Check a real streamed conversation through HTTPS after deployment; a health response alone does not verify streaming or rule out buffering by another proxy layer.

Stop the old local assistant through its UI or CLI `/stop` before replacing or launching the rebuilt client. Simply closing the browser leaves the old daemon running. A normal launch now prints its complete localhost URL (including the private fragment); `--desktop` opens the default browser and `--cli` enters the terminal prompt. Browser refreshes reconnect to ongoing work without repeating it.

## Model and reasoning settings

```sh
# Edit the private backend environment without replacing its other settings.
nano /etc/linux-assistant/server.env

# Add or change this line in the file (without the leading #):
# ASSISTANT_REASONING_EFFORT=medium
# Optionally set ASSISTANT_MODEL to a model ID available to your connected accounts.

# Restart after saving to apply the new setting to subsequent AI turns.
systemctl restart linux-assistant.service
```

Accepted reasoning names are `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`, and `persistent`. Choose a value supported by the model; accepting the name at startup does not guarantee that every model supports it. The setting applies to conversations and dashboard account tests. It overrides the effort on each new turn even in resumed conversations. Omitting it preserves Codex's thread default, including a previous effort saved in an existing thread. There is no database change. See the [App Server turn configuration](https://learn.chatgpt.com/docs/app-server).

## Connecting Stripe and adding paid credits

Create a product with a **one-time Price** in your Stripe Dashboard. Its price is the amount charged; `ASSISTANT_PACK_CREDITS` is how many of this application's credits that payment buys. Use API credentials, Price and webhook from the same Stripe sandbox/test or live mode. Start in test mode.

In Stripe Workbench, create a **snapshot** webhook destination for your account at `https://ai.slax.org/webhooks/stripe`. Select `checkout.session.completed` and `checkout.session.async_payment_succeeded`. Copy its signing secret (`whsec_...`), which is different from the API secret key. See [Stripe keys](https://docs.stripe.com/keys) and [webhook setup](https://docs.stripe.com/webhooks).

```sh
# Edit the private server settings; keep your existing SMTP and account settings.
nano /etc/linux-assistant/server.env

# Set these four lines in that file, replacing placeholders with your own values:
# STRIPE_SECRET_KEY=sk_test_REPLACE_LOCALLY
# STRIPE_WEBHOOK_SECRET=whsec_REPLACE_LOCALLY
# STRIPE_PRICE_ID=price_REPLACE_LOCALLY
# ASSISTANT_PACK_CREDITS=100
# Remove the leading # when entering the actual settings.

# Reload settings after saving the file. This also refreshes the payment status in the dashboard.
systemctl restart linux-assistant.service
```

In `/admin`, find a user, choose **Manage → Create payment link**, and share that Checkout URL yourself. The user's identity and credit quantity are stored on the server before Checkout is created. Only a correctly signed webhook for a paid session adds credits; opening the success page does not. Duplicate events do not add credits twice. No client application or publishable Stripe key is needed for this dashboard flow. Switching to live payments requires all three live-mode settings together.

PayPal automation is not implemented. For a verified payment already received through PayPal or another channel, use **Manage → Credit adjustment**, enter a positive number and the payment transaction reference as the reason. Verify the payment yourself first. The transaction reference is an audit note, not an automatic PayPal verification or deduplication mechanism. Refunds and disputes are not automatically debited; use a documented negative adjustment. If the remaining balance is insufficient, the adjustment is rejected; you can block further use while resolving the payment.

Accounting data has no automatic deletion policy. Include the SQLite database and WAL in consistent backups. Admin tests retain usage metadata rather than prompt/response text; ordinary conversations retain their complete application transcript.
