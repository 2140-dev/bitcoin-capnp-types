#!/usr/bin/env bash
#
# Build Bitcoin Core, launch a regtest node with IPC enabled, run the
# integration test suite against it, and shut the node down.
#
# Usage: contrib/run-integration-tests.sh <path-to-bitcoin-source>

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <path-to-bitcoin-source>" >&2
    exit 1
fi

BITCOIN_SRC=$(cd "$1" && pwd)
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
BITCOIN_BIN="$BITCOIN_SRC/build/bin/bitcoin"

echo "==> Building Bitcoin Core in $BITCOIN_SRC"
(
    cd "$BITCOIN_SRC"
    cmake -B build -DENABLE_WALLET=ON -DBUILD_TESTS=OFF
    cmake --build build -j"$(nproc)"
)

stop_bitcoin() {
    if [ -n "${BITCOIN_STARTED:-}" ]; then
        echo "==> Stopping bitcoin"
        "$BITCOIN_BIN" rpc -chain=regtest stop || true
    fi
}
trap stop_bitcoin EXIT

echo "==> Starting bitcoin node (regtest, IPC)"
"$BITCOIN_BIN" node -chain=regtest -ipcbind=unix -server -debug=ipc -daemon
BITCOIN_STARTED=1

echo "==> Running cargo test"
(
    cd "$REPO_ROOT"
    BITCOIN_BIN="$BITCOIN_BIN" cargo test
)
