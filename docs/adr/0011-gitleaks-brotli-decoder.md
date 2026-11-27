# ADR 0011: Gitleaks 8.30.1 with the brotli decoder of CVE-2026-111497

- Status: Accepted. Decided by the lead within delegated authority
  (`DOCTRINE.md` "Exceptions"), 28 November 2026, with approval by an
  independent reviewer (qa-enforcer) in PR #47: the review of head
  `bd5aac5834b8c9070ff5790bd76158d48aad5e29`,
  <https://github.com/jonikanerva/telemetry/pull/47#pullrequestreview-6091806244>.
- Responsible lead: the lead agent of the delivery task.
- Scope: the pinned Gitleaks 8.30.1 (`mise.toml`), which runs as the secret
  scan in `mise run verify` and in the CI job `ci`. Only CVE-2026-111497
  (`github.com/andybalholm/brotli`). The other advisories of the modules
  in the Gitleaks binary are not in this scope.

## Context

- CVE-2026-111497 is a decoder flaw in Google Brotli. CISA KEV lists it
  (added 2026-10-27, catalog version 2026.11.25). `DOCTRINE.md` P7: a
  known exploited vulnerability in a pinned component makes its fix due
  before other work.
- The Gitleaks 8.30.1 release binaries (darwin/arm64 and linux/amd64)
  contain the Go port `github.com/andybalholm/brotli`
  `v1.1.2-0.20250424173009-453214e765f3` (`go version -m`). The Go port
  fixes the flaw in v1.2.7 (GO-2026-6888, GHSA-c9gr-gmg4-2vqv).
- In the Go port, the flaw causes a panic, an infinite loop, or excessive
  memory allocation: a denial of service. The C library has a memory
  corruption. Source: GHSA-c9gr-gmg4-2vqv and GO-2026-6888.
- On 28 November 2026, Gitleaks 8.30.1 is the newest release. The
  Gitleaks default branch uses brotli v1.2.0, which is also affected. No
  upstream issue or PR updates brotli.
- The repository is public. The CI job `ci` runs on `pull_request`, so the
  input of Gitleaks can be commits of a fork. The input is not trusted.
- Gitleaks 8.30.1 reaches the decoder only through archive scanning:
  - `gitleaks git` gives only a binary file with an archive-like name to
    the archive path. A text file goes through the diff text.
  - The archive path calls `archives.Identify` with the file name and no
    content. Name matching does not decode.
  - The archive path skips each archive when the archive depth is 0,
    before it opens a decoder.
  - The command line flag `--max-archive-depth` sets the depth. The
    default is 0. No config key and no environment variable sets it.
  - A test with the pinned binary confirms this: a brotli blob with a
    secret is skipped at depth 0 and decoded at depth 1.
- Upstream PR gitleaks/gitleaks#2317 (open) adds a content check of each
  archive-like file name before the depth check. The content check of a
  brotli name runs the decoder. A Gitleaks release with that change can
  reach the decoder at depth 0.

## Decision

1. Keep Gitleaks 8.30.1 with the secret scan unchanged. The decoder does
   not run in the scans of this repository.
2. The check `scripts/gitleaks-guard.sh` runs before the secret scan in
   the task `checks` (`verify` and `ci`). It fails:
   - when a tracked file other than this ADR holds the literal flag name
     `--max-archive-depth`. The check reads each tracked file as text,
     also a file that Git or `.gitattributes` treats as binary;
   - when `git grep` fails, for example without a repository;
   - when the Gitleaks binary is not version 8.30.1.
3. A change of the Gitleaks version needs a new reading of its archive
   path and a change of this ADR in the same PR.

## Known limits of the guard

- The guard finds the literal flag name only. It does not find a flag
  that a command builds at run time: from a variable, with quotes inside
  the name, or split over lines. It also does not find an escape of the
  file format inside the name, for example a TOML or YAML Unicode escape:
  the tool that reads the file gives Gitleaks the real flag. Such a change
  is deliberate, and the review of the PR is the control.
- The guard reads tracked files only. An untracked file that mise or a
  shell runs is not checked, for example `mise.local.toml`, a global mise
  config, a local task file, or the untracked target of a tracked
  symlink. CI runs only tracked files.
- For a tracked file with the assume-unchanged or skip-worktree bit,
  `git grep` reads the index copy, not the work tree file. The bits are
  local index state, so the effect is local only; CI has a fresh
  checkout.
- The guard searches from the directory where it runs; the task `checks`
  runs it at the repository root.
- The repository has no submodules; the guard does not read submodules.
- At tag v8.30.1, only the command line flag sets the depth: no config
  key and no `GITLEAKS_*` environment variable sets it. The version check
  binds this statement to 8.30.1.

## Key alternatives

- **A newer Gitleaks release.** None exists with brotli v1.2.7 or later.
- **An own build of Gitleaks with brotli v1.2.7.** Rejected for now: the
  build adds a Go toolchain step, an unofficial binary without the
  upstream checksum, and its own provenance and maintenance. The guard
  gives the same protection for the commands of this repository.
- **Another secret scanner.** Rejected for now: a change of the secret
  scan needs new evidence of the same strength, for a decoder that does
  not run.
- **Remove the secret scan until a fix exists.** Rejected: it removes a
  required check (`DOCTRINE.md` P3).

## Consequences

- The secret scan does not read the content of archives. This is the
  present behaviour of the scan and does not change.
- A known exploited vulnerability stays in a pinned tool binary. The
  guard prevents the known path to it in this repository. The guard does
  not cover a Gitleaks command outside the repository, for example a
  command that a person types.
- An update of Gitleaks fails `checks` until the PR changes this ADR and
  the guard.

## Cost of reversal

Low. Remove the guard line in `mise.toml`, the script, and the row of
`STACK.md` §13.

## Reassessment

The first of these conditions:

- A Gitleaks release with `github.com/andybalholm/brotli` v1.2.7 or later.
- Any change of the Gitleaks version or of its install method.
- A need to scan into archives.
- A new advisory for the brotli decoder or for the archive path of
  Gitleaks.
- 2027-01-25.
