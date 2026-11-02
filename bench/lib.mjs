// Shared Phase 0 helpers: fixture paths, worktree layers, fixed hover targets.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

export const BENCH = import.meta.dirname;
export const TSC = join(BENCH, "node_modules/@typescript/typescript-darwin-arm64/lib/tsc");

export function fixtureInfo(fixture, agents) {
  // tsconfig per fixture comes from setup.sh's FIXTURES table (single source of truth).
  const tsconfigRel = readFileSync(join(BENCH, "setup.sh"), "utf8").match(new RegExp(`"${fixture} \\S+ \\S+ (\\S+)"`))[1];
  const BASE = join(BENCH, "fixtures", fixture, "base");
  const WTS = Array.from({ length: agents }, (_, i) => join(BENCH, "fixtures", fixture, `wt${i + 1}`));
  return { tsconfigRel, BASE, WTS };
}

// Layer = the worktree's changes against strata-base (committed, uncommitted, untracked), re-rooted at base paths,
// plus deleted paths. Read from git, as the overlay tracker will, so only changed files are read: a content walk of
// the whole repo cost the harness ~590 MiB on calcom, which the footprint counted against the shared side.
// Ignored paths (node_modules, generated clients) are not in the layer; the fixtures never change them.
export function layerFor(base, wt) {
  const git = (...a) => execFileSync("git", ["-C", wt, ...a], { encoding: "utf8" }).split("\0").filter(Boolean);
  const status = git("diff", "-z", "--name-status", "--no-renames", "strata-base");
  const changed = git("ls-files", "-z", "--others", "--exclude-standard"), removed = [];
  for (let k = 0; k < status.length; k += 2) (status[k] === "D" ? removed : changed).push(status[k + 1]);
  return { files: changed.map((r) => [join(base, r), readFileSync(join(wt, r), "utf8")]), removed: removed.map((r) => join(base, r)) };
}

// 20 identifier positions in files that no agent touched, so offsets are valid in every worktree.
export function hoverTargets(base, wts, tsconfigRel, count = 20) {
  const touched = new Set();
  for (const wt of new Set(wts)) {
    for (const f of execFileSync("git", ["-C", wt, "diff", "--name-only", "strata-base", "HEAD"], { encoding: "utf8" }).split("\n")) touched.add(f);
  }
  const root = tsconfigRel.includes("/") ? tsconfigRel.slice(0, tsconfigRel.lastIndexOf("/") + 1) : "";
  const skip = /(^|\/)(examples|tests?|__tests__|playwright|dist|build|scripts)\/|\.d\.ts$|\.(test|spec|stories)\.tsx?$/;
  const files = execFileSync("git", ["-C", base, "ls-files", "--", `${root}*.ts`, `${root}*.tsx`], { encoding: "utf8" })
    .split("\n").filter((f) => f && !touched.has(f) && !skip.test(f.slice(root.length))).sort();
  const targets = [];
  for (let k = 0; targets.length < count && k < files.length; k++) {
    const f = files[Math.floor((((k + 1) * 2654435761) % 4294967296) / 4294967296 * files.length)];
    if (targets.some((t) => t.file === f)) continue;
    const text = readFileSync(join(base, f), "utf8");
    const from = Math.floor(text.length * 0.3);
    for (const m of [...text.slice(from).matchAll(/\b[A-Za-z_]\w{3,}\b/g)].slice(0, 2)) targets.push({ file: f, pos: from + m.index });
  }
  return targets.slice(0, count);
}

// One phys_footprint reading (bytes) for pid and its descendants.
export function footprintNow(pid = process.pid) {
  const out = execFileSync("/bin/sh", ["-c", `"${join(BENCH, "footprint")}" ${pid} 1 | head -1`], { encoding: "utf8" });
  return Number(out.split(",")[1]);
}
