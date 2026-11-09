#!/usr/bin/env bash
# One run of the clash study (paper/PLAN.md): a fresh daemon home, 4 agents at once on a task
# set, Strata's verdict on the final trees, the ground truth (bench/clash-truth.mjs), then the
# worktrees removed. Their final commits stay on their branches, so the truth can be rerun.
#
#   TASKS=paper/tasks/excalidraw-related.md TAG=a1 [REPO=calcom] [MODEL=sonnet] [CONFLICTS=off] \
#     bench/clash-run.sh
#
# Results land in bench/results/clash-study/<run>/ (not committed). TAG must be unique per run:
# it names the worktrees and the daemon's home.
set -euo pipefail
BENCH=$(cd "$(dirname "$0")" && pwd)
: "${TASKS:?set TASKS to a task file}" "${TAG:?set TAG, unique per run}"
export REPO=${REPO:-excalidraw} TAG RESULTS=clash-study
export TASKS="$(cd "$(dirname "$TASKS")" && pwd)/$(basename "$TASKS")"
export STRATA=${STRATA:-$BENCH/../target/release/strata}
# Short on purpose: a Unix socket path is limited to 104 bytes.
export STRATA_HOME=$HOME/.strata-study/$TAG
BASE=$BENCH/fixtures/$REPO/base
[ ! -e "$STRATA_HOME" ] || { echo "$STRATA_HOME exists: TAG $TAG was used before" >&2; exit 1; }
echo "[clash-run] $TAG: $REPO, $(basename "$TASKS"), ${MODEL:-default model}, conflicts ${CONFLICTS:-on}, $("$STRATA" --version)"

(cd "$BASE" && "$STRATA" init --no-service >/dev/null)
trap '"$STRATA" daemon stop >/dev/null 2>&1 || true' EXIT
"$BENCH/agent-usage.sh" note
out=$(ls -td "$BENCH"/results/clash-study/*/ | head -1)
out=${out%/}
grep -q "\"run\": \".*-$TAG" "$out/run.json" || { echo "newest result $out is not run $TAG" >&2; exit 1; }
node "$BENCH/clash-truth.mjs" "$out"
for p in $(jq -r '.agents[].path' "$out/run.json"); do
  (cd "$BASE" && "$STRATA" rm "$(basename "$p")" --force >/dev/null)
done
echo "[clash-run] $TAG done: $out"
