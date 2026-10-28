#!/bin/sh
# Reset application conversations for the new delta protocol; preserve business data and account logins.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
test "$(id -u)" -eq 0
command -v python3 >/dev/null
database="${1:-/var/lib/linux-assistant/assistant.db}"
test -f "$database"

# Leave the old backend stopped: only the new executable must recreate these tables.
systemctl stop linux-assistant.service
python3 scripts/reset_conversations.py "$database"
printf '%s\n' 'Install the new backend with: sh scripts/install_server_update.sh'
