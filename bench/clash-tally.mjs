// Tallies the clash study (paper/PLAN.md) across runs written by bench/clash-run.sh.
//
//   node bench/clash-tally.mjs [bench/results/clash-study]
//
// Prints, per cell (repo, task set, model, condition): clash rates with 95% Wilson intervals,
// Strata's verdict against the ground truth, warning latency and the checker's cost. Writes
// labels.csv next to the runs: every agent's final summary, for labelling with paper/rubric.md.
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

const root = process.argv[2] ?? new URL("results/clash-study", import.meta.url).pathname;
const read = (f) => JSON.parse(readFileSync(f, "utf8"));
const jsonl = (f) => (existsSync(f) ? readFileSync(f, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);

/** 95% Wilson score interval for k of n, as "k/n (p%, lo–hi%)". */
function rate(k, n) {
  if (!n) return "0/0";
  const z = 1.96, p = k / n, d = 1 + (z * z) / n;
  const c = (p + (z * z) / (2 * n)) / d, h = (z * Math.sqrt((p * (1 - p)) / n + (z * z) / (4 * n * n))) / d;
  const pct = (x) => Math.round(x * 100);
  return `${k}/${n} (${pct(p)}%, ${pct(Math.max(0, c - h))}-${pct(Math.min(1, c + h))}%)`;
}

/** The agent's final summary and how often it called strata_conflicts, from its log. */
function agentLog(file) {
  let summary = "", conflictsCalls = 0;
  for (const m of jsonl(file)) {
    if (m.type === "result" && typeof m.result === "string") summary = m.result;
    if (m.type === "assistant") for (const c of m.message?.content ?? []) if (c.type === "tool_use" && c.name === "mcp__strata__strata_conflicts") conflictsCalls++;
    if (m.type === "item.completed" && m.item?.type === "agent_message") summary = m.item.text ?? summary;
    if (m.type === "item.completed" && m.item?.type === "mcp_tool_call" && m.item.tool === "strata_conflicts") conflictsCalls++;
  }
  return { summary, conflictsCalls };
}

const runs = readdirSync(root).filter((d) => existsSync(join(root, d, "truth.json"))).sort();
const cells = new Map();
const labels = [["run", "agent", "pair_truth", "strata_conflicts_calls", "summary", "R1", "R2", "R3", "R4", "notes"]];

for (const d of runs) {
  const dir = join(root, d);
  const run = read(join(dir, "run.json"));
  const truth = read(join(dir, "truth.json"));
  const timeline = jsonl(join(dir, "timeline.jsonl"));
  const cellKey = [run.repo, basename(run.tasks || "", ".md"), run.model, run.conflicts].join(" | ");
  const cell = cells.get(cellKey) ?? { runs: 0, pairs: 0, unmerged: 0, clash: 0, testsOnly: 0, any: 0, tp: 0, fp: 0, fn: 0, unpairedTruth: 0, latencies: [], agents: 0, called: 0, peakMb: 0 };
  cell.runs++;
  for (const p of truth.pairs) {
    const t = p.truth.verdict, s = p.strata.verdict;
    cell.pairs++;
    if (t === "unmerged") cell.unmerged++;
    if (t === "clash") cell.clash++;
    if (t === "tests only") cell.testsOnly++;
    if (t !== "clean") cell.any++;
    // Detection is judged on what Strata claims to see: type clashes and files git cannot merge.
    const real = t === "clash" || t === "unmerged", flagged = s === "clash" || s === "unmerged";
    if (s === "unpaired" && real) cell.unpairedTruth++;
    if (real && flagged) cell.tp++;
    else if (!real && flagged) cell.fp++;
    else if (real && !flagged) cell.fn++;
    if (p.latency?.problem) cell.latencies.push(p.latency.seconds);
  }
  for (const e of timeline) for (const c of e.checker ?? []) cell.peakMb = Math.max(cell.peakMb, Math.round((c.memory_bytes ?? 0) / 2 ** 20));
  for (const a of run.agents) {
    const { summary, conflictsCalls } = agentLog(join(dir, `${a.name}.jsonl`));
    cell.agents++;
    if (conflictsCalls) cell.called++;
    const inPair = truth.pairs.filter((p) => p.pair.split("+").includes(a.name) && p.truth.verdict !== "clean").map((p) => `${p.pair}:${p.truth.verdict}`).join(" ");
    labels.push([run.run, a.name, inPair, conflictsCalls, summary.replace(/\s+/g, " ").trim(), "", "", "", "", ""]);
  }
  cells.set(cellKey, cell);
}

const median = (xs) => (xs.length ? [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)] : null);
for (const [key, c] of cells) {
  console.log(`\n## ${key}  (${c.runs} runs, ${c.pairs} pairs)`);
  console.log(`  any problem:        ${rate(c.any, c.pairs)}`);
  console.log(`  type clash:         ${rate(c.clash, c.pairs)}`);
  console.log(`  git cannot merge:   ${rate(c.unmerged, c.pairs)}`);
  console.log(`  tests only:         ${rate(c.testsOnly, c.pairs)}  (what a type check cannot see)`);
  console.log(`  Strata recall:      ${rate(c.tp, c.tp + c.fn)}${c.unpairedTruth ? `, ${c.unpairedTruth} missed because one agent changed no code` : ""}`);
  console.log(`  Strata precision:   ${rate(c.tp, c.tp + c.fp)}`);
  if (c.latencies.length) console.log(`  warning latency:    median ${median(c.latencies)} s, worst ${Math.max(...c.latencies)} s (from the first 5 s snapshot showing the problem)`);
  console.log(`  called strata_conflicts: ${rate(c.called, c.agents)} of agents`);
  console.log(`  checker peak:       ${c.peakMb} MB`);
}
const csv = (v) => `"${String(v).replace(/"/g, '""')}"`;
writeFileSync(join(root, "labels.csv"), labels.map((r) => r.map(csv).join(",")).join("\n") + "\n");
console.log(`\n${labels.length - 1} agent summaries in ${join(root, "labels.csv")}`);
