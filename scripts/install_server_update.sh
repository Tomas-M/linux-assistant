#!/bin/sh
# Install the backend and its config-free systemd unit on an existing root-managed deployment.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
test "$(id -u)" -eq 0
test -x target/release/assistant-server

# Stop before copying SQLite and its WAL so the backup is consistent.
systemctl stop linux-assistant.service
trap 'systemctl start linux-assistant.service' EXIT
backup="/var/backups/linux-assistant-$(date +%Y%m%d-%H%M%S)"
install -d -m 700 "$backup"
cp -p /usr/local/bin/assistant-server "$backup/assistant-server"
for suffix in '' '-wal' '-shm'; do
    file="/var/lib/linux-assistant/assistant.db$suffix"
    if [ -f "$file" ]; then cp -p "$file" "$backup/"; fi
done

# No configuration import: settings are entered in the admin dashboard.
install -m 755 target/release/assistant-server /usr/local/bin/assistant-server.new
mv /usr/local/bin/assistant-server.new /usr/local/bin/assistant-server

install -m 644 deploy/linux-assistant.service /etc/systemd/system/linux-assistant.service
systemctl daemon-reload
systemctl start linux-assistant.service
trap - EXIT
printf 'Backend installed. Private backup: %s\n' "$backup"
printf '%s\n' 'Open https://ai.slax.org/admin. On first run, set the password and enter backend settings manually.'
