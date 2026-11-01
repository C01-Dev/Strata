# Types depend on the generated Prisma client (and its zod/kysely/enum outputs). Skip `prisma format`: it rewrites tracked files.
node "$(sed -n 's/^yarnPath: *//p' .yarnrc.yml)" workspace @calcom/prisma prisma generate
# apps/web imports @calcom/trpc's emitted router declarations (packages/trpc/types/server), built with the repo's own TS 5.9.
node "$(sed -n 's/^yarnPath: *//p' .yarnrc.yml)" workspace @calcom/trpc build:server
