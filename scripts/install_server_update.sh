#!/bin/sh
# Install an already-built backend on the existing root-managed systemd deployment.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
test "$(id -u)" -eq 0
test -x target/release/assistant-server
test -f /etc/linux-assistant/server.env

# Stop before copying SQLite and its WAL so the backup is consistent.
systemctl stop linux-assistant.service
trap 'systemctl start linux-assistant.service' EXIT
backup="/var/backups/linux-assistant-$(date +%Y%m%d-%H%M%S)"
install -d -m 700 "$backup"
cp -p /etc/linux-assistant/server.env "$backup/server.env"
cp -p /usr/local/bin/assistant-server "$backup/assistant-server"
for suffix in '' '-wal' '-shm'; do
    file="/var/lib/linux-assistant/assistant.db$suffix"
    if [ -f "$file" ]; then cp -p "$file" "$backup/"; fi
done

# New tables are created at startup; existing users, balances and account logins are retained.
install -m 755 target/release/assistant-server /usr/local/bin/assistant-server.new
mv /usr/local/bin/assistant-server.new /usr/local/bin/assistant-server

# Generate a password only when the dashboard has not already been configured.
if ! grep -q '^ASSISTANT_ADMIN_PASSWORD=' /etc/linux-assistant/server.env; then
    password=$(openssl rand -hex 32)
    printf '\nASSISTANT_ADMIN_PASSWORD=%s\n' "$password" >> /etc/linux-assistant/server.env
    unset password
fi
chmod 600 /etc/linux-assistant/server.env
systemctl start linux-assistant.service
trap - EXIT
printf 'Backend installed. Private backup: %s\n' "$backup"
printf '%s\n' 'Open https://ai.slax.org/admin. Read the password locally from /etc/linux-assistant/server.env; do not send that file to anyone.'
