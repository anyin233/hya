#!/usr/bin/env bash
# Authoritative zero-HTTP gate: trace the headless native_round_trip test binary under strace and
# assert it opens NO inet socket. Uses force_offline (in the test) so the backend makes no provider
# calls; a correct native turn therefore emits zero socket/bind/connect/listen/accept on inet fds.
set -euo pipefail

command -v strace >/dev/null || { echo "FAIL: strace is required for the zero-HTTP gate"; exit 1; }
command -v jq >/dev/null || { echo "FAIL: jq is required to locate the test binary"; exit 1; }

cd "$(dirname "$0")/.."

# Build the test binary and locate its artifact via cargo JSON (no path guessing).
BIN=$(cargo test -p hya --test native_round_trip --no-run --message-format=json \
      | jq -r 'select(.executable != null and .target.name == "native_round_trip") | .executable' \
      | tail -1)
[ -x "$BIN" ] || { echo "FAIL: could not locate native_round_trip test binary"; exit 1; }

TRACE=$(mktemp)
trap 'rm -f "$TRACE"' EXIT

# Empty HOME -> no ~/.config/yaca config (belt-and-suspenders with force_offline).
HOME=$(mktemp -d) strace -f -e trace=socket,bind,connect,listen,accept -o "$TRACE" \
  "$BIN" native_turn_opens_no_socket --exact --nocapture >/dev/null 2>&1 || {
    echo "FAIL: native_round_trip test did not pass under strace"; exit 1;
  }

# An inet socket shows as socket(AF_INET*/AF_INET6*), or bind/connect/listen/accept on such an fd.
# AF_UNIX / AF_NETLINK (local IPC) are not HTTP/network and are allowed.
if grep -E 'socket\(AF_INET6?|\bconnect\(|\bbind\(|\blisten\(|\baccept\(' "$TRACE" \
     | grep -Ev 'AF_UNIX|AF_NETLINK'; then
  echo "FAIL: native turn opened an inet socket (see lines above)"
  exit 1
fi

echo "OK: zero inet sockets"
