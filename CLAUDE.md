# CLAUDE.md

@AGENTS.md

The file above is the canonical guidance for this repository — read it first. Everything
below is Claude-specific and additive.

## Quick orientation

aprsr is an APRS-IS server in Rust. Six crates, strictly layered
(`aprsr-core` ← `aprsr-config` ← `aprsr-store` ← `aprsr-server` ← `aprsr-web` ← `aprsr`),
plus a `web/` frontend (TypeScript + Tailwind + HTMX) and a `www/` landing page.

The single most important constraint: **aprsr is a clean-room reimplementation of aprsc.
Never copy aprsc code.** Implement from the specs at <http://www.aprs-is.net/> and cite
them. See AGENTS.md §1.

## Subagents

`.claude/agents/` defines two specialists. Use them:

- **`aprs-protocol-reviewer`** — invoke after any change to `aprsr-core` or to the login
  and dispatch paths in `aprsr-server`. It checks the change against the published
  specification and enforces the clean-room rule.
- **`rust-test-author`** — invoke when a module needs its test coverage filled out.
  It writes table-driven `rstest` cases and property tests in this repo's style.

## Skills

`.claude/skills/` captures the two multi-file workflows that recur most:

- **`add-filter`** — adding an APRS-IS server-side filter type end to end.
- **`add-migration`** — adding a SeaORM table, migration, entity, and test.

Prefer these over improvising; they encode the ordering that keeps the test suite green.

## Things that bite

- **Committed assets.** `crates/aprsr-web/static/` is generated but tracked. Edit
  `web/src/`, then `make web`, then commit the output. CI diffs it and fails if stale.
- **`aprsr-core` must stay pure.** No tokio, no I/O, no clock reads. If you need time,
  take it as a parameter. If you need station positions, take `&dyn PositionSource`.
- **Clippy denies panics.** `unwrap`, `expect`, `panic!`, and `foo[i]` indexing are
  `deny` in production code and allowed in tests. Do not add `#[allow]` to get around
  this — restructure instead.
- **Snapshots.** `insta` covers `status.json` and the aprsc.conf converter. After an
  intentional change run `cargo insta review`, and read the diff rather than accepting
  blind.
- **Async tests.** Server tests bind port `0` and read back the assigned port. Never
  hardcode a port and never `sleep` to synchronise — use the channels and readiness
  signals the test helpers already provide.

## Verifying your work

`make ci` is the gate — it runs fmt, clippy, the Rust suite, cargo-deny, the TypeScript
type check and tests, and the stale-asset check. cargo-deny is a separate install
(`cargo install cargo-deny --locked`); without it that one check is skipped behind a loud
banner, and CI will still run it. For a live smoke test:

```bash
cargo run -p aprsr -- run --config aprsr.example.toml &
printf 'user N0CALL pass -1 vers test 0.1 filter r/60/25/100\r\n' | nc localhost 14580
curl -s localhost:14501/status.json
```
