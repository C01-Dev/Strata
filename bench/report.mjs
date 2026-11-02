// Task 0.7: roll every run summary into results/phase0.csv and print the Gate 0 table.
// S = 1 - shared / baseline, on peak and warm readings.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const RAW = join(import.meta.dirname, "results/raw");
const runs = readdirSync(RAW).filter((f) => /^[a-z]+-(baseline|shared|lsp)-\d+\.json$/.test(f))
  .map((f) => JSON.parse(readFileSync(join(RAW, f), "utf8")).summary);

const cols = ["fixture", "mode", "agents", "harness", "peak", "warm", "idle_60", "idle_900"];
const mb = (r, k) => (r.mb[k] === undefined ? "" : r.mb[k].toFixed(0));
const secs = (r) => ["loaded", "diagnostics_done", "hover_done"].map((k) => r.seconds[k] ?? "");
const csv = [[...cols.map((c) => (c === "fixture" || c === "mode" || c === "agents" ? c : `${c}_mb`)), "loaded_s", "diagnostics_done_s", "hover_done_s", "typescript", "cpu", "ram_gb", "os", "fs"].join(",")];
for (const r of runs.sort((a, b) => a.tag.localeCompare(b.tag))) {
  csv.push([r.fixture, r.mode, r.agents, ...cols.slice(3).map((k) => mb(r, k)), ...secs(r), r.typescript, `"${r.machine.cpu}"`, r.machine.ramGb.toFixed(0), `"${r.machine.os}"`, r.machine.fs].join(","));
}
writeFileSync(join(import.meta.dirname, "results/phase0.csv"), csv.join("\n") + "\n");

console.log("| Fixture | Agents | Baseline peak | Shared peak | S peak | Baseline warm | Shared warm | S warm | Baseline idle 15 min | Shared idle 15 min | 5x LSP peak |");
console.log("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
const by = (f, m, n) => runs.find((r) => r.fixture === f && r.mode === m && r.agents === n);
for (const f of [...new Set(runs.map((r) => r.fixture))]) {
  for (const n of [...new Set(runs.filter((r) => r.fixture === f).map((r) => r.agents))].sort((a, b) => a - b)) {
    const b = by(f, "baseline", n), s = by(f, "shared", n), l = by(f, "lsp", n);
    if (!b || !s) continue;
    const S = (k) => `${((1 - s.mb[k] / b.mb[k]) * 100).toFixed(0)}%`;
    const g = (r, k) => (r?.mb[k] === undefined ? "n/a" : `${(r.mb[k] / 1024).toFixed(2)} GiB`);
    console.log(`| ${f} | ${n} | ${g(b, "peak")} | ${g(s, "peak")} | ${S("peak")} | ${g(b, "warm")} | ${g(s, "warm")} | ${S("warm")} | ${g(b, "idle_900")} | ${g(s, "idle_900")} | ${g(l, "peak")} |`);
  }
}
