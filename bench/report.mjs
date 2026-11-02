// Task 0.7: roll every run summary into results/phase0.csv and print the Gate 0 table.
// S = 1 - shared / baseline, on peak and warm readings, per workload and agent count.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const RAW = join(import.meta.dirname, "results/raw");
const runs = readdirSync(RAW).filter((f) => /^[a-z]+-(baseline|shared|lsp)-\d+(-files)?\.json$/.test(f))
  .map((f) => ({ workload: "whole", ...JSON.parse(readFileSync(join(RAW, f), "utf8")).summary }));

const cols = ["fixture", "workload", "mode", "agents", "harness", "peak", "warm", "idle_60", "idle_900"];
const mb = (r, k) => (r.mb[k] === undefined ? "" : r.mb[k].toFixed(0));
const secs = (r) => ["loaded", "diagnostics_done", "hover_done"].map((k) => r.seconds[k] ?? "");
const csv = [[...cols.map((c) => (["fixture", "workload", "mode", "agents"].includes(c) ? c : `${c}_mb`)), "loaded_s", "diagnostics_done_s", "hover_done_s", "typescript", "cpu", "ram_gb", "os", "fs"].join(",")];
for (const r of runs.sort((a, b) => a.tag.localeCompare(b.tag))) {
  csv.push([r.fixture, r.workload, r.mode, r.agents, ...cols.slice(4).map((k) => mb(r, k)), ...secs(r), r.typescript, `"${r.machine.cpu}"`, r.machine.ramGb.toFixed(0), `"${r.machine.os}"`, r.machine.fs].join(","));
}
writeFileSync(join(import.meta.dirname, "results/phase0.csv"), csv.join("\n") + "\n");

console.log("| Fixture | Workload | Agents | Baseline peak | Shared peak | S peak | Baseline warm | Shared warm | S warm | Baseline idle 15 min | Shared idle 15 min | LSP peak | S peak vs LSP |");
console.log("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
const by = (f, w, m, n) => runs.find((r) => r.fixture === f && r.workload === w && r.mode === m && r.agents === n);
for (const f of [...new Set(runs.map((r) => r.fixture))]) {
  for (const w of ["whole", "files"]) {
    for (const n of [...new Set(runs.filter((r) => r.fixture === f && r.workload === w).map((r) => r.agents))].sort((a, b) => a - b)) {
      const b = by(f, w, "baseline", n), s = by(f, w, "shared", n), l = by(f, w, "lsp", n);
      if (!b || !s) continue;
      const S = (k, o = b) => (o?.mb[k] === undefined ? "n/a" : `${((1 - s.mb[k] / o.mb[k]) * 100).toFixed(0)}%`);
      const g = (r, k) => (r?.mb[k] === undefined ? "n/a" : `${(r.mb[k] / 1024).toFixed(2)} GiB`);
      console.log(`| ${f} | ${w} | ${n} | ${g(b, "peak")} | ${g(s, "peak")} | ${S("peak")} | ${g(b, "warm")} | ${g(s, "warm")} | ${S("warm")} | ${g(b, "idle_900")} | ${g(s, "idle_900")} | ${g(l, "peak")} | ${w === "files" ? S("peak", l) : "n/a"} |`);
    }
  }
}
