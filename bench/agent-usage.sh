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
# its workspace-write sandbox, which keeps it to its worktree.
#
# Needs `AGENTS=0 bench/setup.sh excalidraw`, `strata init --no-service` in its base, and a
# logged-in `claude`. Each session is one real task on your Claude plan.
set -euo pipefail
cond=${1:?usage: agent-usage.sh asis|hint|bare|note}
: "${STRATA_HOME:?set STRATA_HOME to the folder the daemon uses}"
BENCH=$(cd "$(dirname "$0")" && pwd)
BASE=$BENCH/fixtures/excalidraw/base
AGENT=${AGENT:-claude}
run=$cond
[ "$AGENT" = claude ] || run+=-$AGENT
[ -z "${MODEL:-}" ] || run+=-$MODEL
OUT=$BENCH/results/agent-usage/$run
STRATA=$(command -v "${STRATA:-strata}")
mkdir -p "$OUT"
printf '{"mcpServers":{"strata":{"command":"%s","args":["mcp"],"env":{"STRATA_HOME":"%s"}}}}\n' \
  "$STRATA" "$STRATA_HOME" >"$OUT/mcp.json"

# Shell access is limited to checks and reading. A command outside this list is refused, but its
# attempt still lands in the log, which is what the tally counts.
CHECKS="Bash(yarn test*),Bash(yarn tsc*),Bash(yarn vitest*),Bash(yarn fix*),Bash(yarn prettier*),Bash(yarn eslint*)"
CHECKS+=",Bash(npx tsc*),Bash(npx tsgo*),Bash(npx vitest*),Bash(npx eslint*),Bash(npx prettier*)"
CHECKS+=",Bash(git diff*),Bash(git status*),Bash(git log*),Bash(git show*),Bash(grep *),Bash(rg *),Bash(ls *),Bash(wc *)"

names=(desc rename opts)
prompts=(
  "Add an optional \`description\` string to every Excalidraw element, so users can attach a note to any shape. It should survive saving and loading a file, and duplicating an element should copy it. Make sure nothing is broken."
  "Rename \`hasBoundTextElement\` to \`hasBoundText\` everywhere in the codebase. Behaviour must stay the same."
  "\`newElementWith\` takes a bare boolean \`force\` as its third argument, which is unreadable at call sites. Change it to an options object, \`{ force?: boolean }\`, and update every caller."
)

for i in "${!names[@]}"; do
  name=${names[$i]}
  wt=$(cd "$BASE" && "$STRATA" new "$run-$name" | tail -1)
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
wait
node "$BENCH/agent-usage.mjs" "$OUT"/*.jsonl
