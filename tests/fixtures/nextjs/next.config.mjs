/** @type {import('next').NextConfig} */
export default {
  // Next 16 writes AGENTS.md into the project on `next dev`; in an agent's worktree that is an
  // untracked file (so `strata rm` would refuse). Off for this fixture.
  agentRules: false,
};
