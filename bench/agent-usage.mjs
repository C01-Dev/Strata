// Tallies what the agent did in each log written by bench/agent-usage.sh (Claude Code's
// stream-json, or Codex's `exec --json`): Strata MCP calls versus type checks it ran itself.
//   node bench/agent-usage.mjs bench/results/agent-usage/<run>/*.jsonl
import { readFileSync } from "node:fs";
import { basename } from "node:path";

const TYPECHECK = /\b(tsc|tsgo)\b|type-?check/;
const OTHER_CHECK = /\b(vitest|jest|eslint|prettier)\b|\b(yarn|npm|pnpm|npx)( run)? (build|lint|test)\b/;

const rows = [];
for (const file of process.argv.slice(2)) {
  const strataCall = (t) => (row.strataCalls[t] = (row.strataCalls[t] ?? 0) + 1);
  const shell = (cmd) => {
    if (TYPECHECK.test(cmd)) row.typecheck.push(cmd);
    else if (OTHER_CHECK.test(cmd)) row.otherChecks.push(cmd);
  };
  const row = { log: basename(file, ".jsonl"), model: "?", strata: "?", calls: 0, strataCalls: {}, typecheck: [], otherChecks: [], turns: 0, secs: 0 };
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (!line.trim()) continue;
    let m;
    try { m = JSON.parse(line); } catch { continue; }
    if (m.type === "system" && m.subtype === "init") {
      row.model = m.model;
      row.strata = m.mcp_servers?.find((s) => s.name === "strata")?.status ?? "absent";
    } else if (m.type === "assistant") {
      for (const c of m.message?.content ?? []) {
        if (c.type !== "tool_use") continue;
        row.calls++;
        if (c.name.startsWith("mcp__strata__")) strataCall(c.name.slice("mcp__strata__".length));
        else if (c.name === "Bash") shell(String(c.input?.command ?? ""));
      }
    } else if (m.type === "thread.started") {
      row.model = "codex";
    } else if (m.type === "item.completed" && m.item) {
      const it = m.item;
      if (it.type === "mcp_tool_call") {
        row.calls++;
        if (it.server === "strata") { row.strata = "connected"; strataCall(it.tool); }
      } else if (it.type === "command_execution") {
        row.calls++;
        shell(String(it.command ?? ""));
      }
    } else if (m.type === "turn.completed") {
      row.turns++;
    } else if (m.type === "result") {
      row.turns = m.num_turns ?? 0;
      row.secs = Math.round((m.duration_ms ?? 0) / 1000);
    }
  }
  rows.push(row);
}

for (const r of rows) {
  const strata = Object.entries(r.strataCalls).map(([t, n]) => `${t} x${n}`).join(", ") || "none";
  console.log(`\n## ${r.log}  (${r.model}, strata ${r.strata}, ${r.calls} tool calls, ${r.turns} turns, ${r.secs}s)`);
  console.log(`  Strata tools: ${strata}`);
  console.log(`  Own type checks: ${r.typecheck.length}${r.typecheck.map((c) => `\n    $ ${c.split("\n")[0].slice(0, 140)}`).join("")}`);
  console.log(`  Tests/lint/build: ${r.otherChecks.length}${r.otherChecks.map((c) => `\n    $ ${c.split("\n")[0].slice(0, 140)}`).join("")}`);
}
const used = rows.filter((r) => Object.keys(r.strataCalls).length).length;
const own = rows.filter((r) => r.typecheck.length).length;
console.log(`\nSessions: ${rows.length}. Used a Strata tool: ${used}. Ran their own type check: ${own}.`);
