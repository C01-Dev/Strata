#!/usr/bin/env bash
# Owner-requested follow-up to Gate 0: per-file agent workload (each agent checks only the files it edited)
# at 1, 3, 5 and 8 agents, every mode in its own process, one at a time. No idle readings (idle 60 s == warm in Phase 0).
# Usage: bench/agents.sh [fixture...]
set -euo pipefail
cd "$(dirname "$0")"
fixtures=("${@:-excalidraw calcom}")
for f in ${fixtures[@]}; do
  for n in 1 3 5 8; do
    for mode in baseline shared lsp; do node run.mjs "$mode" "$f" --agents "$n" --workload files --idle ""; sleep 5; done
    node compare.mjs "$f" "$n" files > "results/raw/$f-parity-$n-files.json" || echo "PARITY MISMATCH: $f $n"
  done
done
node report.mjs
