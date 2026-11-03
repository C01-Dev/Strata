#!/usr/bin/env bash
# PLAN 5.6 soak: 5 agents on the excalidraw fixture (bench/setup.sh) make scripted edits for
# HOURS (default 8). Each agent, every ~20 s, puts a type error into one of a fixed set of files
# or takes it out again (so the code never grows), then checks that file through the shared
# checker; every 15th round it checks the whole project. The daemon's process tree (daemon plus
# its tsgo checker) is sampled every minute with bench/footprint into bench/results/soak.csv.
# Growth = median of the last 30 minutes / median of minutes 30-60 (for a run under 2 hours:
# last third / middle third). PLAN wants under 10%.
#
# Usage: cargo build --release -p strata && caffeinate -i bench/soak.sh [hours]
set -euo pipefail
HOURS=${1:-8}
BENCH=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$BENCH")
S=$ROOT/target/release/strata
REPO=$BENCH/fixtures/excalidraw/base
export STRATA_HOME=$BENCH/fixtures/soakh
CSV=$BENCH/results/soak.csv
LOG=$BENCH/results/soak.log
AGENTS=(s1 s2 s3 s4 s5)
END=$(( $(date +%s) + $(awk "BEGIN {print int($HOURS * 3600)}") ))

cleanup() {
  kill "${PIDS[@]}" 2>/dev/null || true
  for a in "${AGENTS[@]}"; do (cd "$REPO" && "$S" rm "$a" --force >/dev/null 2>&1) || true; done
  "$S" daemon stop >/dev/null 2>&1 || true
  git -C "$REPO" worktree list --porcelain | awk '/^worktree /{print $2}' | grep "/.strata/" |
    while read -r w; do git -C "$REPO" worktree unlock "$w" 2>/dev/null || true; git -C "$REPO" worktree remove --force "$w"; done
  rm -rf "$REPO/strata.toml" "$STRATA_HOME" "$(dirname "$REPO")/.strata"
}
PIDS=()
trap cleanup EXIT

cd "$REPO"
printf '[repo]\nbase = "strata-base"\n\n[seed]\nwarm_pool = 0\n' > strata.toml
"$S" init --no-service >> "$LOG" 2>&1
for a in "${AGENTS[@]}"; do "$S" new "$a" >> "$LOG" 2>&1; done
# Ten source files every worktree has, the same list in each run.
FILES=()
while IFS= read -r f; do FILES+=("$f"); done < <(git ls-files 'packages/element/src/*.ts' | grep -v -e test -e '\.d\.ts' | sort | head -10)

agent() { # name index
  local a=$1 k=$2 wt i=0
  wt=$(dirname "$REPO")/.strata/base/$a
  while [ "$(date +%s)" -lt "$END" ]; do
    local f=${FILES[$(( (i + k) % ${#FILES[@]} ))]}
    if grep -q "__soak_" "$wt/$f"; then git -C "$wt" checkout -q -- "$f"
    else printf '\nexport const __soak_%s_%s: number = "%s";\n' "$k" "$i" "$i" >> "$wt/$f"; fi
    local start out what=$f
    start=$(date +%s)
    # Exit 1 only means type errors were found (every other round adds one); a check failed
    # when no answer came back.
    if [ $(( i % 15 )) -eq 14 ]; then what=project; out=$("$S" check "$a" --json 2>> "$LOG" || true)
    else out=$("$S" check "$a" "$f" --json 2>> "$LOG" || true); fi
    grep -q '"items"' <<< "$out" || echo "$(date +%T) $a check of $what FAILED" >> "$LOG"
    echo "$(date +%s),$a,$i,$what,$(( $(date +%s) - start )),$(grep -c '"category": "error"' <<< "$out" || true)" >> "$BENCH/results/soak-checks.csv"
    i=$(( i + 1 ))
    sleep 20
  done
}

: > "$BENCH/results/soak-checks.csv"
for k in "${!AGENTS[@]}"; do agent "${AGENTS[$k]}" "$k" & PIDS+=($!); done
DAEMON=$("$S" status --json | node -p 'JSON.parse(require("fs").readFileSync(0)).pid')
"$BENCH/footprint" "$DAEMON" 60000 > "$CSV" & PIDS+=($!)
echo "$(date +%T) soak started: ${#AGENTS[@]} agents, ${HOURS} h, daemon $DAEMON" | tee -a "$LOG"
while [ "$(date +%s)" -lt "$END" ]; do sleep 30; done
wait "${PIDS[@]:0:${#AGENTS[@]}}" 2>/dev/null || true

# Median of the samples in [from, to) minutes after the first one.
median() {
  awk -F, -v from="$1" -v to="$2" 'NR == 1 {t0 = $1} { m = ($1 - t0) / 60000; if (m >= from && m < to) print $2 }' "$CSV" |
    sort -n | awk '{v[NR] = $1} END {print (NR ? v[int((NR + 1) / 2)] : 0)}'
}
total=$(awk -F, 'NR == 1 {t0 = $1} {m = ($1 - t0) / 60000} END {print int(m)}' "$CSV")
if [ "$total" -ge 120 ]; then base=$(median 30 60); last=$(median $((total - 30)) $((total + 1)))
else third=$((total / 3)); base=$(median "$third" $((2 * third))); last=$(median $((2 * third)) $((total + 1))); fi
checks=$(wc -l < "$BENCH/results/soak-checks.csv" | tr -d ' ')
failed=$(grep -c "FAILED" "$LOG" || true)
growth=$(awk -v a="$base" -v b="$last" 'BEGIN { printf "%.1f", (b / a - 1) * 100 }')
echo "$(date +%T) soak done: ${total} min, $checks checks ($failed failed); daemon tree median $((base / 1048576)) MB -> $((last / 1048576)) MB, growth ${growth}%" | tee -a "$LOG"
