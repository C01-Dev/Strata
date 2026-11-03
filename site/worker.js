// strata.c01.dev on a Cloudflare Worker: the page, its screenshot, and /install.sh, a short
// link to the latest release's installer. Deploy with `npx wrangler deploy` in this folder.
// (site/menubar.png is a link to docs/menubar.png for local previews; the Worker serves the
// repo's copy, cached at the edge.)
import html from "./index.html";

const INSTALLER = "https://github.com/C01-Dev/strata/releases/latest/download/strata-installer.sh";
const SCREENSHOT = "https://raw.githubusercontent.com/C01-Dev/strata/main/docs/menubar.png";

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/install.sh") return Response.redirect(INSTALLER, 302);
    if (pathname === "/menubar.png") {
      const img = await fetch(SCREENSHOT, { cf: { cacheTtl: 86400, cacheEverything: true } });
      return new Response(img.body, {
        status: img.status,
        headers: { "content-type": "image/png", "cache-control": "public, max-age=86400" },
      });
    }
    if (pathname === "/") {
      return new Response(html, {
        headers: { "content-type": "text/html; charset=utf-8", "cache-control": "public, max-age=300" },
      });
    }
    return new Response("Not found. The page is at https://strata.c01.dev/\n", {
      status: 404,
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  },
};
