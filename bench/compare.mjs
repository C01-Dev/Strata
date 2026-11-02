// Task 0.5: shared-checker answers must equal dedicated-per-worktree answers, as sets, after path translation.
// Usage: node compare.mjs <fixture> [agents] [workload suffix, e.g. files]
import { readFileSync } from "node:fs";
import { join } from "node:path";

const [fixture, agents = "5", suffix] = process.argv.slice(2);
const load = (mode) => JSON.parse(readFileSync(join(import.meta.dirname, "results/raw", `${fixture}-${mode}-${agents}${suffix ? `-${suffix}` : ""}.json`), "utf8"));
const base = load("baseline"), shared = load("shared");

const report = { fixture, agents: Number(agents), workload: suffix ?? "whole", perAgent: [], mismatches: 0 };
for (let i = 0; i < base.diags.length; i++) {
  const row = { agent: i + 1 };
  for (const kind of ["diags", "hovers"]) {
    const a = new Set(base[kind][i]), b = new Set(shared[kind][i]);
    const onlyBaseline = [...a].filter((x) => !b.has(x)), onlyShared = [...b].filter((x) => !a.has(x));
    row[kind] = { baseline: a.size, shared: b.size, onlyBaseline: onlyBaseline.slice(0, 5), onlyShared: onlyShared.slice(0, 5) };
    report.mismatches += onlyBaseline.length + onlyShared.length;
  }
  row.distinctFromAgent1 = i > 0 && base.diags[i].join("\n") !== base.diags[0].join("\n");
  report.perAgent.push(row);
}
console.log(JSON.stringify(report, null, 1));
process.exit(report.mismatches ? 1 : 0);
