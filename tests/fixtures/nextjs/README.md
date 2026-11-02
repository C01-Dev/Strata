Minimal Next.js app for Gate 3 (PLAN 3.6): five agents each run `next dev` behind Strata's proxy,
and hot reload is checked over the proxied WebSocket. Plain `.js` so Next does not try to install
TypeScript. `package-lock.json` is committed; `node_modules` is not.
