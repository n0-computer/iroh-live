#!/usr/bin/env bash
set -euo pipefail

if [ $# -lt 2 ]; then
    echo "Usage: $0 <SSID> <PSK> [ROOTFS_PATH]"
    echo "  SSID        WiFi network name"
    echo "  PSK         WiFi password"
    echo "  ROOTFS_PATH mount point (default: /media/$USER/rootfs)"
    exit 1
fi

SSID="$1"
PSK="$2"
ROOTFS="${3:-/media/$USER/rootfs}"

CONN_DIR="$ROOTFS/etc/NetworkManager/system-connections"
CONN_FILE="$CONN_DIR/preconfigured.nmconnection"

if [ ! -d "$ROOTFS/etc/NetworkManager" ]; then
    echo "ERROR: $ROOTFS does not look like a Bookworm rootfs (no NetworkManager dir)"
    exit 1
fi

UUID=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())')

sudo mkdir -p "$CONN_DIR"

sudo tee "$CONN_FILE" > /dev/null <<EOF
[connection]
id=preconfigured
uuid=$UUID
type=wifi
autoconnect=true

[wifi]
mode=infrastructure
ssid=$SSID

[wifi-security]
auth-alg=open
key-mgmt=wpa-psk
psk=$PSK

[ipv4]
method=auto

[ipv6]
addr-gen-mode=default
method=auto

[proxy]
EOF

sudo chmod 600 "$CONN_FILE"
sudo chown root:root "$CONN_FILE"

echo "Done. Wrote: $CONN_FILE"
echo "SSID: $SSID"
echo "Permissions: $(stat -c '%a %U:%G' "$CONN_FILE")"
