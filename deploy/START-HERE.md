# Backend deployment

The backend is one executable. Application configuration is entered in the web admin and saved in `/var/lib/linux-assistant/assistant.db`. Linux listens on `127.0.0.1:8787`; Apache provides public HTTPS. No `/etc/linux-assistant/server.env` file is read or required.

## Build

Upload `dist/linux-assistant-server-source.tar.gz`, or use the Git checkout. The bundle excludes tools, credentials, build output and vendored Codex; the build script downloads the exact pinned Codex commit. The first build needs internet access and substantial free disk space.

Run as root on Debian/Ubuntu:

```sh
# Install system build dependencies once.
apt-get update
apt-get install -y build-essential git curl ca-certificates pkg-config libssl-dev libcap-dev cmake

# Inside the extracted source directory, build the backend.
sh scripts/build_server.sh
```

Install Rust/rustup before building if it is not already available. `rust-toolchain.toml` selects the required version. The build defaults to two jobs; use `CARGO_BUILD_JOBS=1 sh scripts/build_server.sh` to reduce memory pressure.

## Fresh service installation

```sh
# Create the service account and private data directory once.
useradd --system --user-group --create-home --home-dir /var/lib/linux-assistant --shell /usr/sbin/nologin linux-assistant
install -d -m 700 -o linux-assistant -g linux-assistant /var/lib/linux-assistant

# Install the executable and config-free service unit.
install -m 755 target/release/assistant-server /usr/local/bin/assistant-server
install -m 644 deploy/linux-assistant.service /etc/systemd/system/linux-assistant.service
systemctl daemon-reload
systemctl enable --now linux-assistant.service

# Check that the backend is accepting local requests.
curl --fail http://127.0.0.1:8787/health
```

## Apache and first-run setup

Point the HTTPS virtual host for `ai.slax.org` at the backend. Keep `ProxyPreserveHost On` during setup so the backend sees the public hostname. A minimal proxy section is:

```apache
ServerName ai.slax.org
ProxyPreserveHost On
ProxyPass / http://127.0.0.1:8787/ timeout=900
ProxyPassReverse / http://127.0.0.1:8787/
```

Use your existing TLS certificate and HTTPS virtual host. Enable Apache's proxy modules if needed, validate its configuration and reload it.

1. Open **https://ai.slax.org/admin** immediately after starting a fresh backend. Until a password exists, anyone reaching setup can claim the administrator account. Operational admin APIs stay unavailable until setup is complete.
2. Enter the administrator password twice, using at least 16 characters. Only a salted PBKDF2 hash is stored. After setup, admin sign-in is required.
3. In **Settings → General**, confirm **Public URL** is `https://ai.slax.org` and choose the welcome credit grant.
4. In **AI and token prices**, choose the model, optional reasoning level and input/cached input/output rates per million tokens. Saved rates apply to new tasks immediately; active tasks retain their original rates.
5. In **Email and SMTP**, set hostname, port, username, password and sender. Gmail uses `smtp.gmail.com`, port `465`, and a Google app password. Enable email sign-in when ready.
6. Configure and enable Google, Microsoft and GitHub sign-in as desired. Each section includes a short guide and exact callback URL. See [SOCIAL_SIGN_IN.md](SOCIAL_SIGN_IN.md) for full registration instructions. SMTP is also used for first-time ownership checks on some social accounts.
7. Click **Save settings**. Saved secrets are never displayed; blank fields preserve them. Explicit clear checkboxes remove saved secrets.
8. Under **Accounts → Add account**, sign into ChatGPT using the displayed device code or paste an OpenAI API key. Credentials are saved in `/var/lib/linux-assistant/managed-accounts/<UUID>/auth.json`. Pause/resume controls determine which accounts can accept new turns. No account-directory list or separate Codex installation is needed.
9. Use the per-account **Run test** action to verify AI access.

User sign-in methods start disabled on a fresh backend. Enable the ones you configure. Reload the local client page to refresh its buttons.

## Stripe

1. In Stripe, create one **one-time USD Price** per credit pack. Several Prices may belong to the same product.
2. In admin **Settings → Stripe**, enter the Stripe secret API key and webhook signing secret. Under **Credit packs**, click **Add credit pack** for each option and enter its `price_...` ID, USD price and credit amount. For example: 100 credits / $2.99, 200 / $5.49, and 500 / $9.99.
3. Register `https://ai.slax.org/webhooks/stripe` for `checkout.session.completed` and `checkout.session.async_payment_succeeded`.
4. Copy its signing secret into **Webhook signing secret** and save. Keys, Price and webhook must belong to the same Stripe mode.

Paid, signed webhook events grant credits once. Checkout redirects never grant credits. Refunds and disputes are not automated. The client shows one purchase button for each configured pack, side by side in Account & settings. Add or remove rows in admin to change the choices; reopening client Settings fetches the current list. The USD amount is for display and must be kept correct by the operator. Stripe Checkout charges the selected Price ID. The backend captures the selected credit amount in the purchase, so later catalog edits cannot change an already created checkout.

## Updates and backups

```sh
# Build before stopping the service.
sh scripts/build_server.sh

# Back up the stopped database and previous binary, install the executable/unit, and restart.
sh scripts/install_server_update.sh

# Verify the public backend through Apache.
curl --fail https://ai.slax.org/health
```

Existing database settings are read directly. There is no old-environment import or compatibility mode. If no administrator password is stored, complete setup and enter settings manually. The update script replaces the systemd unit so it no longer references an environment file.

Back up the database with SQLite's online backup mechanism, or stop the service while copying the database and WAL. Back up `managed-accounts/` separately to preserve ChatGPT sessions and API keys. These files contain private credentials. Remote diagnostics are available through the password-protected admin.
