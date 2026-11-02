// Strata tsbridge: the daemon's link to tsgo, one bridge (and one tsgo process) per checker group.
// Usage: node bridge.mjs <repo root>   (typescript/async is loaded from that repo's node_modules)
//
// Protocol: JSON lines over stdio. Request {id, method, params}; response {id, result} or
// {id, error: {code, message}} with code not_in_project | restarting | bad_request | internal.
// Paths are absolute: base paths for layered agents, the worktree's own paths for unshared ones;
// translation happens in Rust. Positions are 0-based lines and UTF-16 characters.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const PROTOCOL = 1;
const CATEGORY = ["warning", "error", "suggestion", "message"];

const repoRequire = createRequire(join(process.argv[2] ?? process.cwd(), "package.json"));
const load = (spec) => import(pathToFileURL(repoRequire.resolve(spec)).href);
const [{ API, SymbolFlags }, { createFileSystemLayer }, { SyntaxKind }] = await Promise.all([
  load("typescript/async"),
  load("typescript/fs"),
  load("typescript/ast"),
]);
const typescript = repoRequire("typescript/package.json").version;

class BridgeError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

let api;
/** group -> { root, tsconfigs, base, retired: Map<baseSnapshot, number of agents still on it> } */
const groups = new Map();
/** agent -> { group, snap, from (base snapshot it derives from, or null when unshared), tsconfigs } */
const agents = new Map();

function tsApi(root) {
  api ??= new API({ cwd: root });
  return api;
}

function release(base, group) {
  if (!group || base === group.base) return;
  const left = (group.retired.get(base) ?? 1) - 1;
  if (left > 0) return group.retired.set(base, left);
  group.retired.delete(base);
  base.dispose();
}

async function disposeAgent(name) {
  const a = agents.get(name);
  if (!a) return;
  agents.delete(name);
  await a.snap.dispose();
  if (a.from) release(a.from, groups.get(a.group));
}

function agent(name) {
  const a = agents.get(name);
  if (!a) throw new BridgeError("bad_request", `no snapshot for agent ${name}`);
  return a;
}

async function projectFor(a, file) {
  const project = await a.snap.getDefaultProjectForFile(file);
  if (!project) throw new BridgeError("not_in_project", `${file} is not in any configured project`);
  return project;
}

/** Source text for line maps: program file, else tsconfig, else nothing. */
async function sourceFile(program, file) {
  if (!file) return undefined;
  return (await program.getSourceFile(file)) ?? (await program.getConfigSourceFile(file).catch(() => undefined));
}

function loc(sf, start, end) {
  const a = sf.getLineAndCharacterOfPosition(start), b = sf.getLineAndCharacterOfPosition(end);
  return { file: sf.fileName, line: a.line, character: a.character, endLine: b.line, endCharacter: b.character };
}

function nodeLoc(node) {
  const named = node.name ?? node;
  return loc(named.getSourceFile(), named.getStart(), named.end);
}

/** TypeScript's flattenDiagnosticMessageText: the chain, indented two spaces per level. */
function flatten(d, indent = 0) {
  let text = (indent ? "\n" + "  ".repeat(indent) : "") + d.text;
  for (const next of d.messageChain ?? []) text += flatten(next, indent + 1);
  return text;
}

async function diagnostic(program, cache, d) {
  if (!cache.has(d.fileName)) cache.set(d.fileName, await sourceFile(program, d.fileName));
  const sf = cache.get(d.fileName);
  const at = sf ? loc(sf, d.pos, d.end) : { file: d.fileName ?? "", line: 0, character: 0, endLine: 0, endCharacter: 0 };
  return { ...at, code: d.code, category: CATEGORY[d.category] ?? "error", message: flatten(d) };
}

const byLocation = (a, b) =>
  a.file.localeCompare(b.file) || a.line - b.line || a.character - b.character || a.endLine - b.endLine || a.endCharacter - b.endCharacter;

function sortUnique(items, key = JSON.stringify) {
  const seen = new Set();
  return items.filter((x) => !seen.has(key(x)) && seen.add(key(x))).sort(byLocation);
}

/** Deepest node whose span contains pos (astnav's touching token can be synthesized, which the API rejects). */
function nodeAt(node, pos) {
  let hit;
  node.forEachChild((c) => {
    if (c.pos <= pos && pos < c.end) return (hit = c);
  });
  return hit ? nodeAt(hit, pos) : node;
}

async function position(a, file, line, character) {
  const project = await projectFor(a, file);
  const sf = await project.program.getSourceFile(file);
  if (!sf) throw new BridgeError("not_in_project", `${file} is not in any configured project`);
  return { project, sf, pos: sf.getPositionOfLineAndCharacter(line, character) };
}

async function symbolAt(project, file, pos) {
  const sym = await project.checker.getSymbolAtPosition(file, pos);
  if (sym && sym.flags & SymbolFlags.Alias) return project.checker.getAliasedSymbol(sym);
  return sym;
}

const KEYWORDS = [
  [SymbolFlags.Class, "class"],
  [SymbolFlags.Interface, "interface"],
  [SymbolFlags.TypeAlias, "type"],
  [SymbolFlags.Enum, "enum"],
  [SymbolFlags.Module, "namespace"],
  [SymbolFlags.Function, "function"],
  [SymbolFlags.Method, "method"],
  [SymbolFlags.Property, "property"],
  [SymbolFlags.TypeParameter, "type parameter"],
  [SymbolFlags.Variable, "var"],
];

const methods = {
  hello({ protocol }) {
    if (protocol !== PROTOCOL) throw new BridgeError("bad_request", `bridge speaks protocol ${PROTOCOL}, not ${protocol}`);
    return { protocol: PROTOCOL, typescript, pid: process.pid };
  },

  /** Opens (or replaces) a group's base snapshot. Agents on a replaced base keep it alive until they move. */
  async openBase({ group, root, tsconfigs }) {
    const base = await tsApi(root).createSnapshot({ openProjects: tsconfigs });
    const g = groups.get(group);
    if (g) {
      const users = [...agents.values()].filter((a) => a.from === g.base).length;
      if (users) g.retired.set(g.base, users);
      else await g.base.dispose();
    }
    groups.set(group, { root, tsconfigs, base, retired: g?.retired ?? new Map() });
    const files = new Set();
    for (const p of base.getProjects()) for (const f of await p.program.getSourceFileNames()) files.add(f);
    return { files: files.size };
  },

  /** Full layer each time: files maps base path -> worktree path to read the content from. */
  async createSnapshot({ agent: name, group = agents.get(name)?.group, files, removed }) {
    const g = groups.get(group);
    if (!g) throw new BridgeError("bad_request", `no base for group ${group}`);
    const entries = Object.entries(files).map(([basePath, from]) => [basePath, readFileSync(from, "utf8")]);
    const layer = createFileSystemLayer(entries, { removedPaths: removed });
    const snap = await g.base.update({ fileSystem: layer, ensurePrograms: true });
    await disposeAgent(name);
    agents.set(name, { group, snap, from: g.base, tsconfigs: g.tsconfigs });
    return { layered: entries.length };
  },

  updateSnapshot(params) {
    return methods.createSnapshot(params);
  },

  /** A snapshot at the worktree's own paths (dependencies differ, a huge diff, or a parity reference). */
  async openUnshared({ agent: name, root, tsconfigs }) {
    const snap = await tsApi(root).createSnapshot({ openProjects: tsconfigs });
    await disposeAgent(name);
    agents.set(name, { group: null, snap, from: null, tsconfigs });
    return {};
  },

  async dispose({ agent: name, group }) {
    if (name) await disposeAgent(name);
    if (group) {
      for (const [n, a] of agents) if (a.group === group) await disposeAgent(n);
      const g = groups.get(group);
      groups.delete(group);
      if (g) for (const b of [g.base, ...g.retired.keys()]) await b.dispose();
    }
    return {};
  },

  async diagnostics({ agent: name, file, limit }) {
    const a = agent(name);
    let items = [];
    if (file) {
      const { program } = await projectFor(a, file);
      const cache = new Map();
      for (const d of [...(await program.getSyntacticDiagnostics(file)), ...(await program.getSemanticDiagnostics(file))]) {
        items.push(await diagnostic(program, cache, d));
      }
    } else {
      for (const { program } of a.snap.getProjects()) {
        const cache = new Map();
        const all = [
          ...(await program.getConfigFileParsingDiagnostics()),
          ...(await program.getGlobalDiagnostics()),
          ...(await program.getSyntacticDiagnostics()),
          ...(await program.getSemanticDiagnostics()),
        ];
        for (const d of all) items.push(await diagnostic(program, cache, d));
      }
    }
    items = sortUnique(items);
    return { items: limit === undefined ? items : items.slice(0, limit), total: items.length };
  },

  async hover({ agent: name, file, line, character }) {
    const { project, pos } = await position(agent(name), file, line, character);
    const sym = await symbolAt(project, file, pos);
    if (!sym) return null;
    const { checker } = project;
    const keyword = KEYWORDS.find(([flag]) => sym.flags & flag)?.[1];
    let text;
    if (sym.flags & (SymbolFlags.Interface | SymbolFlags.Class | SymbolFlags.Enum | SymbolFlags.Module)) {
      text = `${keyword} ${sym.name}`;
    } else if (sym.flags & (SymbolFlags.TypeAlias | SymbolFlags.TypeParameter)) {
      text = `${keyword} ${sym.name} = ${await checker.typeToString(await checker.getDeclaredTypeOfSymbol(sym))}`;
    } else {
      text = `${keyword ? keyword + " " : ""}${sym.name}: ${await checker.typeToString(await checker.getTypeOfSymbol(sym))}`;
    }
    const doc = await sym.getDocumentationComment(checker);
    return { text: doc ? `${text}\n\n${doc}` : text };
  },

  async definition({ agent: name, file, line, character }) {
    const { project, pos } = await position(agent(name), file, line, character);
    const sym = await symbolAt(project, file, pos);
    const locations = [];
    for (const decl of sym?.declarations ?? []) {
      const node = await decl.resolve(project);
      if (node) locations.push(nodeLoc(node));
    }
    return { locations: sortUnique(locations) };
  },

  async references({ agent: name, file, line, character }) {
    const { project, sf, pos } = await position(agent(name), file, line, character);
    const entries = await project.languageService.getReferencedSymbolsForNode(nodeAt(sf, pos), pos);
    const locations = [];
    for (const handle of entries.flatMap((e) => e.references)) {
      const node = await handle.resolve(project);
      if (node) locations.push(loc(node.getSourceFile(), node.getStart(), node.end));
    }
    return { locations: sortUnique(locations) };
  },

  /** Top-level declarations, with members of classes, interfaces, enums and namespaces one level down. */
  async symbols({ agent: name, file }) {
    const { program } = await projectFor(agent(name), file);
    const sf = await program.getSourceFile(file);
    const symbol = (node) => {
      const sym = { name: node.name.text ?? node.name.getText?.() ?? "", kind: SyntaxKind[node.kind], loc: nodeLoc(node) };
      const members = node.members ?? (node.kind === SyntaxKind.ModuleDeclaration ? node.body?.statements : undefined);
      if (members) sym.children = outline(members, false);
      return sym;
    };
    const outline = (nodes, deep) => {
      const out = [];
      for (const n of nodes) {
        if (n.kind === SyntaxKind.VariableStatement) {
          for (const d of n.declarationList.declarations) if (d.name?.text) out.push({ name: d.name.text, kind: SyntaxKind[d.kind], loc: nodeLoc(d) });
        } else if (n.name?.text !== undefined) {
          out.push(deep ? symbol(n) : { name: n.name.text, kind: SyntaxKind[n.kind], loc: nodeLoc(n) });
        }
      }
      return out;
    };
    return { symbols: outline(sf.statements, true) };
  },

  stats() {
    let snapshots = agents.size;
    for (const g of groups.values()) snapshots += 1 + g.retired.size;
    return { snapshots, memoryBytes: process.memoryUsage().rss };
  },
};

const send = (msg) => process.stdout.write(JSON.stringify(msg) + "\n");

createInterface({ input: process.stdin }).on("line", async (line) => {
  let req;
  try {
    req = JSON.parse(line);
    const method = methods[req.method];
    if (!method) throw new BridgeError("bad_request", `unknown method ${req.method}`);
    send({ id: req.id, result: (await method(req.params ?? {})) ?? null });
  } catch (e) {
    const code = e instanceof BridgeError ? e.code : "internal";
    send({ id: req?.id ?? null, error: { code, message: String(e?.message ?? e).split("\n")[0] } });
  }
}).on("close", async () => {
  await api?.close().catch(() => {});
  process.exit(0);
});
