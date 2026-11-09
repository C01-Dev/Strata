// Ground truth for one clash-study run (paper/PLAN.md), independent of Strata's checker: for
// every pair of agents, an actual `git merge` of their final commits, then the TypeScript CLI on
// each agent alone and on the merge, and the test files either agent touched. Compares the
// result with Strata's own verdict on the final trees (final-status.json).
//
//   node bench/clash-truth.mjs bench/results/agent-usage/<run>     (TESTS=0 skips the tests)
//
// Writes <run>/truth.json. Every state is checked in one scratch worktree next to the base,
// seeded once with the base's ignored files (node_modules, generated types), so agent alone
// and merged are checked with the same dependencies.
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const TSCONFIG = { excalidraw: "tsconfig.json", calcom: "apps/web/tsconfig.json" };
const CODE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;
const TEST = /\.(test|spec)\.[cm]?[jt]sx?$/;

const dir = process.argv[2];
const run = JSON.parse(readFileSync(join(dir, "run.json"), "utf8"));
const final = JSON.parse(readFileSync(join(dir, "final-status.json"), "utf8"));
const tsconfig = TSCONFIG[run.repo] ?? "tsconfig.json";
const tests = process.env.TESTS !== "0";

const git = (cwd, ...args) => execFileSync("git", ["-C", cwd, ...args], { encoding: "utf8", maxBuffer: 1 << 28 });
const lines = (s) => s.split("\n").filter(Boolean);
const t0 = Date.now();
const log = (m) => console.error(`[truth ${Math.round((Date.now() - t0) / 1000)}s] ${m}`);

// The scratch worktree, with every ignored path of the base cloned in (APFS clone, cp -c).
const scratch = join(dirname(run.base), `.truth-${run.run}`);
if (existsSync(scratch)) git(run.base, "worktree", "remove", "--force", scratch);
git(run.base, "worktree", "add", "-q", "--detach", scratch, run.base_commit);
log("seeding ignored files");
const ignored = execFileSync("git", ["-C", run.base, "ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"], { maxBuffer: 1 << 28 })
  .toString().split("\0").filter(Boolean).map((p) => p.replace(/\/$/, ""));
for (const p of ignored) {
  if (p.startsWith(".truth-") || existsSync(join(scratch, p))) continue;
  mkdirSync(dirname(join(scratch, p)), { recursive: true });
  execFileSync("cp", ["-c", "-R", join(run.base, p), join(scratch, p)]);
}

function checkout(commit) {
  git(scratch, "reset", "-q", "--hard");
  git(scratch, "checkout", "-q", "-f", "--detach", commit);
  git(scratch, "clean", "-q", "-fd"); // untracked only; ignored files stay
}

/** TypeScript errors in the scratch tree: [{file, line, column, code, message}]. Never incremental:
 * cal.com turns it on, and a seeded .tsbuildinfo would answer from cache. */
function typecheck() {
  const r = spawnSync(join(scratch, "node_modules/typescript/bin/tsc"), ["-p", tsconfig, "--noEmit", "--incremental", "false", "--pretty", "false"], { cwd: scratch, encoding: "utf8", maxBuffer: 1 << 28 });
  const out = [];
  for (const l of (r.stdout ?? "").split("\n")) {
    const m = l.match(/^(.+?)\((\d+),(\d+)\): error TS(\d+): (.*)$/);
    if (m) out.push({ file: m[1], line: +m[2], column: +m[3], code: +m[4], message: m[5] });
  }
  if (r.status !== 0 && out.length === 0) throw new Error(`tsc failed: ${r.stderr || r.stdout}`);
  return out;
}

/** Test files that pass and fail in the scratch tree, of those given that exist there. */
function runTests(files) {
  const present = files.filter((f) => existsSync(join(scratch, f)));
  if (!present.length) return { passed: [], failed: [] };
  const report = join(scratch, ".truth-vitest.json");
  spawnSync("npx", ["vitest", "run", ...present, "--reporter=json", `--outputFile=${report}`], { cwd: scratch, env: { ...process.env, TZ: "UTC", CI: "1" }, encoding: "utf8", timeout: 15 * 60_000 });
  const passed = [], failed = [];
  try {
    for (const r of JSON.parse(readFileSync(report, "utf8")).testResults ?? []) {
      const f = r.name.startsWith(scratch) ? r.name.slice(scratch.length + 1) : r.name;
      (r.status === "passed" ? passed : failed).push(f);
    }
  } catch {
    failed.push(...present); // no report: the run itself broke (a syntax error, a missing import)
  }
  rmSync(report, { force: true });
  return { passed, failed };
}

const key = (d) => `${d.file}\0${d.code}\0${d.message}`;
/** Errors in `both` beyond those in `own`, counting repeats (Strata's rule, NOTES "Clash detection"). */
function onlyTogether(both, own) {
  const seen = new Map();
  for (const d of own) seen.set(key(d), (seen.get(key(d)) ?? 0) + 1);
  return both.filter((d) => {
    const n = seen.get(key(d)) ?? 0;
    if (n > 0) return seen.set(key(d), n - 1), false;
    return true;
  });
}

// Each agent alone.
const agents = run.agents.map((a) => {
  const changed = lines(git(run.base, "diff", "--name-only", run.base_commit, a.commit));
  checkout(a.commit);
  log(`${a.name}: ${changed.length} files changed, checking`);
  const errors = typecheck();
  return { ...a, changed, code: changed.some((f) => CODE.test(f)), errors, tests: tests ? runTests(changed.filter((f) => TEST.test(f))) : null };
});

// Strata's verdict, keyed by the pair's task names (worktrees are named <run>-<task>).
const strip = (n) => (n.startsWith(`${run.run}-`) ? n.slice(run.run.length + 1) : n);
const strataPairs = new Map();
for (const c of final.checker ?? []) for (const p of c.conflicts ?? []) strataPairs.set([...p.agents].map(strip).sort().join("+"), p);

const pairs = [];
for (let i = 0; i < agents.length; i++) {
  for (let j = i + 1; j < agents.length; j++) {
    const [a, b] = [agents[i], agents[j]];
    const name = [a.name, b.name].sort().join("+");
    const pair = { pair: name, truth: { unmerged: [], clashes: [], tests: [] }, strata: null };
    checkout(a.commit);
    const merge = spawnSync("git", ["-C", scratch, "merge", "--no-commit", "--no-ff", b.commit], { encoding: "utf8" });
    if (merge.status !== 0) {
      pair.truth.unmerged = lines(git(scratch, "diff", "--name-only", "--diff-filter=U"));
      spawnSync("git", ["-C", scratch, "merge", "--abort"]);
    } else {
      log(`${name}: merged, checking`);
      pair.truth.clashes = onlyTogether(typecheck(), [...a.errors, ...b.errors]);
      if (tests) {
        const touched = [...new Set([...a.changed, ...b.changed].filter((f) => TEST.test(f)))];
        const alone = new Set([...a.tests.passed, ...b.tests.passed]);
        const aloneFailed = new Set([...a.tests.failed, ...b.tests.failed]);
        // A behaviour clash: passes in every tree that has it alone, fails merged.
        pair.truth.tests = runTests(touched).failed.filter((f) => alone.has(f) && !aloneFailed.has(f));
      }
    }
    const s = strataPairs.get(name);
    const paired = a.code && b.code;
    pair.strata = !paired ? { verdict: "unpaired" }
      : !s ? { verdict: "clean" }
      : s.skipped ? { verdict: "skipped", reason: s.skipped }
      : { verdict: s.clashes.length ? "clash" : "unmerged", clashes: s.clashes, unmerged: s.unmerged };
    const kind = pair.truth.unmerged.length ? "unmerged" : pair.truth.clashes.length ? "clash" : pair.truth.tests.length ? "tests only" : "clean";
    pair.truth.verdict = kind;
    pairs.push(pair);
  }
}

// Warning latency: for each pair Strata warned about while the agents worked, walk back through
// the timeline's tree snapshots from the first warning to the first poll at which the problem
// already existed (git cannot merge, or a type clash), and time the gap.
const timeline = existsSync(join(dir, "timeline.jsonl"))
  ? readFileSync(join(dir, "timeline.jsonl"), "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l))
  : [];
const env = { ...process.env, GIT_AUTHOR_NAME: "study", GIT_AUTHOR_EMAIL: "study@localhost", GIT_COMMITTER_NAME: "study", GIT_COMMITTER_EMAIL: "study@localhost" };
const commits = new Map(), treeErrors = new Map();
const commitOf = (tree) => {
  if (!commits.has(tree)) commits.set(tree, execFileSync("git", ["-C", run.base, "commit-tree", tree, "-p", run.base_commit, "-m", "snapshot"], { encoding: "utf8", env }).trim());
  return commits.get(tree);
};
function errorsOf(tree) {
  if (!treeErrors.has(tree)) {
    git(scratch, "read-tree", "-u", "--reset", tree);
    git(scratch, "clean", "-q", "-fd");
    treeErrors.set(tree, typecheck());
  }
  return treeErrors.get(tree);
}
/** The problem between two snapshots: "unmerged", "clash" or null. */
function problemAt(ta, tb) {
  if (!/^[0-9a-f]{40}$/.test(ta ?? "") || !/^[0-9a-f]{40}$/.test(tb ?? "")) return null; // older runs logged hashes, not trees
  const m = spawnSync("git", ["-C", run.base, "merge-tree", "--write-tree", commitOf(ta), commitOf(tb)], { encoding: "utf8" });
  if (m.status === 1) return "unmerged";
  if (m.status !== 0) throw new Error(`merge-tree: ${m.stderr}`);
  const merged = m.stdout.split("\n")[0];
  return onlyTogether(errorsOf(merged), [...errorsOf(ta), ...errorsOf(tb)]).length ? "clash" : null;
}
for (const p of pairs) {
  const [a, b] = p.pair.split("+");
  const first = timeline.findIndex((e) => (e.checker ?? []).some((c) => (c.conflicts ?? []).some((q) => !q.skipped && q.agents.map(strip).sort().join("+") === p.pair)));
  if (first < 0) continue;
  let k = first;
  while (k > 0 && problemAt(timeline[k - 1].trees?.[a], timeline[k - 1].trees?.[b])) k--;
  const existed = problemAt(timeline[k].trees?.[a], timeline[k].trees?.[b]);
  // The problem appeared somewhere between poll k-1 and poll k: the latency is within 5 s of this.
  p.latency = { warned_at: timeline[first].t, existed_at: timeline[k].t, seconds: timeline[first].t - timeline[k].t, problem: existed };
  log(`${p.pair}: warned ${p.latency.seconds} s after the ${existed ?? "problem"} first showed in a snapshot`);
}

checkout(run.base_commit);
git(run.base, "worktree", "remove", "--force", scratch);

const out = {
  run: run.run,
  tsc_errors_alone: Object.fromEntries(agents.map((a) => [a.name, a.errors.length])),
  tests_alone: tests ? Object.fromEntries(agents.map((a) => [a.name, { passed: a.tests.passed.length, failed: a.tests.failed }])) : null,
  pairs,
};
writeFileSync(join(dir, "truth.json"), JSON.stringify(out, null, 2) + "\n");
for (const p of pairs) {
  const t = p.truth;
  const detail = t.unmerged.length ? t.unmerged.join(", ") : t.clashes.map((c) => `${c.file}:${c.line} TS${c.code}`).join(", ") || t.tests.join(", ");
  console.log(`${p.pair.padEnd(24)} truth: ${t.verdict.padEnd(10)} strata: ${p.strata.verdict.padEnd(9)} ${detail}`);
}
