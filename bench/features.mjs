// Task 0.6: which calls work per layered snapshot, whether they match a dedicated snapshot,
// and how many layered snapshots one tsgo process holds before errors or slowdowns.
// Usage: node features.mjs <fixture> [--max 32] [--stop-gb 14]
import { writeFileSync, mkdirSync } from "node:fs";
import { join, relative } from "node:path";
import { API } from "typescript/async";
import { createFileSystemLayer } from "typescript/fs";
import { BENCH, fixtureInfo, footprintNow, hoverTargets, layerFor } from "./lib.mjs";

const [fixture, ...rest] = process.argv.slice(2);
const opt = (name, def) => { const k = rest.indexOf(`--${name}`); return k >= 0 ? rest[k + 1] : def; };
const MAX = Number(opt("max", 32)), STOP_GB = Number(opt("stop-gb", 14));
const { tsconfigRel, BASE, WTS } = fixtureInfo(fixture, 5);
const time = async (fn) => { const t = performance.now(); try { return { ok: true, value: await fn(), ms: performance.now() - t }; } catch (e) { return { ok: false, error: String(e.message).split("\n")[0], ms: performance.now() - t }; } };

// Each feature returns a string so shared and dedicated answers compare directly.
// Deepest real AST node spanning pos (astnav's touching token can be synthesized, which the API rejects).
const nodeAt = (n, pos) => { let hit; n.forEachChild((c) => { if (c.pos <= pos && pos < c.end) return (hit = c); }); return hit ? nodeAt(hit, pos) : n; };

async function features(snap, cfg, root, t) {
  const project = snap.getConfiguredProject(cfg);
  const file = join(root, t.file), { checker, program, languageService: ls } = project;
  // PathKeys are canonical (lowercased on case-insensitive filesystems), so relativize against the canonical root.
  const rel = (p) => relative(program.getCanonicalFileName(root), String(p));
  const sym = () => checker.getSymbolAtPosition(file, t.pos);
  const node = async () => nodeAt(await program.getSourceFile(file), t.pos);
  return {
    diagnostics: () => program.getSemanticDiagnostics(file).then((d) => d.map((x) => `${x.pos}:${x.code}`).join(",")),
    hover: async () => { const s = await sym(); return s ? `${s.name}: ${await checker.typeToString(await checker.getTypeOfSymbol(s))}` : "-"; },
    definition: async () => { const s = await sym(); if (!s) return "-"; return (await Promise.all(s.declarations.map(async (d) => `${rel(d.path)}@${(await d.resolve(project))?.pos}`))).sort().join(","); },
    references: async () => (await ls.getReferencedSymbolsForNode(await node(), t.pos)).flatMap((e) => e.references.map((r) => `${rel(r.path)}#${r.index}`)).sort().join(","),
    symbols: async () => { const out = []; (await program.getSourceFile(file)).forEachChild((n) => { if (n.name?.text) out.push(n.name.text); }); return out.join(","); },
    completions: async () => {
      const s2 = await snap.update({ openFiles: [file], prepareAutoImports: file });
      try { return String((await s2.getConfiguredProject(cfg).languageService.getCompletionsAtPosition(file, t.pos))?.entries?.length ?? 0); } finally { await s2.dispose(); }
    },
    rename: async () => { if (typeof ls.findRenameLocations !== "function") throw new Error("not exposed by the 7.1 API"); },
  };
}

const api = new API({ cwd: BASE });
const cfg = join(BASE, tsconfigRel);
const base = await api.createSnapshot({ openProjects: [cfg] });
const layers = WTS.map((wt) => layerFor(BASE, wt));
const targets = hoverTargets(BASE, WTS, tsconfigRel, 5);
const layered = (l) => base.update({ fileSystem: createFileSystemLayer(l.files, { removedPaths: l.removed }), ensurePrograms: true });

// ---- A: feature matrix on agent 1, shared (base paths) vs dedicated (own paths, own process) ----
const shared = await layered(layers[0]);
const dedApi = new API({ cwd: WTS[0] });
const dedCfg = join(WTS[0], tsconfigRel);
const ded = await dedApi.createSnapshot({ openProjects: [dedCfg] });
const matrix = {};
for (const t of targets) {
  const s = await features(shared, cfg, BASE, t), d = await features(ded, dedCfg, WTS[0], t);
  for (const name of Object.keys(s)) {
    const [a, b] = [await time(s[name]), await time(d[name])];
    const m = (matrix[name] ??= { works: 0, matches: 0, errors: [], msShared: 0, msDedicated: 0 });
    if (a.ok) m.works++; else m.errors.push(a.error);
    if (a.ok && b.ok && a.value === b.value) m.matches++;
    m.msShared += a.ms / targets.length; m.msDedicated += b.ms / targets.length;
  }
}
for (const m of Object.values(matrix)) m.errors = [...new Set(m.errors)];
await shared.dispose(); await ded.dispose(); await dedApi.close();
console.log(JSON.stringify({ targets: targets.length, matrix }, null, 1));

// ---- B: scaling, N layered snapshots in one process, all running whole-project diagnostics ----
const scaling = [];
for (let n = 1; n <= MAX; n *= 2) {
  const t0 = performance.now();
  const snaps = await Promise.all(Array.from({ length: n }, (_, i) => layered(layers[i % 5])));
  const createMs = performance.now() - t0;
  const diag = await Promise.all(snaps.map((s) => time(() => s.getConfiguredProject(cfg).program.getSemanticDiagnostics())));
  const hover = await Promise.all(snaps.map((s) => time(async () => { const p = s.getConfiguredProject(cfg); const sym = await p.checker.getSymbolAtPosition(join(BASE, targets[0].file), targets[0].pos); return sym?.name; })));
  const row = {
    snapshots: n, createMs: Math.round(createMs),
    diagMsMax: Math.round(Math.max(...diag.map((d) => d.ms))), hoverMsMax: Math.round(Math.max(...hover.map((d) => d.ms))),
    errors: [...new Set([...diag, ...hover].filter((d) => !d.ok).map((d) => d.error))],
    footprintMb: Math.round(footprintNow() / 2 ** 20),
  };
  scaling.push(row);
  console.log(JSON.stringify(row));
  await Promise.all(snaps.map((s) => s.dispose()));
  if (row.errors.length || row.footprintMb > STOP_GB * 1024) break;
}
await base.dispose(); await api.close();
mkdirSync(join(BENCH, "results/raw"), { recursive: true });
writeFileSync(join(BENCH, "results/raw", `${fixture}-features.json`), JSON.stringify({ fixture, matrix, scaling }, null, 1));
process.exit(0);
