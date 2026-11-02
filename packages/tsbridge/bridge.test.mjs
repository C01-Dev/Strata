// Drives bridge.mjs over stdio against a toy project: base snapshot, a layered agent and an
// unshared snapshot of the same worktree, which must agree after path translation.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { after, before, test } from "node:test";

const here = import.meta.dirname;
const tmp = realpathSync(mkdtempSync(join(tmpdir(), "tsbridge-")));
const base = join(tmp, "base"), wt = join(tmp, "wt");
let bridge, next = 0;
const pending = new Map();
const call = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++next;
    pending.set(id, (msg) => (msg.error ? reject(Object.assign(new Error(msg.error.message), msg.error)) : resolve(msg.result)));
    bridge.stdin.write(JSON.stringify({ id, method, params }) + "\n");
  });

function project(dir) {
  mkdirSync(join(dir, "src"), { recursive: true });
  writeFileSync(join(dir, "tsconfig.json"), JSON.stringify({ compilerOptions: { strict: true, target: "es2022", module: "esnext", moduleResolution: "bundler" }, include: ["src"] }));
  writeFileSync(join(dir, "src/a.ts"), "/** Adds one. */\nexport function addOne(n: number): number { return n + 1; }\nexport interface Point { x: number; y: number }\nexport class Box { size = 1; grow(): void {} }\n");
  writeFileSync(join(dir, "src/b.ts"), 'import { addOne, Point } from "./a";\nconst emoji = "😀"; export const total = addOne(2);\nconst p: Point = { x: 1, y: 2 };\n');
  writeFileSync(join(dir, "src/gone.ts"), "export const gone = 1;\n");
  mkdirSync(join(dir, "node_modules"));
  symlinkSync(join(here, "node_modules/typescript"), join(dir, "node_modules/typescript"));
}

before(async () => {
  project(base);
  cpSync(base, wt, { recursive: true, verbatimSymlinks: true });
  // The agent's diff: an error in b.ts, a new file, a deleted file.
  writeFileSync(join(wt, "src/b.ts"), 'import { addOne } from "./a";\nconst emoji = "😀"; export const total: string = addOne(2);\n');
  writeFileSync(join(wt, "src/c.ts"), 'import { total } from "./b";\nexport const again = total;\n');
  rmSync(join(wt, "src/gone.ts"));
  bridge = spawn(process.execPath, [join(here, "bridge.mjs"), base], { stdio: ["pipe", "pipe", "inherit"] });
  createInterface({ input: bridge.stdout }).on("line", (l) => {
    const msg = JSON.parse(l);
    pending.get(msg.id)?.(msg);
    pending.delete(msg.id);
  });
});

after(() => {
  bridge.stdin.end();
  rmSync(tmp, { recursive: true, force: true });
});

const strip = (r, root) => JSON.parse(JSON.stringify(r).replaceAll(root + "/", "<root>/"));

test("hello checks the protocol", async () => {
  assert.deepEqual((await call("hello", { protocol: 1 })).protocol, 1);
  await assert.rejects(call("hello", { protocol: 2 }), { code: "bad_request" });
});

test("layered and unshared snapshots agree", async () => {
  const tsconfig = (root) => [join(root, "tsconfig.json")];
  assert.equal((await call("openBase", { group: "g", root: base, tsconfigs: tsconfig(base) })).files > 3, true);
  const layered = await call("createSnapshot", {
    group: "g",
    agent: "a",
    files: { [join(base, "src/b.ts")]: join(wt, "src/b.ts"), [join(base, "src/c.ts")]: join(wt, "src/c.ts") },
    removed: [join(base, "src/gone.ts")],
  });
  assert.equal(layered.layered, 2);
  await call("openUnshared", { agent: "ref", root: wt, tsconfigs: tsconfig(wt) });

  const both = async (method, params) => [
    strip(await call(method, { agent: "a", ...params(base) }), base),
    strip(await call(method, { agent: "ref", ...params(wt) }), wt),
  ];
  const [d, dRef] = await both("diagnostics", () => ({}));
  assert.deepEqual(d, dRef);
  assert.deepEqual(d.items.map((x) => [x.file, x.line, x.character, x.code, x.category]), [["<root>/src/b.ts", 1, 33, 2322, "error"]]);
  assert.equal(d.items[0].character, 33, "UTF-16 column after an emoji");

  const at = (root) => ({ file: join(root, "src/b.ts"), line: 1, character: 50 }); // `addOne(2)` call
  const [h, hRef] = await both("hover", at);
  assert.deepEqual(h, hRef);
  assert.equal(h.text, "function addOne: (n: number) => number\n\nAdds one.");
  const [def, defRef] = await both("definition", at);
  assert.deepEqual(def, defRef);
  assert.deepEqual(def.locations.map((l) => [l.file, l.line, l.character]), [["<root>/src/a.ts", 1, 16]]);
  const [refs, refsRef] = await both("references", at);
  assert.deepEqual(refs, refsRef);
  assert.equal(refs.locations.length, 3);
  const [syms, symsRef] = await both("symbols", (root) => ({ file: join(root, "src/a.ts") }));
  assert.deepEqual(syms, symsRef);
  assert.deepEqual(syms.symbols.map((s) => [s.name, s.kind, s.children?.map((c) => c.name)]), [
    ["addOne", "FunctionDeclaration", undefined],
    ["Point", "InterfaceDeclaration", ["x", "y"]],
    ["Box", "ClassDeclaration", ["size", "grow"]],
  ]);
  const [one, oneRef] = await both("diagnostics", (root) => ({ file: join(root, "src/c.ts") }));
  assert.deepEqual(one, oneRef);
  assert.equal(one.total, 0);
});

test("errors carry codes", async () => {
  await assert.rejects(call("diagnostics", { agent: "a", file: join(tmp, "elsewhere.ts") }), { code: "not_in_project" });
  await assert.rejects(call("hover", { agent: "nobody", file: "x", line: 0, character: 0 }), { code: "bad_request" });
  await assert.rejects(call("nope"), { code: "bad_request" });
});

test("a new base keeps old agents working until they move", async () => {
  const before = (await call("stats")).snapshots;
  await call("openBase", { group: "g", root: base, tsconfigs: [join(base, "tsconfig.json")] });
  assert.equal((await call("stats")).snapshots, before + 1, "old base retired, not disposed");
  assert.equal((await call("diagnostics", { agent: "a" })).total, 1);
  await call("updateSnapshot", { agent: "a", files: {}, removed: [] });
  assert.equal((await call("stats")).snapshots, before, "old base disposed once its last agent moved");
  assert.equal((await call("diagnostics", { agent: "a" })).total, 0, "empty layer = base");
  await call("dispose", { group: "g" });
  await call("dispose", { agent: "ref" });
  assert.equal((await call("stats")).snapshots, 0);
});
