#!/usr/bin/env bash
# Installs the pi-zero publisher as a systemd user service. Runs on the Pi.
#
# Usage, from a development machine:
#   scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo pi@livepizero:~/
#   scp -r demos/pi-zero/scripts pi@livepizero:~/
#   ssh pi@livepizero ./scripts/install-service.sh
#
# The secret key is generated once and kept across reinstalls, so the ticket
# survives them. Override the binary's location with PI_ZERO_DEMO_BIN.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${PI_ZERO_DEMO_BIN:-$HOME/pi-zero-demo}"
UNIT_SRC="$SCRIPT_DIR/pi-zero-demo.service"
UNIT_DST="$HOME/.config/systemd/user/pi-zero-demo.service"
ENV_DIR="$HOME/.config/pi-zero-demo"
ENV_FILE="$ENV_DIR/env"
UNIT="pi-zero-demo"

fail() {
    echo "error: $*" >&2
    exit 1
}

# --- checks ---------------------------------------------------------------

[ -f "$UNIT_SRC" ] || fail "$UNIT_SRC not found; copy the whole scripts directory over"

if [ ! -f "$BIN" ]; then
    fail "no binary at $BIN
Build and copy it first:
  cargo make cross-build-aarch64 -- -p pi-zero-demo --release
  scp target/aarch64-unknown-linux-gnu/release/pi-zero-demo $(whoami)@$(hostname):~/"
fi

[ -x "$BIN" ] || chmod +x "$BIN"

# Running it is the only check that catches a binary built for the wrong
# architecture or against a newer glibc than this Pi has.
if ! "$BIN" --help >/dev/null 2>/tmp/pi-zero-demo-check.$$; then
    reason="$(cat /tmp/pi-zero-demo-check.$$)"
    rm -f /tmp/pi-zero-demo-check.$$
    fail "$BIN did not run: ${reason:-no output}"
fi
rm -f /tmp/pi-zero-demo-check.$$

# A user manager is what runs the unit, and over SSH it is not always there.
systemctl --user show-environment >/dev/null 2>&1 ||
    fail "no systemd user manager for $USER (is XDG_RUNTIME_DIR set?)"

command -v rpicam-vid >/dev/null ||
    echo "warning: rpicam-vid is not on PATH; the publisher has no camera to read" >&2

# --- secret key -----------------------------------------------------------

install -m 700 -d "$ENV_DIR"

if [ -f "$ENV_FILE" ]; then
    echo "keeping the existing identity in $ENV_FILE"
else
    # 32 bytes of hex, which is what IROH_SECRET parses. Straight from
    # urandom so that openssl is not a requirement.
    secret="$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')"
    (
        umask 077
        cat >"$ENV_FILE" <<EOF
# The publisher's identity. Losing this line changes the ticket.
IROH_SECRET=$secret
# Extra arguments for the publisher, such as --epaper. Read the note on
# RestartSec in the unit before turning the e-paper display on.
PI_ZERO_DEMO_ARGS=
EOF
    )
    echo "generated a new identity in $ENV_FILE"
fi

# --- install --------------------------------------------------------------

install -Dm644 "$UNIT_SRC" "$UNIT_DST"
systemctl --user daemon-reload
systemctl --user enable --now "$UNIT"

# Lingering is what makes this start at boot rather than at login.
if [ "$(loginctl show-user "$USER" --property=Linger --value 2>/dev/null)" = "yes" ]; then
    echo "lingering is already enabled for $USER"
elif sudo -n true 2>/dev/null; then
    sudo loginctl enable-linger "$USER"
    echo "enabled lingering for $USER"
else
    echo "warning: could not enable lingering without a password. The service" >&2
    echo "will only start at login until you run:" >&2
    echo "  sudo loginctl enable-linger $USER" >&2
fi

# --- report ---------------------------------------------------------------

echo
systemctl --user --no-pager --lines=0 status "$UNIT" || true
echo
echo "waiting for the ticket ..."
ticket=""
for _ in $(seq 15); do
    ticket="$(journalctl --user -u "$UNIT" --since "-2min" --no-pager 2>/dev/null |
        grep -o 'iroh-live:[^ ]*' | tail -1)"
    [ -n "$ticket" ] && break
    sleep 1
done

if [ -n "$ticket" ]; then
    echo "$ticket"
    echo
    echo "watch it with: irl watch $ticket"
else
    echo "no ticket yet. Follow the log with:"
    echo "  journalctl --user -u $UNIT -f"
fi
