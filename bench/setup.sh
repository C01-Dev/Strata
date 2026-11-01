#!/usr/bin/env bash
# Phase 0 fixtures: pinned repos, TS 7.1 fixes, deps, 5 worktrees with scripted diffs.
# Idempotent: rerun skips finished steps. Usage: bench/setup.sh [fixture...]
set -euo pipefail
cd "$(dirname "$0")"
BENCH=$PWD
FIX=$BENCH/fixtures
AGENTS=${AGENTS:-5}

# name repo sha tsconfig(relative to repo root)
FIXTURES=(
  "excalidraw excalidraw/excalidraw 8bcdca4526867433f99fdfcb55a063668db0c9dc tsconfig.json"
  "calcom calcom/cal.com f972feefa1376cf47631177231c671da9a9dd09f apps/web/tsconfig.json"
)

log() { printf '\n[setup] %s\n' "$*"; }

clone() { # name repo sha
  local base=$FIX/$1/base
  [ -d "$base/.git" ] && return
  log "$1: shallow fetch $2@$3"
  mkdir -p "$base"
  git -C "$base" init -q
  git -C "$base" remote add origin "https://github.com/$2"
  git -C "$base" fetch -q --depth 1 origin "$3"
  git -C "$base" checkout -q -b strata-base FETCH_HEAD
}

migrate() { # name: apply bench/patches/<name>.sh once, commit so worktrees inherit it
  local base=$FIX/$1/base patch=$BENCH/patches/$1.sh
  [ -f "$patch" ] || return 0
  git -C "$base" log -1 --format=%s | grep -q '^strata: ts7.1' && return
  log "$1: TS 7.1 fixes"
  (cd "$base" && bash "$patch")
  git -C "$base" add -A
  git -C "$base" -c user.name=strata -c user.email=strata@localhost commit -q --no-verify -m "strata: ts7.1 migration fixes" --allow-empty
}

install() { # name
  local base=$FIX/$1/base
  [ -d "$base/node_modules" ] && return
  log "$1: install"
  (cd "$base"
   if [ -f pnpm-lock.yaml ]; then pnpm install --frozen-lockfile --ignore-scripts
   elif [ -f bun.lock ] || [ -f bun.lockb ]; then bun install --frozen-lockfile --ignore-scripts
   elif [ -f .yarnrc.yml ]; then YARN_ENABLE_SCRIPTS=0 node "$(sed -n 's/^yarnPath: *//p' .yarnrc.yml)" install --immutable
   elif [ -f yarn.lock ]; then yarn install --frozen-lockfile --ignore-scripts
   else npm ci --ignore-scripts; fi
   if [ -f "$BENCH/patches/$1.post.sh" ]; then bash "$BENCH/patches/$1.post.sh"; fi)
}

worktrees() { # name tsconfig
  local name=$1 base=$FIX/$1/base
  for ((i = 1; i <= AGENTS; i++)); do
    local wt=$FIX/$name/wt$i
    if [ ! -d "$wt" ]; then
      log "$name: worktree wt$i"
      git -C "$base" worktree add -q -b "agent-$i" "$wt" strata-base
      local t0=$SECONDS
      # Seed every ignored path (node_modules, generated clients) by APFS clone (cp -c), keeping symlinks as-is.
      git -C "$base" ls-files -z --others --ignored --exclude-standard --directory |
        while IFS= read -r -d '' p; do p=${p%/}; mkdir -p "$wt/$(dirname "$p")"; cp -c -R "$base/$p" "$wt/$p"; done
      log "$name: wt$i ignored paths cloned in $((SECONDS - t0))s"
    fi
    if ! git -C "$wt" log -1 --format=%s | grep -q '^strata: agent diff'; then
      node "$BENCH/diff.mjs" "$wt" "$2" "$i"
      git -C "$wt" add -A
      git -C "$wt" -c user.name=strata -c user.email=strata@localhost commit -q --no-verify -m "strata: agent diff $i"
    fi
  done
}

want=("$@")
for spec in "${FIXTURES[@]}"; do
  read -r name repo sha tsconfig <<<"$spec"
  if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " =~ " $name " ]]; then continue; fi
  clone "$name" "$repo" "$sha"
  migrate "$name"
  install "$name"
  worktrees "$name" "$tsconfig"
done
log "typescript $(node -p 'require("./node_modules/typescript/package.json").version')"
