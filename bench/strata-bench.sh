#!/usr/bin/env bash
# Gate 2: `strata bench` (the daemon's checker code) on the Phase 0 fixtures and protocol, both
# workloads at 5 agents, appended to bench/results/strata.csv. Compare with results/phase0.csv.
# Usage: bench/strata-bench.sh [fixture...]   (needs `cargo build --release` and bench/setup.sh)
set -euo pipefail
cd "$(dirname "$0")/.."
strata=target/release/strata
fixtures=("${@:-excalidraw calcom}")
for f in ${fixtures[@]}; do
  tsconfig=$(sed -n "s/^ *\"$f [^ ]* [^ ]* \([^\"]*\)\"$/\1/p" bench/setup.sh)
  for w in whole files; do
    "$strata" bench "bench/fixtures/$f" --tsconfig "$tsconfig" --agents 5 --workload "$w" --out bench/results/strata.csv
  done
done
