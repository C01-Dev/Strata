// strata.c01.dev on a Cloudflare Worker. The page and its screenshot come from this repo's
// main branch and are cached at the edge, so a push to main updates the site within minutes;
// /install.sh redirects to the latest release's installer. Deploy the Worker itself with
// `npx wrangler deploy` in this folder (only needed when this file changes).
// ponytail: serves from raw.githubusercontent.com behind the edge cache; bundle the page into
// the Worker if GitHub's availability ever matters more than deploy-by-push.
// (site/menubar.png links to docs/menubar.png for local previews.)

const RAW = "https://raw.githubusercontent.com/C01-Dev/Strata/main/";
// Part of every upstream URL, so the edge caches them under it: bump it to drop the cache.
const EPOCH = 4;
const INSTALLER = "https://github.com/C01-Dev/Strata/releases/latest/download/strata-installer.sh";
const FILES = {
  "/": { path: "site/index.html", type: "text/html; charset=utf-8", ttl: 300 },
  "/menubar.png": { path: "docs/menubar.png", type: "image/png", ttl: 300 },
  "/strata-launch.mp4": { path: "docs/strata-launch.mp4", type: "video/mp4", ttl: 300 },
  "/strata-launch.jpg": { path: "docs/strata-launch.jpg", type: "image/jpeg", ttl: 300 },
  "/strata-og.png": { path: "docs/strata-og.png", type: "image/png", ttl: 300 },
};

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/install.sh") return Response.redirect(INSTALLER, 302);
    const file = FILES[pathname];
    if (!file) {
      return new Response("Not found. The page is at https://strata.c01.dev/\n", {
        status: 404,
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }
    // Byte ranges pass through: Safari plays video only through them.
    const range = request.headers.get("range");
    const upstream = await fetch(`${RAW}${file.path}?v=${EPOCH}`, {
      headers: range ? { range } : {},
      cf: { cacheTtl: file.ttl, cacheEverything: true },
    });
    if (!upstream.ok) return new Response("Temporarily unavailable\n", { status: 502 });
    const headers = { "content-type": file.type, "cache-control": `public, max-age=${file.ttl}`, "accept-ranges": "bytes" };
    for (const h of ["content-range", "content-length"]) {
      const v = upstream.headers.get(h);
      if (v) headers[h] = v;
    }
    return new Response(upstream.body, { status: upstream.status, headers });
  },
};
