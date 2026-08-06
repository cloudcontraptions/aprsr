# Contributing to aprsr

Contributions are welcome — bug reports, protocol corrections, tests, and code.

Please read [`AGENTS.md`](AGENTS.md) before writing any. It is addressed to AI coding
agents, but everything in it applies to people too, and it carries the two rules that
matter most: the clean-room rule and the testing rule.

## The two rules

**1. No aprsc code.** aprsr's architecture follows
[aprsc](https://github.com/hessu/aprsc), but aprsc is BSD-3-Clause and aprsr is
MIT OR Apache-2.0. No aprsc source, comments, constant tables, configuration, prose or
assets may enter this repository. Implement from the public specifications at
[aprs-is.net](http://www.aprs-is.net/) and cite them in a doc comment. Reading aprsc to
understand the problem is fine; translating from it is not.
[`docs/attribution.md`](docs/attribution.md) explains why.

**2. Tests come with the change.** Not afterwards. If you add a filter, a packet format, a
route or a config directive, its tests are part of the same commit.

## Getting set up

```bash
git clone https://github.com/cloudcontraptions/aprsr
cd aprsr
make test          # Rust only — no Node needed
```

`make ci` also runs a licence and advisory check, which needs one extra tool:

```bash
cargo install cargo-deny --locked
```

Without it that check is skipped, and `make ci` says so in a banner you cannot miss —
CI still runs it, so a skip means CI can fail where you just passed.

For the frontend you also need Node 22 or newer:

```bash
cd web && npm ci && cd ..
make ci            # everything CI runs
```

## What good looks like here

**Errors say what was wrong with the input.** `ParseError::PayloadTooLong { len }` beats
`ParseError::Invalid`. Libraries use `thiserror`; only the binary uses `anyhow`.

**Nothing panics in production paths.** `unwrap`, `expect`, `panic!` and slice indexing are
`deny`-level clippy lints outside tests. This is a server that holds thousands of sockets
for months; a panic in a parser reachable from an unauthenticated peer is a denial of
service. Do not add `#[allow]` to get around it — restructure.

**All socket input is hostile.** Bound everything before allocating. Filter expressions,
login lines and packets all arrive from strangers.

**Tests are table-driven and use real data.** `rstest` with one `#[case]` per documented
behaviour, real APRS packet text in fixtures, and the *specific* error asserted for
malformed input — a parser test that only checks the happy path is half a test. Parsers and
the duplicate checker also get `proptest` properties; they have already caught two genuine
bugs in this codebase.

**Tests are deterministic.** No wall-clock reads, no `sleep` to synchronise, no hardcoded
ports. Time is a parameter; server tests bind port 0; database tests use `sqlite::memory:`.

## Frontend changes

Templates are Askama and compile-time checked, so a bad one is a build error. Interactivity
is HTMX first; reach for TypeScript only where HTMX cannot express it. Styling is Tailwind
with shared tokens in `web/src/css/theme.css`.

`crates/aprsr-web/static/` is generated but tracked, so `cargo run` works without Node.
**After touching anything under `web/`, run `make web` and commit the result** — CI fails
if it is stale.

## Before opening a pull request

- [ ] `make ci` passes, with no SKIPPED banner
- [ ] New behaviour has tests; new protocol behaviour cites its spec URL
- [ ] No aprsc code was copied
- [ ] `crates/aprsr-web/static/` regenerated if `web/` changed
- [ ] [`docs/roadmap.md`](docs/roadmap.md) updated if you completed or added an item
- [ ] [`CHANGELOG.md`](CHANGELOG.md) has an entry under *Unreleased*

## Reporting a protocol bug

The most useful reports name the specification. "aprsr appends `qAC` where
[the q Algorithm](http://www.aprs-is.net/qalgorithm.aspx) says `qAO`, because the FROMCALL
differs from the login" is something anyone can act on immediately. A packet that
demonstrates it, in TNC2 form, is even better — it becomes a test case.

For security issues, see [`SECURITY.md`](SECURITY.md).

## Licence

By contributing you agree that your work is dual-licensed under MIT and Apache-2.0, matching
the project, without additional terms.
