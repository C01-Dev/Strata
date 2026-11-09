# Do real agents clash, and do they say so? (2026-11-09)

Four Claude Code sessions (Opus) ran at the same time on the excalidraw fixture, each in its own Strata worktree. They worked on related tasks around text inside shapes: `SET=related bench/agent-usage.sh note`. Nothing was planted: whether their changes clash was up to them.

| Agent | Task |
| --- | --- |
| rename | Rename `getBoundTextElement` to `getBoundText` everywhere |
| opts | Make `newElementWith`'s bare `force` boolean an options object |
| clear | Add a "Clear text" action for shapes with text |
| upper | Add an "Uppercase text" action |

## Run 1: the note did not mention `strata_conflicts` yet

The release binary predated the note line. Agents could only learn of clashes from the line on `strata_diagnostics` answers.

| Pair | What happened | Found by Strata |
| --- | --- | --- |
| clear + rename | `actionBoundText.tsx:167` calls `getBoundTextElement`, which rename removed (TS2552) | yes |
| upper + rename | the new `actionUppercaseText.test.tsx:3` imports `getBoundTextElement` (TS2724) | yes |
| clear + upper | both added their action on the same lines of `actions/index.ts` | yes, as a file git cannot merge |
| the other 3 pairs | nothing | nothing reported |

- No agent called `strata_conflicts`, since nothing told it to.
- rename and clear got the clash on a later `strata_diagnostics` answer, and both reported it in their summaries. clear also reported the `index.ts` conflict, and said it had not touched the other agents' code.
- upper checked types once, at 04:38:00. Its clash with rename appeared at 04:39:06, so upper never saw it and did not report it.

## Run 2: the shipped note (`TAG=2`, fresh daemon)

| Pair | What happened | Found by Strata |
| --- | --- | --- |
| clear + rename | `actionBoundText.tsx:160` and `:168` call `getBoundTextElement` (TS2552, twice) | yes |
| the other 5 pairs | nothing | nothing reported |

- **All 4 agents called `strata_conflicts` before reporting back.**
- rename and clear each named the other agent, the file, the lines and the error. clear said "I left my code as it is rather than changing it to match theirs."
- opts and upper reported no clashes, which was correct.
- No agent ran its own type check. upper ran its tests through `strata_run`.

## Totals

- **Pairs:** 12 across the two runs.
- **Real problems that happened on their own:** 4.
  - 3 pairs with type clashes. All of them would have failed to compile after both branches merged.
  - 1 pair with a file git cannot merge.
- **Caught by Strata:** all 4. No false alarms.
- **Reported by the agents involved:** 4 of 6 agent reports.
  - Run 1, without the note line: 2 of 3.
  - Run 2, with it: 2 of 2.

Raw logs: `bench/results/agent-usage/note-related/` and `note-related-2/` (not committed).
