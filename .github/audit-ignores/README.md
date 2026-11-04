# cargo audit ignores

One file per ignored advisory: `<RUSTSEC-id>.md`. The file name IS the ignore;
the body is why it is safe to ignore and when to stop.

- `ci.yml`'s `security` job builds its `--ignore` flags from this directory
  (`scripts/ci/audit-ignore-flags.sh`). Never add an `--ignore RUSTSEC-…` to a
  workflow by hand; `scripts/ci/audit-ignores-test.sh` refuses one.
- Every file needs a line that starts with `Drop when` — the exit condition.
- A review date is optional and is written exactly `REVIEW BY YYYY-MM-DD`.
  `scripts/ci-audit-ignore-review.mjs` (`audit-ignore-review.yml`, nightly)
  reads it, and also reports an ignore whose advisory no longer fires on
  `Cargo.lock`. Any other spelling of "review by" is refused, so a date cannot
  silently disappear.
- This `README.md` is not an ignore.

Adding an ignore adds one file and edits no shared line, so two PRs adding
different ignores do not conflict (plan
`2026-11-04-runner-generated-and-ledger-files-are-merge-neutral-and-land-behind-a-drain`
Phase 3c). A shared rationale is copied into each advisory's file.

This is deliberately not `.cargo/audit.toml`: that is one shared list (the same
conflict), and cargo-audit auto-loads it, which would silently apply the
ignores to `audit-ignore-review.yml`'s no-ignores audit and blind it.
