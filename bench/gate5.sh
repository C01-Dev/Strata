#!/usr/bin/env bash
# Gate 5, run inside a clean VM: from install to an agent using the shared checker, using the
# README's commands, timed. The README's prerequisites (git, Node 20+) must already be in place.
#
#   RELEASE=http://<host>:<port> bash gate5.sh
#
# RELEASE serves a release directory (strata-installer.sh plus the tarballs and their
# .sha256). It stands in for GitHub releases until the first one is published; the installer
# command is the README's, with its URL pointed there (STRATA_DOWNLOAD_URL).
set -euo pipefail
: "${RELEASE:?set RELEASE to the release URL}"
start=$(date +%s)
step() { echo "[$(( $(date +%s) - start )) s] $*"; }

step "install (README: shell installer)"
curl --proto '=http,https' -LsSf "$RELEASE/strata-installer.sh" | STRATA_DOWNLOAD_URL="$RELEASE" sh
export PATH="$HOME/.cargo/bin:$PATH"
strata --version

step "a TypeScript repo with one type error"
rm -rf ~/gate5-app && mkdir ~/gate5-app && cd ~/gate5-app
git init -q -b main
printf 'node_modules\n' > .gitignore
printf '{ "name": "gate5-app", "private": true }\n' > package.json
npm install --silent --save-dev typescript@7.1.0-beta
printf '{ "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler" }, "include": ["src"] }\n' > tsconfig.json
mkdir src && printf 'export const answer: number = "forty-two";\n' > src/index.ts
git add -A && git -c user.name=gate5 -c user.email=gate5@localhost commit -q -m init

step "README quickstart: strata init"
# INIT_ARGS=--no-service only for a dry run in a container (no systemd); the gate uses the service.
strata init ${INIT_ARGS:-}
step "strata doctor"
strata doctor || true
step "strata new fix-login"
wt=$(strata new fix-login)
cd "$wt"

step "the agent asks the shared checker over MCP (strata mcp, as in the recipes)"
node - <<'EOF'
const { spawn } = require("node:child_process");
const mcp = spawn("strata", ["mcp"], { stdio: ["pipe", "pipe", "inherit"] });
let buf = "";
const waiting = new Map();
mcp.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i);
    buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    const msg = JSON.parse(line);
    if (msg.id !== undefined && waiting.has(msg.id)) waiting.get(msg.id)(msg);
  }
});
let next = 1;
const send = (method, params) =>
  new Promise((resolve) => {
    const id = next++;
    waiting.set(id, resolve);
    mcp.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
(async () => {
  await send("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "gate5", version: "1" } });
  mcp.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  const res = await send("tools/call", { name: "strata_diagnostics", arguments: { file: "src/index.ts" } });
  const text = (res.result?.content ?? []).map((c) => c.text).join("\n");
  console.log(text);
  mcp.kill();
  if (!/TS2322|not assignable/.test(text)) { console.error("gate5: the checker did not report the error"); process.exit(1); }
})();
EOF
step "done: install to an agent using the shared checker"
echo "GATE5_SECONDS=$(( $(date +%s) - start ))"
