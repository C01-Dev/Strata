#!/usr/bin/env bash
# Phase 0 measurement run: every mode in its own process, one at a time, so nothing else competes for RAM.
# Usage: bench/phase0.sh [fixture...]   (IDLE=60,900 by default; LSP is a launch comparison, 60 s only)
set -euo pipefail
cd "$(dirname "$0")"
IDLE=${IDLE:-60,900}
fixtures=("${@:-excalidraw calcom}")
for f in ${fixtures[@]}; do
  for mode in baseline shared; do node run.mjs "$mode" "$f" --idle "$IDLE"; sleep 5; done
  node run.mjs lsp "$f" --idle 60; sleep 5
  node compare.mjs "$f" > "results/raw/$f-parity.json" || echo "PARITY MISMATCH: $f"
done
