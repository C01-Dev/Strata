// Scripted "agent mid-task" diff for one worktree: 5 files edited (one with a type error),
// 1 file added, 1 file deleted. Deterministic per agent index.
// Usage: node diff.mjs <worktree> <tsconfig relative path> <agent index>
import { execFileSync } from "node:child_process";
import { appendFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, extname, join } from "node:path";

const [wt, tsconfig, idx] = process.argv.slice(2);
const i = Number(idx);
const root = dirname(tsconfig) === "." ? "" : dirname(tsconfig) + "/";
const skip = /(^|\/)(examples|tests?|__tests__|playwright|dist|build|scripts)\/|\.d\.ts$|\.(test|spec|stories)\.tsx?$/;

const files = execFileSync("git", ["-C", wt, "ls-files", "-z", "--", `${root}*.ts`, `${root}*.tsx`], { encoding: "utf8" })
  .split("\0")
  .filter((f) => f && !skip.test(f.slice(root.length)))
  .sort();

// mulberry32: small seeded PRNG so every run picks the same files for agent i.
let seed = 0x5eed + i * 7919;
const rand = () => {
  seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const picked = new Set();
while (picked.size < 6) picked.add(files[Math.floor(rand() * files.length)]);
const [first, ...rest] = [...picked];
const edits = [first, ...rest.slice(0, 4)];
const removed = rest[4];

edits.forEach((f, k) => {
  let code = `\nexport function strataAgent${i}Helper${k}(input: string): number {\n  return input.length * ${i} + ${k};\n}\n`;
  if (k === 0) code += `export const strataAgent${i}Broken: number = "agent-${i}";\n`;
  appendFileSync(join(wt, f), code);
});
const added = join(dirname(first), `strataAgent${i}New.ts`);
const spec = "./" + basename(first, extname(first));
writeFileSync(join(wt, added), `import { strataAgent${i}Helper0 } from "${spec}";\n\nexport const strataAgent${i}Value: number = strataAgent${i}Helper0("agent");\n`);
rmSync(join(wt, removed));

console.log(JSON.stringify({ agent: i, candidates: files.length, edited: edits, added, removed }));
