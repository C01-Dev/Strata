// Builds Strata's npm packages from release binaries (PLAN 5.3):
//   node npm/build.mjs <version> <out-dir> <platform>=<binary> [<platform>=<binary> ...]
// e.g. darwin-arm64=dist/strata-aarch64-apple-darwin/strata. Platforms use Node's names
// (`${process.platform}-${process.arch}`): darwin-arm64, linux-x64, linux-arm64.
// Writes <out>/strata (package `stratagents`, the launcher, with every binary's SHA-256 inside)
// and <out>/strata-<platform> (package `stratagents-<platform>`). Plain names, not a scope, so
// the packages can change hands later.
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const NAME = "stratagents";
const here = dirname(fileURLToPath(import.meta.url));
const [version, out, ...builds] = process.argv.slice(2);
if (!version || !out || builds.length === 0) {
  console.error("usage: node npm/build.mjs <version> <out-dir> <platform>=<binary>...");
  process.exit(2);
}
const common = {
  version,
  description: "Shared type checker, dependency seeding and dev-server routing for parallel coding agents",
  repository: "https://github.com/C01-Dev/Strata",
  license: "MIT OR Apache-2.0",
};
const sums = {};
for (const build of builds) {
  const [platform, binary] = build.split("=");
  const [os, cpu] = platform.split("-");
  const dir = join(out, `strata-${platform}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "bin"), { recursive: true });
  copyFileSync(binary, join(dir, "bin", "strata"));
  chmodSync(join(dir, "bin", "strata"), 0o755);
  sums[platform] = createHash("sha256").update(readFileSync(binary)).digest("hex");
  const pkg = { name: `${NAME}-${platform}`, ...common, os: [os], cpu: [cpu], files: ["bin/strata"] };
  writeFileSync(join(dir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
}
const dir = join(out, "strata");
rmSync(dir, { recursive: true, force: true });
mkdirSync(join(dir, "bin"), { recursive: true });
const launcher = readFileSync(join(here, "launcher.js"), "utf8")
  .replace("@@NAME@@", NAME)
  .replace("/* @@SHA256@@ */ {}", JSON.stringify(sums));
writeFileSync(join(dir, "bin", "strata.js"), launcher);
chmodSync(join(dir, "bin", "strata.js"), 0o755);
const pkg = {
  name: NAME,
  ...common,
  bin: { strata: "bin/strata.js" },
  files: ["bin/strata.js"],
  engines: { node: ">=20" },
  optionalDependencies: Object.fromEntries(Object.keys(sums).map((p) => [`${NAME}-${p}`, version])),
};
writeFileSync(join(dir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
console.log(`${out}: strata ${version} for ${Object.keys(sums).join(", ")}`);
