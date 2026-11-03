#!/usr/bin/env bash
# Gate 5 (PLAN): on a clean machine that has the README's prerequisites (git, Node 20+), go from
# the README's install command to an agent getting a type error from the shared checker over
# MCP, using only README commands, timed; under 5 minutes passes. .github/workflows/gate5.yml
# runs it on fresh GitHub macOS and Ubuntu VMs; the owner then repeats it by hand.
#
#   bash bench/gate5.sh
set -euo pipefail
start=$(date +%s)
step() { echo "[$(( $(date +%s) - start )) s] $*"; }

step "install (README: shell installer)"
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/C01-Dev/strata/releases/latest/download/strata-installer.sh | sh
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
took=$(( $(date +%s) - start ))
echo "GATE5_SECONDS=$took"
[ "$took" -lt 300 ] || { echo "gate5: over 5 minutes"; exit 1; }
