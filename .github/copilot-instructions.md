# Copilot instructions for aprsr

aprsr is an APRS-IS server written in Rust. Full guidance lives in
[`AGENTS.md`](../AGENTS.md) at the repository root; this file is a condensed mirror.

## Clean-room rule

aprsr's architecture follows [aprsc](https://github.com/hessu/aprsc) (C, BSD-3-Clause),
but aprsr is MIT OR Apache-2.0. **Never copy aprsc source, config, docs, or assets.**
Implement protocol behaviour from the public specs at <http://www.aprs-is.net/> and cite
the relevant URL in a doc comment on anything spec-driven.

## Layout and layering

| Crate | Role |
|---|---|
| `aprsr-core` | Protocol logic — pure, synchronous, no I/O, no async |
| `aprsr-config` | Typed TOML config + aprsc.conf importer |
| `aprsr-store` | SeaORM entities and migrations (SQLite) |
| `aprsr-server` | tokio listeners, client registry, packet dispatch |
| `aprsr-web` | actix-web dashboard and status API, Askama templates |
| `aprsr` | Binary: CLI, wiring, tracing, shutdown |

Dependencies flow one way only, `aprsr` → … → `aprsr-core`. Never add tokio, actix, or
sea-orm to `aprsr-core`; inject the clock and the database through parameters and traits.

## Conventions

- `thiserror` in libraries, `anyhow` only in the binary.
- `unwrap`, `expect`, `panic!`, and slice indexing are clippy-`deny` outside tests. This
  is a long-running network server; a parser panic is a denial of service.
- All socket input is untrusted: enforce the 512-byte APRS-IS line limit, validate before
  allocating, and never let one malformed packet disturb other clients.
- `tracing` with structured fields, never `println!`. Never log a passcode.
- Use APRS-IS vocabulary verbatim: `callsign`, `passcode`, `igate`, `q_construct`,
  `unproto`, `fullfeed`, `dupecheck`.

## Testing is mandatory

No change lands without tests. Unit tests go in `#[cfg(test)] mod tests` in the same
file; table-driven cases use `rstest`; parsers and the dupe checker also get `proptest`
property tests; snapshots use `insta`. Use real APRS packet text in fixtures, assert the
specific error for malformed input, and keep tests deterministic — the dupe checker takes
time as a parameter and server tests bind port 0.

## Frontend

Askama templates (compile-time checked) + HTMX first, TypeScript only where HTMX cannot
express it, Tailwind for styling with shared tokens in `web/src/css/theme.css`.
`crates/aprsr-web/static/` is generated but committed — after editing `web/`, run
`make web` and commit the output or CI will fail the stale-asset check.

## Before finishing

Run `make ci`. It runs fmt, clippy with warnings denied, the Rust test suite, the
TypeScript type check and tests, and the committed-asset freshness check.
