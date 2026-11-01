// Shared Phase 0 helpers: fixture paths, worktree layers, fixed hover targets.
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

export const BENCH = import.meta.dirname;
export const TSC = join(BENCH, "node_modules/@typescript/typescript-darwin-arm64/lib/tsc");

export function fixtureInfo(fixture, agents) {
  // tsconfig per fixture comes from setup.sh's FIXTURES table (single source of truth).
  const tsconfigRel = readFileSync(join(BENCH, "setup.sh"), "utf8").match(new RegExp(`"${fixture} \\S+ \\S+ (\\S+)"`))[1];
  const BASE = join(BENCH, "fixtures", fixture, "base");
  const WTS = Array.from({ length: agents }, (_, i) => join(BENCH, "fixtures", fixture, `wt${(i % 5) + 1}`));
  return { tsconfigRel, BASE, WTS };
}

// Every file under dir except node_modules and .git, as a set of relative paths.
export function walk(dir, rel = "", out = new Set()) {
  for (const e of readdirSync(join(dir, rel), { withFileTypes: true })) {
    if (e.name === "node_modules" || e.name === ".git") continue;
    const r = rel ? `${rel}/${e.name}` : e.name;
    if (e.isDirectory()) walk(dir, r, out);
    else if (e.isFile()) out.add(r);
  }
  return out;
}

// Layer = files that differ from base or exist only in the worktree, re-rooted at base paths,
// plus base files missing from the worktree. Content comparison, not git, so untracked files count.
export function layerFor(base, wt, baseFiles) {
  const wtFiles = walk(wt);
  const files = [];
  for (const r of wtFiles) {
    const w = readFileSync(join(wt, r));
    if (!baseFiles.has(r) || !w.equals(readFileSync(join(base, r)))) files.push([join(base, r), w.toString("utf8")]);
  }
  const removed = [...baseFiles].filter((r) => !wtFiles.has(r)).map((r) => join(base, r));
  return { files, removed };
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
