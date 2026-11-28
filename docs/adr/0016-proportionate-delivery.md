# ADR 0016: Proportionate delivery and one full verify per merge

- Status: Accepted, 28 November 2026. Decided by the owner.
- Supersedes in part: ADR 0008, decision 1, where it requires the local
  `verify` from the implementer and from the reviewer.
- Changes: ADR 0005, decision 8 (decision 7 below).

## Context

- Measurement of PRs #45 to #51 (7 PRs): slices took up to five review
  rounds. The full `mise run verify` ran about 45 times: 15 reviewer runs,
  read from the review texts, and about 30 implementer runs, estimated
  from the pushed heads. One run for each merged version, plus one for
  each base change after a PASS, is about 9 runs.
- One full `verify` took 14.2 minutes (macOS arm64, cold worktree, under
  load). Estimate for a PR with three review rounds: about 86 minutes of
  local runs before this decision, about 24 minutes after it.
- The reviewer's full `verify` found one blocking fault in these PRs: a
  type check that depended on the checkout path. A clean checkout finds it.
- Each run wakes the Neon branch `test`; its use was 5.39 CU-hours, above
  the 5 CU-hours of ADR 0002.

## Decision

1. **Two risk classes.** The high class covers harm that is hard to
   reverse; every other surface has the light class (`DOCTRINE.md`
   introduction, `STACK.md` §0). Light-class work gets one review round;
   a second round only for a defect in behaviour, privacy, or data.
2. **Review bar.** Only a defect in behaviour, privacy, or data, a broken
   owner rule, or a failed or missing required check blocks. The lead has
   other findings fixed and checks the delta itself (`DOCTRINE.md` P9).
3. **One full verify per merge.** The implementer runs the fast checks and
   the tests of the changed parts, the database tests for a database
   change, and waits for a green CI result. The reviewer runs the full
   `verify` once for a change with a high-class part; the lead runs it
   once for any other change. After a new head or base, the party that ran
   it runs the affected checks again (`DOCTRINE.md` P6, P9; `STACK.md` §3).
4. **Size limit.** At a third review round, or well past the estimate, the
   lead stops and asks the owner (`DOCTRINE.md` P1).
5. **Cost and fewer documents.** Time, machine resources, and review
   rounds are costs (`DOCTRINE.md` P4). An ADR records only the decisions
   that `DOCTRINE.md` P8 names; other reasons go in the commit message.
6. **Web profile cuts.** Removed: the old-client compatibility window, the
   browser diagnostic mechanism, the readable development formatter (so
   the §13 exception of ADR 0009 and ADR 0010 ends), the adoption trial,
   tenant cache scoping, and the budgets of the warm battery API, the login
   time, and the database requests (`STACK.md` §4). Optional: executed response validation and
   real-response tests outside the high class, and axe checks beyond one
   for each page. The browser and viewport rules do not change.
7. **CI by changed inputs.** A draft PR runs no job. On a PR, `image-check`
   and `vuln` run only when an input of the job changes (`STACK.md` §3).
   This changes ADR 0005, decision 8: a red `vuln` job of a push to `main`
   or of the schedule starts the triage.

## Key alternatives

- **Keep the rules.** Rejected: the cost above, with almost no findings.
- **The reviewer runs `verify` for every reviewed change.** Rejected: a
  light-class change does not need an independent run.
- **CI only on a merge to `main`.** Rejected: a merge is a release.

## Consequences

- The database tests no longer run on each implementer head. The tests
  that need Neon run once for each merged version.
- A light-class change has no independent `verify` run; the lead's run
  and CI are the evidence.
- Stale document text can reach `main` and get its fix after the merge.
- Read-only API responses need no executed validation tests. The current
  client still validates every response (`STACK.md` §1).

## Cost of reversal

Low. The decisions are text rules, not code.

## Reassessment

A defect reaches production that a removed run or review round would have
found; or by 2027-02-28.
