// strata.c01.dev on a Cloudflare Worker. The page and its screenshot come from this repo's
// main branch and are cached at the edge, so a push to main updates the site within minutes;
// /install.sh redirects to the latest release's installer. Deploy the Worker itself with
// `npx wrangler deploy` in this folder (only needed when this file changes).
// ponytail: serves from raw.githubusercontent.com behind the edge cache; bundle the page into
// the Worker if GitHub's availability ever matters more than deploy-by-push.
// (site/menubar.png links to docs/menubar.png for local previews.)

const RAW = "https://raw.githubusercontent.com/C01-Dev/Strata/main/";
const INSTALLER = "https://github.com/C01-Dev/Strata/releases/latest/download/strata-installer.sh";
const FILES = {
  "/": { path: "site/index.html", type: "text/html; charset=utf-8", ttl: 300 },
  "/menubar.png": { path: "docs/menubar.png", type: "image/png", ttl: 86400 },
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
    const upstream = await fetch(RAW + file.path, { cf: { cacheTtl: file.ttl, cacheEverything: true } });
    if (!upstream.ok) return new Response("Temporarily unavailable\n", { status: 502 });
    return new Response(upstream.body, {
      headers: { "content-type": file.type, "cache-control": `public, max-age=${file.ttl}` },
    });
  },
};
