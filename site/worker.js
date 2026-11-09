// strata.c01.dev on a Cloudflare Worker. The page and its screenshot come from this repo's
// main branch and are cached at the edge, so a push to main updates the site within minutes;
// /install.sh redirects to the latest release's installer, and /.well-known/agent-skills/ lets
// `npx skills add https://strata.c01.dev` install skills/strata. Deploy the Worker itself with
// `npx wrangler deploy` in this folder (only needed when this file changes).
// ponytail: serves from raw.githubusercontent.com behind the edge cache; bundle the page into
// the Worker if GitHub's availability ever matters more than deploy-by-push.
// (site/menubar.png links to docs/menubar.png for local previews.)

const RAW = "https://raw.githubusercontent.com/C01-Dev/Strata/main/";
// Part of every upstream URL, so the edge caches them under it: bump it to drop the cache.
const EPOCH = 4;
const INSTALLER = "https://github.com/C01-Dev/Strata/releases/latest/download/strata-installer.sh";
const SKILL = "skills/strata/SKILL.md";
const FILES = {
  "/": { path: "site/index.html", type: "text/html; charset=utf-8", ttl: 300 },
  "/menubar.png": { path: "docs/menubar.png", type: "image/png", ttl: 300 },
  "/strata-launch.mp4": { path: "docs/strata-launch.mp4", type: "video/mp4", ttl: 300 },
  "/strata-launch.jpg": { path: "docs/strata-launch.jpg", type: "image/jpeg", ttl: 300 },
  "/strata-og.png": { path: "docs/strata-og.png", type: "image/png", ttl: 300 },
  "/.well-known/agent-skills/strata/SKILL.md": { path: SKILL, type: "text/markdown; charset=utf-8", ttl: 300 },
};

const raw = (path, ttl, headers = {}) =>
  fetch(`${RAW}${path}?v=${EPOCH}`, { headers, cf: { cacheTtl: ttl, cacheEverything: true } });

/** Agent Skills discovery index (v0.2.0), with the digest of the SKILL.md served next to it. */
async function skillIndex() {
  const upstream = await raw(SKILL, 300);
  if (!upstream.ok) return new Response("Temporarily unavailable\n", { status: 502 });
  const bytes = await upstream.arrayBuffer();
  const sha = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  const description = new TextDecoder().decode(bytes).match(/^description:\s*(.+)$/m)?.[1] ?? "Strata";
  const index = {
    $schema: "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
    skills: [{ name: "strata", type: "skill-md", description, url: "strata/SKILL.md", digest: `sha256:${sha}` }],
  };
  return new Response(JSON.stringify(index, null, 2) + "\n", {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=300" },
  });
}

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/install.sh") return Response.redirect(INSTALLER, 302);
    if (pathname === "/.well-known/agent-skills/index.json") return skillIndex();
    const file = FILES[pathname];
    if (!file) {
      return new Response("Not found. The page is at https://strata.c01.dev/\n", {
        status: 404,
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }
    // Byte ranges pass through: Safari plays video only through them.
    const range = request.headers.get("range");
    const upstream = await raw(file.path, file.ttl, range ? { range } : {});
    if (!upstream.ok) return new Response("Temporarily unavailable\n", { status: 502 });
    const headers = { "content-type": file.type, "cache-control": `public, max-age=${file.ttl}`, "accept-ranges": "bytes" };
    for (const h of ["content-range", "content-length"]) {
      const v = upstream.headers.get(h);
      if (v) headers[h] = v;
    }
    return new Response(upstream.body, { status: upstream.status, headers });
  },
};
