#!/bin/sh
# Install the backend and its config-free systemd unit on an existing root-managed deployment.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
test "$(id -u)" -eq 0
test -x target/release/assistant-server

# SIGTERM drains active turns and closes workers before the executable is replaced.
systemctl stop linux-assistant.service
trap 'systemctl start linux-assistant.service' EXIT
# No configuration import: settings are entered in the admin dashboard.
# Keep the helper in our own directory, avoiding collisions with a separately installed Codex.
install -d -m 755 /usr/local/lib/linux-assistant
install -m 755 target/release/assistant-server /usr/local/lib/linux-assistant/assistant-server.new
mv /usr/local/lib/linux-assistant/assistant-server.new /usr/local/lib/linux-assistant/assistant-server
ln -sfn assistant-server /usr/local/lib/linux-assistant/codex-code-mode-host
ln -sfn ../lib/linux-assistant/assistant-server /usr/local/bin/assistant-server

install -m 644 deploy/linux-assistant.service /etc/systemd/system/linux-assistant.service
systemctl daemon-reload
systemctl start linux-assistant.service
trap - EXIT
printf '%s\n' 'Backend installed. No automatic backup was created.'
printf '%s\n' 'Open https://ai.slax.org/admin. On first run, set the password and enter backend settings manually.'
