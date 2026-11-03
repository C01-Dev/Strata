#!/usr/bin/env node
// Strata's npm launcher (PLAN 5.3). npm installs only the platform package that matches this
// machine (its `os` and `cpu` fields); this file finds its binary, checks it against the
// SHA-256 written into this file at release time, and runs it with the same arguments.
"use strict";
const { spawnSync } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");

const NAME = "@@NAME@@";
const SHA256 = /* @@SHA256@@ */ {};
const platform = `${process.platform}-${process.arch}`;

let bin;
try {
  bin = require.resolve(`${NAME}-${platform}/bin/strata`);
} catch {
  console.error(
    `strata: no build for ${platform} (builds: ${Object.keys(SHA256).join(", ")}). ` +
      `If yours is listed, reinstall without --no-optional / --omit=optional.`,
  );
  process.exit(1);
}
const sum = crypto.createHash("sha256").update(fs.readFileSync(bin)).digest("hex");
if (sum !== SHA256[platform]) {
  console.error(`strata: ${bin} does not match the checksum this release shipped with; reinstall it.`);
  process.exit(1);
}
const run = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (run.error) {
  console.error(`strata: cannot run ${bin}: ${run.error.message}`);
  process.exit(1);
}
if (run.signal) process.kill(process.pid, run.signal);
process.exit(run.status ?? 1);
