# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

Initial implementation.

**Protocol** (`aprsr-core`)

- TNC2 packet framing with the 512-byte APRS-IS limit, enforced during decoding.
- Callsign validation, including alphanumeric SSIDs such as `AE5PL-TS`.
- The q construct algorithm: all ten codes, the client-to-server rules, and the reject
  rules for `qAZ`, `qAC` without a TCPIP marker, self-loops and repeated callsigns.
- APRS payload classification and position decoding — uncompressed, compressed base-91,
  and Mic-E.
- Server-side filters `r/ p/ b/ o/ t/ s/ d/ a/ e/ g/ u/ q/ m/ f/`, additive with `-`
  negation and bounded against hostile input.
- Rolling duplicate detection keyed on the transmission rather than the digipeater path.
- Passcode generation and verification; login line parsing and handshake rendering.

**Configuration** (`aprsr-config`)

- Typed TOML with `APRSR_*` environment overrides and aprsc-style interval strings.
- An importer for existing `aprsc.conf` files that reports, rather than drops, anything
  without an aprsr equivalent.

**Storage** (`aprsr-store`)

- SeaORM entities and migrations over SQLite: station positions, connection log, sampled
  counters, ACL entries.
- An in-memory position cache backing the `m/` and `f/` filters, persisted across restarts.

**Server** (`aprsr-server`)

- `fullfeed` and `igate` TCP listeners with per-port forced filters and client caps.
- Full login handshake, in-band `filter` commands, keepalive comment lines.
- Single-task packet dispatch with bounded per-client queues; slow clients drop packets
  rather than stalling the server.
- Graceful shutdown.

**Web** (`aprsr-web`)

- Server-rendered dashboard kept live with HTMX, built with Askama and Tailwind.
- `status.json` and `/healthz`.

**Binary** (`aprsr`)

- `run`, `check-config`, `convert-config` and `passcode` subcommands.
- Text or JSON logging; SIGINT and SIGTERM handling; a maintenance task that persists
  positions and samples counters.

**Project**

- `AGENTS.md`, `CLAUDE.md`, `.claude/` agents and skills, and Copilot instructions.
- CI covering formatting, clippy with warnings denied, the Rust and TypeScript test suites,
  licence and advisory checks, and a stale-asset check on the committed dashboard build.
- The aprsr.net landing page, deployed to GitHub Pages by Actions.

### Notes

aprsr is an independent implementation and contains no source code from
[aprsc](https://github.com/hessu/aprsc), whose architecture it follows with thanks. See
[`NOTICE`](NOTICE) and [`docs/attribution.md`](docs/attribution.md).

Uplinks, TLS, UDP and ACL enforcement are not in this release; see
[`docs/roadmap.md`](docs/roadmap.md).

[Unreleased]: https://github.com/cloudcontraptions/aprsr/commits/main
