// Phase 0 harness. One mode per process so the footprint sampler sees only that mode's tree.
//   baseline: one tsgo API process per worktree, each holding a snapshot of its own worktree
//   shared:   one tsgo API process, a base snapshot plus one layered snapshot per worktree (base paths)
//   lsp:      one `tsc --lsp --stdio` per worktree (launch comparison; opens the hover files only)
// Usage: node run.mjs <mode> <fixture> [--agents 5] [--idle 60,900] [--tag name]
import { execSync, spawn } from "node:child_process";
import os from "node:os";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { API } from "typescript/async";
import { createFileSystemLayer } from "typescript/fs";
import { BENCH, TSC, fixtureInfo, hoverTargets as targetsFor, layerFor as layerAt, walk } from "./lib.mjs";

const [mode, fixture, ...rest] = process.argv.slice(2);
const opt = (name, def) => { const k = rest.indexOf(`--${name}`); return k >= 0 ? rest[k + 1] : def; };
const AGENTS = Number(opt("agents", 5));
const IDLE = opt("idle", "60").split(",").map(Number);
const TAG = opt("tag", `${fixture}-${mode}-${AGENTS}`);
const { tsconfigRel, BASE, WTS } = fixtureInfo(fixture, AGENTS);
const OUT = join(BENCH, "results", "raw");
mkdirSync(OUT, { recursive: true });

// ---- memory sampler: phys_footprint summed over this process and its children ----
const samples = [];
const sampler = spawn(join(BENCH, "footprint"), [String(process.pid), "250"]);
let buf = "";
sampler.stdout.on("data", (d) => {
  buf += d;
  let nl;
  while ((nl = buf.indexOf("\n")) >= 0) {
    const [t, bytes, n] = buf.slice(0, nl).split(",").map(Number);
    samples.push({ t, bytes, n });
    buf = buf.slice(nl + 1);
  }
});
const marks = {};
const mark = (name) => { marks[name] = Date.now(); console.error(`[${TAG}] ${name} +${((marks[name] - marks.start) / 1000 || 0).toFixed(1)}s`); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const hoverTargets = () => targetsFor(BASE, WTS, tsconfigRel);
const layerFor = (wt, baseFiles) => layerAt(BASE, wt, baseFiles);

// ---- normalizers (paths relative to the agent's own root, so modes compare) ----
const norm = (root, s) => (s ?? "").split(root + sep).join("<root>/");
const flatten = (d) => [d.text, ...(d.messageChain ?? []).flatMap(flatten)];
const diagKey = (root, d) =>
  `${d.fileName ? relative(root, d.fileName) : "-"}|${d.pos}-${d.end}|TS${d.code}|${norm(root, flatten(d).join(" / "))}`;

async function queries(agents) {
  mark("diagnostics_start");
  const diags = await Promise.all(agents.map(async (a) => {
    const p = a.project.program;
    const all = [
      ...(await p.getConfigFileParsingDiagnostics()),
      ...(await p.getGlobalDiagnostics()),
      ...(await p.getSyntacticDiagnostics()),
      ...(await p.getSemanticDiagnostics()),
    ];
    return all.map((d) => diagKey(a.root, d)).sort();
  }));
  mark("diagnostics_done");
  const targets = hoverTargets();
  const hovers = await Promise.all(agents.map(async (a) => {
    const out = [];
    for (const t of targets) {
      try {
        const sym = await a.project.checker.getSymbolAtPosition(join(a.root, t.file), t.pos);
        if (!sym) { out.push(`${t.file}:${t.pos} -`); continue; }
        const type = await a.project.checker.typeToString(await a.project.checker.getTypeOfSymbol(sym));
        out.push(`${t.file}:${t.pos} ${norm(a.root, sym.name)}: ${norm(a.root, type)}`);
      } catch (e) {
        out.push(`${t.file}:${t.pos} error: ${norm(a.root, String(e.message).split("\n")[0])}`);
      }
    }
    return out;
  }));
  mark("hover_done");
  return { diags, hovers, targets };
}

// ---- modes ----
let result, cleanup;
mark("start");
await sleep(500); // baseline sample of the bare harness
if (mode === "baseline") {
  const agents = await Promise.all(WTS.map(async (root) => {
    const api = new API({ cwd: root });
    const cfg = join(root, tsconfigRel);
    const snap = await api.createSnapshot({ openProjects: [cfg] });
    return { root, api, snap, project: snap.getConfiguredProject(cfg) };
  }));
  mark("loaded");
  result = await queries(agents);
  cleanup = () => Promise.all(agents.map(async (a) => { await a.snap.dispose(); await a.api.close(); }));
} else if (mode === "shared") {
  const api = new API({ cwd: BASE });
  const cfg = join(BASE, tsconfigRel);
  const base = await api.createSnapshot({ openProjects: [cfg] });
  mark("base_loaded");
  const baseFiles = walk(BASE);
  const layers = WTS.map((wt) => layerFor(wt, baseFiles));
  mark("layers_computed");
  // Paths stay at base paths inside tsgo; results are reported relative to BASE, i.e. re-rooted per agent.
  const agents = await Promise.all(layers.map(async (l, i) => {
    const snap = await base.update({ fileSystem: createFileSystemLayer(l.files, { removedPaths: l.removed }), ensurePrograms: true });
    return { root: BASE, wt: WTS[i], snap, project: snap.getConfiguredProject(cfg), layered: l.files.length, removed: l.removed.length };
  }));
  mark("loaded");
  result = await queries(agents);
  result.layers = agents.map((a) => ({ layered: a.layered, removed: a.removed }));
  cleanup = async () => { for (const a of agents) await a.snap.dispose(); await base.dispose(); await api.close(); };
} else if (mode === "lsp") {
  const targets = hoverTargets();
  const servers = await Promise.all(WTS.map((root) => lspSession(root, targets)));
  mark("loaded");
  result = { hovers: await Promise.all(servers.map((s) => s.run())) };
  mark("hover_done");
  cleanup = () => Promise.all(servers.map((s) => s.close()));
} else throw new Error(`unknown mode ${mode}`);

// ---- warm and idle readings ----
const done = marks.hover_done;
await sleep(10_000); mark("warm");
for (const s of IDLE) { await sleep(Math.max(0, done + s * 1000 - Date.now())); mark(`idle_${s}`); }
await cleanup();
sampler.kill();

const at = (t) => samples.reduce((best, s) => (Math.abs(s.t - t) < Math.abs(best.t - t) ? s : best), samples[0]).bytes;
const peak = Math.max(...samples.filter((s) => s.t <= done + 10_000).map((s) => s.bytes));
const summary = {
  tag: TAG, mode, fixture, agents: AGENTS, typescript: JSON.parse(readFileSync(join(BENCH, "node_modules/typescript/package.json"))).version,
  gogc: process.env.GOGC ?? "default", gomemlimit: process.env.GOMEMLIMIT ?? "default",
  mb: { harness: at(marks.start + 400) / 2 ** 20, peak: peak / 2 ** 20, warm: at(marks.warm) / 2 ** 20, ...Object.fromEntries(IDLE.map((s) => [`idle_${s}`, at(marks[`idle_${s}`]) / 2 ** 20])) },
  seconds: Object.fromEntries(Object.entries(marks).map(([k, v]) => [k, (v - marks.start) / 1000])),
  maxProcs: Math.max(...samples.map((s) => s.n)),
  machine: {
    cpu: os.cpus()[0].model, ramGb: os.totalmem() / 2 ** 30, os: `${os.type()} ${os.release()}`,
    fs: execSync(`diskutil info "$(df "${BASE}" | tail -1 | cut -d' ' -f1)" | sed -n 's/^ *Type (Bundle): *//p'`, { encoding: "utf8" }).trim(),
  },
};
writeFileSync(join(OUT, `${TAG}.json`), JSON.stringify({ summary, ...result }, null, 1));
writeFileSync(join(OUT, `${TAG}-mem.csv`), "t_ms,bytes,nprocs\n" + samples.map((s) => `${s.t - marks.start},${s.bytes},${s.n}`).join("\n") + "\n");
console.log(JSON.stringify(summary));
process.exit(0);

// ---- minimal LSP client ----
function lspSession(root, targets) {
  const p = spawn(TSC, ["--lsp", "--stdio"], { cwd: root, stdio: ["pipe", "pipe", "ignore"] });
  let id = 0, data = Buffer.alloc(0);
  const pending = new Map();
  const send = (msg) => { const b = JSON.stringify({ jsonrpc: "2.0", ...msg }); p.stdin.write(`Content-Length: ${Buffer.byteLength(b)}\r\n\r\n${b}`); };
  const request = (method, params) => new Promise((res) => { const i = ++id; pending.set(i, res); send({ id: i, method, params }); });
  p.stdout.on("data", (d) => {
    data = Buffer.concat([data, d]);
    for (;;) {
      const h = data.indexOf("\r\n\r\n"); if (h < 0) return;
      const len = Number(/Content-Length: (\d+)/.exec(data.slice(0, h).toString())[1]);
      if (data.length < h + 4 + len) return;
      const msg = JSON.parse(data.slice(h + 4, h + 4 + len).toString()); data = data.slice(h + 4 + len);
      if (msg.method && msg.id !== undefined) send({ id: msg.id, result: msg.method === "workspace/configuration" ? (msg.params.items ?? []).map(() => null) : null });
      else if (msg.id !== undefined && pending.has(msg.id)) { pending.get(msg.id)(msg.result); pending.delete(msg.id); }
    }
  });
  const uri = (f) => pathToFileURL(join(root, f)).href;
  const lc = (text, pos) => { const lines = text.slice(0, pos).split("\n"); return { line: lines.length - 1, character: lines.at(-1).length }; };
  return (async () => {
    await request("initialize", { processId: process.pid, rootUri: pathToFileURL(root).href, workspaceFolders: [{ uri: pathToFileURL(root).href, name: "wt" }], capabilities: { textDocument: { hover: { contentFormat: ["plaintext"] }, diagnostic: {} } } });
    send({ method: "initialized", params: {} });
    const texts = new Map();
    for (const t of targets) if (!texts.has(t.file)) {
      texts.set(t.file, readFileSync(join(root, t.file), "utf8"));
      send({ method: "textDocument/didOpen", params: { textDocument: { uri: uri(t.file), languageId: t.file.endsWith("x") ? "typescriptreact" : "typescript", version: 1, text: texts.get(t.file) } } });
    }
    return {
      run: async () => {
        for (const f of texts.keys()) await request("textDocument/diagnostic", { textDocument: { uri: uri(f) } });
        const out = [];
        for (const t of targets) { const h = await request("textDocument/hover", { textDocument: { uri: uri(t.file) }, position: lc(texts.get(t.file), t.pos) }); out.push(`${t.file}:${t.pos} ${h ? "hover" : "-"}`); }
        return out;
      },
      close: async () => { await request("shutdown", null); send({ method: "exit" }); },
    };
  })();
}
