// Can the shared checker see two agents' changes clash before either is merged?
// Loads the base once through tsbridge, then one snapshot per agent and one holding both
// agents' changes (files both edited get a three-way merge), and reports type errors that only
// the combination has. Each pair is checked twice: the whole project, and only the files either
// agent changed, to see whether the cheap check finds the same clashes.
//
//   node bench/clash.mjs <base worktree> <worktree>...    (every pair of worktrees is checked)
//   CHEAP=1 skips the whole-project checks, to measure the changed-files check on its own.
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { createInterface } from "node:readline";

const [base, ...trees] = process.argv.slice(2);
const bridgePath = new URL("../packages/tsbridge/bridge.mjs", import.meta.url).pathname;
const tmp = mkdtempSync(join(tmpdir(), "strata-clash-"));

// tsbridge over stdio: one JSON line per request and response.
const child = spawn(process.execPath, [bridgePath, base], { stdio: ["pipe", "pipe", "inherit"] });
const waiting = new Map();
let nextId = 1;
createInterface({ input: child.stdout }).on("line", (line) => {
  const msg = JSON.parse(line);
  const w = waiting.get(msg.id);
  waiting.delete(msg.id);
  msg.error ? w.reject(new Error(`${msg.error.code}: ${msg.error.message}`)) : w.resolve(msg.result);
});
const call = (method, params) =>
  new Promise((resolve, reject) => {
    const id = nextId++;
    waiting.set(id, { resolve, reject });
    child.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });

const git = (dir, ...args) => execFileSync("git", ["-C", dir, ...args], { encoding: "utf8" }).split("\n").filter(Boolean);
function changes(tree) {
  const removed = git(tree, "diff", "--name-only", "--diff-filter=D", "HEAD");
  const changed = [...git(tree, "diff", "--name-only", "--diff-filter=d", "HEAD"), ...git(tree, "ls-files", "--others", "--exclude-standard")];
  return { changed, removed };
}

const rel = (f) => (f.startsWith(base + "/") ? f.slice(base.length + 1) : f);
const key = (d) => `${rel(d.file)}\0${d.code}\0${d.message}`;
const ms = (t0) => Math.round(performance.now() - t0);
/** Resident memory of the bridge and everything under it (tsgo), in MB. */
function mb() {
  const rows = execFileSync("ps", ["-axo", "pid=,ppid=,rss="], { encoding: "utf8" }).trim().split("\n").map((l) => l.trim().split(/\s+/).map(Number));
  const tree = new Set([child.pid]);
  for (let grew = true; grew; ) {
    grew = false;
    for (const [pid, ppid] of rows) if (tree.has(ppid) && !tree.has(pid)) tree.add(pid), (grew = true);
  }
  return Math.round(rows.filter(([pid]) => tree.has(pid)).reduce((s, [, , rss]) => s + rss, 0) / 1024);
}

async function check(agent, files, removed, only) {
  await call("createSnapshot", { agent, group: "g", files, removed });
  if (!only) return (await call("diagnostics", { agent })).items;
  const items = [];
  for (const f of only) {
    try {
      items.push(...(await call("diagnostics", { agent, file: f })).items);
    } catch (e) {
      if (!String(e.message).startsWith("not_in_project")) throw e;
    }
  }
  return items;
}

/** Errors in `both` beyond those already in `a` or `b` (counted per file, code and message). */
function onlyTogether(both, a, b) {
  const seen = new Map();
  for (const d of [...a, ...b]) seen.set(key(d), (seen.get(key(d)) ?? 0) + 1);
  return both.filter((d) => {
    const n = seen.get(key(d)) ?? 0;
    if (n > 0) return seen.set(key(d), n - 1), false;
    return true;
  });
}

await call("hello", { protocol: 1 });
let t0 = performance.now();
const { files: programFiles } = await call("openBase", { group: "g", root: base, tsconfigs: [join(base, "tsconfig.json")] });
console.log(`base: ${programFiles} program files, opened in ${ms(t0)} ms, checker ${mb()} MB`);

const cheapOnly = process.env.CHEAP === "1";
const agents = trees.map((tree) => ({ name: basename(tree), tree, ...changes(tree) }));
for (const a of agents) {
  a.files = Object.fromEntries(a.changed.map((r) => [join(base, r), join(a.tree, r)]));
  a.removedAbs = a.removed.map((r) => join(base, r));
  t0 = performance.now();
  a.full = cheapOnly ? [] : await check(a.name, a.files, a.removedAbs);
  a.fullMs = ms(t0);
  console.log(`${a.name}: ${a.changed.length} changed, ${a.removed.length} removed; ${a.full.length} errors alone (${a.fullMs} ms, checker ${mb()} MB)`);
}

for (let i = 0; i < agents.length; i++) {
  for (let j = i + 1; j < agents.length; j++) {
    const [a, b] = [agents[i], agents[j]];
    const files = { ...a.files, ...b.files };
    const conflicts = [];
    const shared = a.changed.filter((r) => b.changed.includes(r));
    for (const r of shared) {
      const out = join(tmp, `${a.name}+${b.name}`, r);
      execFileSync("mkdir", ["-p", join(out, "..")]);
      let merged;
      try {
        merged = execFileSync("git", ["merge-file", "-p", join(a.tree, r), join(base, r), join(b.tree, r)], { encoding: "utf8" });
      } catch (e) {
        merged = e.stdout; // exit code = number of conflicts; stdout still holds the result
        conflicts.push(r);
      }
      writeFileSync(out, merged);
      files[join(base, r)] = out;
    }
    for (const r of [...a.removed.filter((r) => b.changed.includes(r)), ...b.removed.filter((r) => a.changed.includes(r))]) {
      conflicts.push(`${r} (removed by one, edited by the other)`);
    }
    const removed = [...new Set([...a.removedAbs, ...b.removedAbs])].filter((f) => !(f in files));
    const name = `${a.name}+${b.name}`;
    const before = mb();

    t0 = performance.now();
    const fullClash = cheapOnly ? [] : onlyTogether(await check(name, files, removed), a.full, b.full);
    const fullMs = ms(t0);
    const peak = mb();

    // Cheap version: only files either agent changed, per agent and together.
    const touched = [...new Set([...a.changed, ...b.changed])].filter((r) => /\.(ts|tsx|mts|cts)$/.test(r)).map((r) => join(base, r));
    const aOnly = await check(a.name, a.files, a.removedAbs, touched);
    const bOnly = await check(b.name, b.files, b.removedAbs, touched);
    t0 = performance.now();
    const cheapClash = onlyTogether(await check(name, files, removed, touched), aOnly, bOnly);
    const cheapMs = ms(t0);
    await call("dispose", { agent: name });

    console.log(`\n## ${name}`);
    console.log(`  files both edited: ${shared.length}${conflicts.length ? `, conflicts: ${conflicts.join(", ")}` : ""}`);
    if (!cheapOnly) console.log(`  whole project: ${fullClash.length} new errors (${fullMs} ms, checker ${before} -> ${peak} MB)`);
    console.log(`  changed files only: ${cheapClash.length} new errors (${cheapMs} ms, ${touched.length} files, checker ${mb()} MB)`);
    for (const d of cheapOnly ? cheapClash : fullClash) console.log(`    ${rel(d.file)}:${d.line + 1}:${d.character + 1} TS${d.code} ${d.message.split("\n")[0]}`);
  }
}
child.stdin.end();
