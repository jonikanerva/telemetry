# ADR 0007: Codex as a second review

- Status: Accepted. Decided by: owner, 27 November 2026. `VISION.md`
  ("Persistence and Privacy Posture", "Agent review") states the same
  decision.
- Scope: the use of Codex in this repository.

## Context

- Claude Code is the agent of the repository contract (`CLAUDE.md`).
- The repository is public.
- `DOCTRINE.md` P9 requires an independent review of material changes.

## Decision

- The repository has no `AGENTS.md`. Claude Code is the only agent with a
  contract in the repository.
- The lead may ask Codex for a read-only second review of a single
  security-critical PR. The task text of the lead states the rules of that
  review.
- Codex gets no secret and no production data. The review sends the code of
  the PR to OpenAI.
- The Codex review is a second review. It does not replace the independent
  review that `CLAUDE.md` requires.

## Key alternatives

Not recorded with the decision.

## Consequences

- The code of a reviewed PR goes to OpenAI. The code is public anyway.
- A security-critical PR can get a second, independent view.

## Cost of reversal

Low. The decision adds no file and no tool to the repository.

## Reassessment

No condition was recorded with the decision.
