#!/usr/bin/env bash
# Do coding agents use Strata's MCP tools on their own, or run tsc themselves?
# Runs Claude Code headless on three real tasks at once, each in its own Strata worktree of the
# excalidraw fixture, and logs every tool call. Tally with: node bench/agent-usage.mjs <logs>
#
#   STRATA_HOME=~/.strata-usage [MODEL=sonnet] [AGENT=codex] bench/agent-usage.sh <condition>
#     asis   the repo as it is (its CLAUDE.md says `yarn test:typecheck`), Strata connected over MCP
#     hint   the same, plus a Strata section appended to CLAUDE.md
#     bare   no CLAUDE.md or AGENTS.md, so nothing in the repo points at tsc
#     note   the repo as it is, relying on the note `strata new` writes (.claude/rules/strata.md)
#
# MODEL picks the model (Claude Code's default otherwise). AGENT=codex runs Codex instead, with
# its workspace-write sandbox, which keeps it to its worktree. SET=related runs four related
# tasks instead (two refactors, two features around text in shapes), for clash detection.
#
# For the clash study (paper/PLAN.md, bench/clash-run.sh runs one cell end to end):
#   REPO=calcom      the fixture to work in (excalidraw by default)
#   TASKS=<file.md>  the tasks, one `## name` heading and one paragraph each (paper/tasks/)
#   CONFLICTS=off    no clash checks while the agents work, and no `strata_conflicts` line in the
#                    note; the control condition. Checks come back on afterwards, so the final
#                    trees are still checked.
# Every run also records timeline.jsonl (status every 5 s, with a hash of each worktree's
# changes), commits each worktree's final state to its branch, and writes run.json and
# final-status.json, which bench/clash-truth.mjs reads.
#
# Needs `AGENTS=0 bench/setup.sh <fixture>`, `strata init --no-service` in its base, and a
# logged-in `claude`. Each session is one real task on your Claude plan.
set -euo pipefail
cond=${1:?usage: agent-usage.sh asis|hint|bare|note}
: "${STRATA_HOME:?set STRATA_HOME to the folder the daemon uses}"
BENCH=$(cd "$(dirname "$0")" && pwd)
REPO=${REPO:-excalidraw}
BASE=$BENCH/fixtures/$REPO/base
AGENT=${AGENT:-claude}
CONFLICTS=${CONFLICTS:-on}
run=$cond
[ -z "${SET:-}" ] || run+=-$SET
[ -z "${TASKS:-}" ] || run+=-$(basename "$TASKS" .md)
[ "$CONFLICTS" = on ] || run+=-quiet
[ -z "${TAG:-}" ] || run+=-$TAG   # repeat a run under new worktree names
[ "$AGENT" = claude ] || run+=-$AGENT
[ -z "${MODEL:-}" ] || run+=-$MODEL
OUT=$BENCH/results/${RESULTS:-agent-usage}/$run
STRATA=$(command -v "${STRATA:-strata}")
mkdir -p "$OUT"
printf '{"mcpServers":{"strata":{"command":"%s","args":["mcp"],"env":{"STRATA_HOME":"%s"}}}}\n' \
  "$STRATA" "$STRATA_HOME" >"$OUT/mcp.json"

# Shell access is limited to checks and reading. A command outside this list is refused, but its
# attempt still lands in the log, which is what the tally counts.
CHECKS="Bash(yarn test*),Bash(yarn tsc*),Bash(yarn vitest*),Bash(yarn fix*),Bash(yarn prettier*),Bash(yarn eslint*)"
CHECKS+=",Bash(npx tsc*),Bash(npx tsgo*),Bash(npx vitest*),Bash(npx eslint*),Bash(npx prettier*)"
CHECKS+=",Bash(git diff*),Bash(git status*),Bash(git log*),Bash(git show*),Bash(grep *),Bash(rg *),Bash(ls *),Bash(wc *)"

if [ -n "${TASKS:-}" ]; then
  names=() prompts=()
  while IFS= read -r -d '' n && IFS= read -r -d '' t; do names+=("$n"); prompts+=("$t"); done < <(
    node -e 'for (const [, n, t] of require("fs").readFileSync(process.argv[1], "utf8").matchAll(/^## (\S+)\s*\n([\s\S]*?)(?=^## |(?![\s\S]))/gm)) process.stdout.write(`${n}\0${t.trim()}\0`)' "$TASKS")
  [ ${#names[@]} -gt 1 ] || { echo "no tasks in $TASKS" >&2; exit 1; }
elif [ "${SET:-}" = related ]; then
  # Two refactors and two features in the same area (text inside shapes), at the same time.
  # Nothing is planted: whether their changes clash is up to the agents.
  names=(rename opts clear upper)
  prompts=(
    "Rename \`getBoundTextElement\` to \`getBoundText\` everywhere in the codebase. Behaviour must stay the same."
    "\`newElementWith\` takes a bare boolean \`force\` as its third argument, which is unreadable at call sites. Change it to an options object, \`{ force?: boolean }\`, and update every caller."
    "Add a \"Clear text\" action: for each selected shape that has text inside it, remove that text and keep the shape. Show it in the context menu for shapes with text, and make it undoable like other actions. Make sure nothing is broken."
    "Add an \"Uppercase text\" action that turns the text of the selected text elements, including text inside shapes, into uppercase, re-measuring it so it still fits its container. Show it in the context menu. Make sure nothing is broken."
  )
else
  names=(desc rename opts)
  prompts=(
    "Add an optional \`description\` string to every Excalidraw element, so users can attach a note to any shape. It should survive saving and loading a file, and duplicating an element should copy it. Make sure nothing is broken."
    "Rename \`hasBoundTextElement\` to \`hasBoundText\` everywhere in the codebase. Behaviour must stay the same."
    "\`newElementWith\` takes a bare boolean \`force\` as its third argument, which is unreadable at call sites. Change it to an options object, \`{ force?: boolean }\`, and update every caller."
  )
fi

# The control: the [checker] conflicts setting in the base's strata.toml, which the daemon reads
# on every round and `strata new` reads when it writes the note.
set_conflicts() { # true|false
  if grep -q '^conflicts *=' "$BASE/strata.toml"; then perl -pi -e "s/^conflicts *=.*/conflicts = $1/" "$BASE/strata.toml"
  else perl -pi -e "s/^\[checker\][ \t]*\$/[checker]\nconflicts = $1/" "$BASE/strata.toml"; fi
}
if [ "$CONFLICTS" = off ]; then set_conflicts false; trap 'set_conflicts true' EXIT; else set_conflicts true; fi

wts=()
for i in "${!names[@]}"; do
  name=${names[$i]}
  wt=$(cd "$BASE" && "$STRATA" new "$run-$name" | tail -1)
  wts+=("$wt")
  case $cond in
    hint) cat >>"$wt/CLAUDE.md" <<'EOF'

## Strata

This repo runs Strata, which shares one TypeScript checker between all agents. For type errors, use the `strata_diagnostics` tool (one file, or the whole project) instead of `yarn test:typecheck` or `tsc`: it answers in about a second and does not start another checker. `strata_references` and `strata_definition` find usages and declarations.
EOF
      ;;
    bare) rm -f "$wt/CLAUDE.md" "$wt/AGENTS.md" ;;
  esac
  echo "[$run] $name in $wt"
  if [ "$AGENT" = codex ]; then
    (codex exec --json --sandbox workspace-write --cd "$wt" ${MODEL:+--model "$MODEL"} \
      -c "mcp_servers.strata.command=\"$STRATA\"" -c 'mcp_servers.strata.args=["mcp"]' \
      -c "mcp_servers.strata.env={STRATA_HOME=\"$STRATA_HOME\"}" \
      "${prompts[$i]}" </dev/null >"$OUT/$name.jsonl" 2>"$OUT/$name.err"
     echo "[$run] $name done") &
  else
    (cd "$wt" && env -u CLAUDECODE claude -p "${prompts[$i]}" ${MODEL:+--model "$MODEL"} \
      --output-format stream-json --verbose \
      --strict-mcp-config --mcp-config "$OUT/mcp.json" \
      --setting-sources project,local --settings '{"disableAllHooks":true}' \
      --permission-mode acceptEdits \
      --allowedTools "Read,Edit,Write,Glob,Grep,mcp__strata,$CHECKS" \
      >"$OUT/$name.jsonl" 2>"$OUT/$name.err"
     echo "[$run] $name done") &
  fi
done

# Every 5 s: the checker's state and clashes, the daemon's CPU time, and each worktree as a git
# tree object (written through a private index, so the agent's own index is untouched), so
# bench/clash-truth.mjs can replay when a clash first existed.
base_commit=$(git -C "$BASE" rev-parse HEAD)
for n in "${names[@]}"; do GIT_INDEX_FILE="$OUT/.index-$n" git -C "$BASE" read-tree "$base_commit"; done
(while :; do
  trees=$(for i in "${!names[@]}"; do
    export GIT_INDEX_FILE="$OUT/.index-${names[$i]}"
    git -C "${wts[$i]}" add -A 2>/dev/null
    printf '"%s":"%s",' "${names[$i]}" "$(git -C "${wts[$i]}" write-tree 2>/dev/null)"; done)
  s=$("$STRATA" status --json 2>/dev/null) || s='{}'
  cpu=$(ps -o cputime= -p "$(jq -r '.pid // 0' <<<"$s")" 2>/dev/null | tr -d ' ')
  jq -c --argjson t "$(date +%s)" --arg cpu "${cpu:-}" --argjson trees "{${trees%,}}" \
    '{t: $t, cpu: $cpu, trees: $trees, checker: [(.checker // [])[] | {memory_bytes, pairs_checked, conflicts}]}' <<<"$s"
  sleep 5
done) >>"$OUT/timeline.jsonl" &
poller=$!

wait $(jobs -p | grep -vx "$poller")
{ kill "$poller" && wait "$poller"; } 2>/dev/null || true
rm -f "$OUT"/.index-*

# The final trees, committed to each agent's branch so the ground truth works on fixed commits.
for i in "${!names[@]}"; do
  git -C "${wts[$i]}" add -A
  git -C "${wts[$i]}" -c user.name=study -c user.email=study@localhost commit -q --no-verify --allow-empty -m "study: final state of ${names[$i]}"
done
jq -n --arg run "$run" --arg repo "$REPO" --arg base "$BASE" --arg base_commit "$(git -C "$BASE" rev-parse HEAD)" \
  --arg agent "$AGENT" --arg model "${MODEL:-default}" --arg conflicts "$CONFLICTS" --arg tasks "${TASKS:-}" \
  --arg strata "$("$STRATA" --version)" --arg date "$(date -u +%FT%TZ)" \
  --argjson agents "$(for i in "${!names[@]}"; do jq -n --arg name "${names[$i]}" --arg path "${wts[$i]}" \
    --arg branch "$(git -C "${wts[$i]}" branch --show-current)" --arg commit "$(git -C "${wts[$i]}" rev-parse HEAD)" \
    '{name: $name, path: $path, branch: $branch, commit: $commit}'; done | jq -s .)" \
  '{run: $run, repo: $repo, base: $base, base_commit: $base_commit, agent: $agent, model: $model, conflicts: $conflicts, tasks: $tasks, strata: $strata, date: $date, agents: $agents}' >"$OUT/run.json"

# Strata's verdict on the final trees: checks on, then wait until every pair of agents that
# changed code has been checked (Strata pairs only those).
set_conflicts true
base_commit=$(git -C "$BASE" rev-parse HEAD)
k=0
for wt in "${wts[@]}"; do
  git -C "$wt" diff --name-only "$base_commit" HEAD | grep -qE '\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$' && k=$((k + 1))
done
want=$((k * (k - 1) / 2))
for _ in $(seq 1 60); do
  [ "$("$STRATA" status --json | jq '[.checker[].pairs_checked] | add // 0')" -ge "$want" ] && break
  sleep 5
done
"$STRATA" status --json >"$OUT/final-status.json"
jq --argjson want "$want" '{pairs_expected: $want, pairs_checked: ([.checker[].pairs_checked] | add // 0)}' "$OUT/final-status.json"
logs=(); for n in "${names[@]}"; do logs+=("$OUT/$n.jsonl"); done
node "$BENCH/agent-usage.mjs" "${logs[@]}"
