#!/bin/bash
#
# Build and install hztop (the operator dashboard) onto this machine:
#   /usr/local/bin/hztop          the binary (built from this checkout)
#   /etc/hztop/config.json        machine config (from deploy/hztop.config.json,
#                                 only written if absent -- edits are preserved)
#
# hztop's zero-arg config search is $HZTOP_CONFIG, ~/.config/hztop/config.json,
# /etc/hztop/config.json -- so after this script, any user can just run `hztop`.
#
# Run as root (or with sudo), from any directory. Re-run to upgrade. Requires
# cargo on the PATH of the invoking user.
set -eu

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_USER="${SUDO_USER:-$(id -un)}"

echo "==> Building hztop (release)..."
# Build as the invoking user so the target/ dir and cargo caches stay theirs.
su - "${BUILD_USER}" -c "cd '${REPO_DIR}' && cargo build --release -p hztop"

echo "==> Installing /usr/local/bin/hztop..."
# rm+cp (not cp in place) so a currently-running hztop keeps its old inode.
rm -f /usr/local/bin/hztop
cp "${REPO_DIR}/target/release/hztop" /usr/local/bin/hztop

if [ ! -f /etc/hztop/config.json ]; then
    echo "==> Installing /etc/hztop/config.json (edit to match this box)..."
    mkdir -p /etc/hztop
    cp "${REPO_DIR}/deploy/hztop.config.json" /etc/hztop/config.json
else
    echo "==> Keeping existing /etc/hztop/config.json"
fi

echo "==> Done. Run: hztop"
