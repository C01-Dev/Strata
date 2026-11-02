Small TypeScript monorepo for the checker parity suite (PLAN task 2.7): three tsconfigs that share
a base config and import each other through `paths`. Tests copy it, link TypeScript 7.1 into
`node_modules/typescript`, and apply scripted diffs per worktree.
