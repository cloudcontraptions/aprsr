# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Windows and macOS are tested, not assumed.** CI builds and runs the full suite on
  `ubuntu-latest`, `macos-latest` and `windows-latest`. Formatting and clippy stay on Linux,
  where they are not platform-dependent.
- **Windows console-control events.** A service host stopping aprsr sends
  `CTRL_CLOSE_EVENT` or `CTRL_SHUTDOWN_EVENT`, not Ctrl-C. aprsr now listens for those and
  Ctrl-Break as well, so it runs its shutdown path and says goodbye to connected clients
  instead of having the socket vanish underneath them.
- **`limits.file_limit` is applied**, having been parsed and ignored since the first
  release. Each client holds a descriptor, so this is effectively the client cap. Where the
  hard limit is lower than the configured value aprsr requests the most it can get and
  warns; Windows has no equivalent per-process limit and says so.
- **`dual_stack` on a listener**, for an IPv6 port that should accept IPv6 only.
- **The `os/` strict object filter**, which was previously rejected at parse time. It is the
  only filter whose argument may contain a space, so it can address an object named
  `NET MTG` — something `o/` cannot express at all, because the expression is split on
  whitespace before the filter sees it. It takes the rest of the line, and an expression that
  puts another filter after it is now an error rather than silently folding that filter into
  an object name.
- **The server-to-server half of the q algorithm** (`qconstruct::apply_server`), which is
  what a packet arriving over an uplink or peer link needs. A packet with no construct gets
  `qAS` and the sending server's login; one that already has a construct keeps it, because
  that construct records where the packet entered APRS-IS; a trailing `,I` becomes the
  lowercase `qAr`; and a `qAI` trace accumulates each server it passes through.
- **Configuration reload without dropping clients**, via `SIGHUP` or
  `POST /admin/reload`. Both go through the same code path, so a Windows operator — where
  there is no `SIGHUP` — gets identical behaviour over HTTP.

  A reload reports what it did. Settings the running server can adopt are applied and
  logged; settings it cannot, such as `server.id` or a listener's bind address, are named
  along with the value still in force, rather than being silently dropped. An invalid file
  changes nothing at all, so a typo cannot leave a server half-configured.

  The administrative endpoints require `http.admin_token` and are disabled entirely when it
  is unset, because the status interface has no other authentication.

### Fixed

- **`bind = "[::]:14580"` now accepts IPv4 clients on every platform.** aprsr took the
  operating system's default for `IPV6_V6ONLY`, and that default is not portable: Linux
  generally accepts IPv4 on an IPv6 socket, Windows and the BSDs generally do not. The same
  configuration file therefore described two different servers. The option is now set
  explicitly, and `dual_stack = false` asks for the other behaviour deliberately.

### Changed

- Integration tests wait for the condition they depend on rather than sleeping a fixed
  50 ms, so they are both faster and reliable on slower and more contended machines.

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
