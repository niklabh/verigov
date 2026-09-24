#!/usr/bin/env bash
# One-shot VeriGov demo: build (if needed), start a throwaway --dev node, run both
# scenarios, print PASS/FAIL, and stop the node.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_BIN="$ROOT/target/release/solochain-template-node"
RPC_PORT="${VERIGOV_RPC_PORT:-9944}"
NODE_LOG="${VERIGOV_NODE_LOG:-/tmp/verigov-node.log}"

if [[ ! -x "$NODE_BIN" ]]; then
  echo "[run-demo] node binary not found; building (this takes a while the first time)..."
  (cd "$ROOT" && cargo build --release)
fi

if [[ ! -d "$ROOT/tools/node_modules" ]]; then
  echo "[run-demo] installing tool dependencies..."
  (cd "$ROOT/tools" && npm install --no-fund --no-audit)
fi

echo "[run-demo] starting dev node on port $RPC_PORT (log: $NODE_LOG)"
"$NODE_BIN" --dev --tmp --rpc-port "$RPC_PORT" >"$NODE_LOG" 2>&1 &
NODE_PID=$!
trap 'echo "[run-demo] stopping node ($NODE_PID)"; kill "$NODE_PID" 2>/dev/null || true; wait "$NODE_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 60); do
  if curl -sf -H 'Content-Type: application/json' \
      -d '{"id":1,"jsonrpc":"2.0","method":"system_health","params":[]}' \
      "http://127.0.0.1:$RPC_PORT" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

cd "$ROOT"
VERIGOV_WS="ws://127.0.0.1:$RPC_PORT" node tools/demo/demo.js
