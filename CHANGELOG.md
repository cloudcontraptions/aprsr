# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Access control and rate limiting**, in an `[access]` section rather than the separate
  `.acl` files aprsc uses — one file that describes the whole server is easier to review and
  to keep in version control than a `.conf` naming four files nobody remembers the contents
  of. `convert-config` says so on an `acl` option rather than dropping it silently.

  Address rules are CIDR blocks, or bare addresses meaning that one host, and **the most
  specific rule wins**. That is how every prefix list a sysop has used behaves, and it means
  the order of lines does not change the meaning of the file — ordering rules are the classic
  way an ACL that reads correctly starts doing the wrong thing after somebody appends a line.
  An exact tie between an allow and a deny resolves to deny. `default = "deny"` turns the
  lists into an allowlist for a closed network.

  The address check is the first thing that happens on an accepted connection: before the
  socket options, before the banner, before anything is allocated. It is also the one place
  in the server where work done before a decision is work an attacker can ask for. An
  IPv4-mapped address is unmapped first, so IPv4 rules keep working when a bind changes from
  `0.0.0.0` to `[::]`.

  A callsign blocklist is checked at the login, and a blocked callsign is *told* — a
  misconfigured station that knows it was refused can be fixed, while one that sees a silent
  disconnect files a bug against its own software.

  The rate limit is a token bucket per client: a sustained rate with a burst allowance,
  because APRS traffic is legitimately bursty and an IGate quiet all night gates several
  packets the moment a net starts. A client over its rate loses packets and keeps its
  connection; disconnecting would turn a beacon interval that is slightly too short into a
  reconnect loop costing more than the packets did. Keepalives and `filter` commands do not
  consume credit. Two new counters, `connections_refused` and `packets_rate_limited`, appear
  in `status.json` and `/metrics`.
- **UDP, both directions.** A `udpsubmit` port accepts datagrams carrying a login line and
  one or more packets, with no connection and no session — which is how a weather station
  that beacons every five minutes avoids holding a socket open for a day. It is also the only
  path that produces a `qAU` construct, so until now aprsr had one it could never emit.

  Every datagram is authenticated on its own: there is no session to authenticate once, and a
  source address is not a credential. Framing is deliberately not the stream rules — a
  datagram has exactly one boundary, so an oversized line fails the *whole* datagram rather
  than being skipped, because a sender whose framing is wrong is not to be trusted about the
  packets either side of it.

  In the other direction, a login carrying `UDP <port>` moves that client's feed to
  datagrams while its TCP connection stays up for submissions, filter commands and
  keepalives. One datagram per packet, never coalesced: a datagram arrives whole or not at
  all, and packing several together makes one loss lose all of them. A server with no UDP
  listener has not consented to sending datagrams, so a client asking there gets the TCP feed
  and a log line saying why.

  Windows reports an ICMP port-unreachable from a *previous* send as `WSAECONNRESET` on the
  *receiving* socket — a connection reset on a protocol with no connections. That would
  otherwise take a listener down the first time a UDP client went away, which is a thing
  clients do constantly, so it and its Linux and BSD equivalents are handled and the loop
  carries on.
- **Uplinks.** aprsr connects out to other APRS-IS servers, so a server with an `[[uplink]]`
  section is a participant in the network rather than a standalone relay between its own
  clients. `readonly` takes the feed and sends nothing; `full` is bidirectional and needs a
  valid `server.passcode`, which `check-config` now checks and warns about — an unverified
  `full` uplink connects and receives, so the failure is otherwise invisible.

  The address is resolved on every attempt and the answers are walked round, because
  `rotate.aprs.net` is a DNS rotation whose whole purpose is to answer differently each time.
  One supervisor runs per uplink with a backoff doubling from 5 s to a minute, reset only by
  a session that lasted long enough to count as working — so a link that connects and
  immediately drops does not reconnect every five seconds forever.
  `limits.upstream_timeout`, parsed and unused since the first release, is the other half:
  an open but silent TCP connection is indistinguishable from a working one at the socket
  level, and an APRS-IS feed is never silent for that long.

  The upstream server's callsign is taken from its own handshake and never from
  configuration. It is what goes into a `qAS` construct for everything arriving over the
  link, an operator configures a hostname, and one hostname answers as a different server
  every time. An upstream that will not identify itself is not usable as an uplink, and the
  session is dropped rather than guessed at.

  Uplinks appear on the dashboard and in `status.json` whether or not they are up — an
  uplink that has never connected is the one an operator needs to see — with the peer's
  identity, the address actually reached, and the reason for the last failure. The
  `no_uplink` alarm now reflects reality and clears itself, and a new `uplink_degraded`
  alarm covers some-up-some-down, which is a different situation from all-down.
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
- **The dashboard updates when something happens, not on a timer.** Each status snapshot
  from the server dispatches an event the panels listen for, so the page reflects the server
  within a second instead of being up to five seconds stale and requesting whether or not
  anything changed. The panels keep a slow timer as a fallback, so a browser without
  `EventSource` — or a proxy that will not hold a streaming response — still updates, and the
  page still works with JavaScript off entirely.
- **A station map** on the dashboard, plotting what this server has actually heard. The tile
  server comes from `/config.json` rather than the bundle, so a closed network can point it
  elsewhere — or set `http.map_tile_url = ""` and have stations drawn on a plain background
  with nothing leaving the browser at all.
- **Light mode**, with a toggle that cycles between following the operating system and
  overriding it in either direction. The choice is applied before the first paint, so a
  reader who chose light never sees a flash of the dark theme, and it is remembered between
  visits. With JavaScript off the stylesheet follows the operating system on its own, and
  the toggle — which could not then do anything — is not shown.
- **Search, sort and a drill-down on the clients table.** The search box matches fields the
  table has no column for, such as the port kind and the exact login time, so a client can
  be found by anything an operator remembers about it; terms are ANDed, so a second word
  narrows rather than widens. Columns sort on the raw numbers the server now emits beside
  each formatted figure — sorting the rendered text would put `1 002` before `999` — and a
  third click on a header returns to the server's order, which is connection time. Clicking
  a row opens a panel with its exact login time in UTC, its port kind, and its byte counts
  in both directions.

  The search term, the sort and any open panel survive the refresh that replaces the whole
  table every few seconds, keyed on the registry id, so the table does not reset itself
  under whoever is reading it.
- **History sparklines** over the last six hours, drawn from `/api/history`, and
  **`GET /api/stations`** for plotting heard stations with a bounding box and a cap.
- **Live streams, metrics and counter history.** `GET /events/status` pushes a snapshot a
  second over server-sent events instead of the dashboard polling three fragments;
  `GET /events/packets` carries the relayed feed itself. `GET /metrics` exposes the counters
  in Prometheus text format, which aprsc has no equivalent for. `GET /api/history` reads the
  `counter_sample` rows — written every minute since the first release and, until now, never
  read by anything. `GET /config.json` carries the settings the browser needs, so the map
  tile server can be changed without rebuilding the committed assets.

  One snapshot is captured per tick however many browsers are watching, and the packet feed
  publishes only when somebody is subscribed — a relaxed atomic load on the dispatch path
  and nothing else when nobody is.

  The packet feed is a full APRS-IS stream over HTTP with no passcode, so it is off unless
  `http.packet_stream` is set *and* the caller presents the administrative token.
- **A message of the day and an alarms panel.** `http.motd_file` names a file whose contents
  appear as a banner, inserted as raw HTML so a notice can be formatted — trusted at the same
  level as the configuration file, and read per request so a notice goes up and comes down by
  creating and deleting the file. `status.json` gains an `alarms` array, always present so a
  consumer can read an empty one as "healthy"; the first alarm fires when uplinks are
  configured but not connected, which is currently always, and says why.
- **`t/c` (CWOP) in the type filter.** Not in the specification's `poimqstunw` letter set,
  but aprsc accepts it, so rejecting it made a filter string that works against the reference
  server an error here. Recognition is a heuristic over the `CW`/`DW`/`EW` callsign series,
  documented as approximate alongside `t/n`.
- **Packets that must not reach APRS-IS are now refused at ingest**: a path carrying
  `NOGATE` or `RFONLY`, a third-party packet whose inner header shows it has already been on
  APRS-IS, and general queries. The check runs before the q algorithm, so a packet nobody may
  relay is not given a construct claiming it entered here. Markers are matched as whole path
  elements, and only the third-party header is examined — the word `TCPIP` in someone's
  comment text is not a routing claim.
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

- **A packet relayed by a station other than its source is no longer downgraded.** aprsr
  applied the q algorithm's `qAR`/`qAr` → `qAo` and `qAS`/`qAC` → `qAO` rules to every
  verified connection. The specification gates them on the packet having "entered the server
  from a verified *client-only* connection" — a term it never defines, and which the live
  network settles: `qAR` constructs whose callsign differs from the packet's source are the
  most common shape on APRS-IS, and every one of them would have been rewritten to `qAo` at
  its first server if the ordinary filtered port were client-only.

  The effect was that aprsr rewrote the record of where a packet entered APRS-IS — including
  every packet from a server uplinking into it — replacing a `qAC` with a claim that the
  packet had been gated from RF. Nothing downstream could have attributed that to aprsr. The
  rules are still implemented, and are still tested against the specification's wording; no
  port aprsr offers now reaches them.

  Two smaller divergences went with it: the `qAC` downgrade lacked its "and callsignssid is
  not equal to the servercall or login" qualifier, and a packet with no construct from a
  station that is not the login now gets `,qAS,login` rather than `,qAO,login`.

  Found by the two-server integration test, not by the unit tests — the algorithm's own
  tests agreed with the algorithm, which is exactly the failure a test at that level cannot
  catch.
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

Peer links and TLS are not in this release; see
[`docs/roadmap.md`](docs/roadmap.md).

[Unreleased]: https://github.com/cloudcontraptions/aprsr/commits/main
