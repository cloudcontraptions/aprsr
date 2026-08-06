# aprsr

**An APRS-IS server written in Rust.**

[aprsr.net](https://aprsr.net) · MIT OR Apache-2.0

APRS-IS is the internet backbone of the [Automatic Packet Reporting
System](http://www.aprs.org/): a mesh of servers that relays position reports, weather
data and messages between amateur radio stations worldwide. aprsr is a server for that
network — it accepts client and IGate connections, applies the q construct algorithm,
suppresses duplicate transmissions, and delivers each client exactly the slice of the feed
its filters ask for.

> **Early days.** The protocol core is implemented and covered by tests, and the server
> runs. Uplink and peer connections, TLS and UDP are not in yet — see
> [`docs/roadmap.md`](docs/roadmap.md) for exactly what works today.

## Getting started

You need a Rust toolchain (1.94 or newer). Nothing else — the dashboard assets are
committed, so there is no Node build to run first.

```bash
git clone https://github.com/cloudcontraptions/aprsr
cd aprsr
cp aprsr.example.toml aprsr.toml
```

Edit `aprsr.toml` and set `server.id` to your own callsign. aprsr refuses to start while it
is `NOCALL`, because a duplicated server identity breaks loop detection for the whole
network, not just for you. `aprsr passcode YOURCALL` computes the matching passcode.

```bash
cargo run -p aprsr -- check-config --config aprsr.toml
cargo run -p aprsr -- run --config aprsr.toml
```

Then connect a client:

```bash
printf 'user N0CALL pass -1 vers demo 0.1 filter r/60/25/100\r\n' | nc localhost 14580
```

and open <http://localhost:14501/> for the dashboard.

### Migrating from aprsc

```bash
cargo run -p aprsr -- convert-config /etc/aprsc/aprsc.conf --output aprsr.toml
```

The TOML goes to standard output (or `--output`), and anything that did not convert is
reported on stderr rather than dropped silently.

## What works

| | |
|---|---|
| **Framing** | TNC2 with the 512-byte limit enforced at the codec, before allocation |
| **Login** | The full handshake, passcode verification, receive-only (`pass -1`) connections |
| **q constructs** | `qAC qAX qAU qAo qAO qAS qAr qAR qAZ qAI`, both halves of the algorithm, and the reject rules for loops and internal traffic |
| **Filters** | `r/ p/ b/ o/ os/ t/ s/ d/ a/ e/ g/ u/ q/ m/ f/` — additive, negatable with `-`, bounded against hostile input |
| **Duplicates** | A rolling 30-second window keyed on the transmission, not the path |
| **Ports** | `fullfeed`, `igate` with per-client filters, per-port forced filters and client caps |
| **Uplinks** | Outbound links to other servers, `full` or `readonly`, with DNS rotation, backoff and failover |
| **Persistence** | SQLite via SeaORM: station positions, connection log, sampled counters |
| **Web** | Live dashboard with a station map, history charts and a searchable client table, plus `status.json`, `/metrics` and `/healthz` |

Not yet: peer links, TLS, UDP, SCTP, ACL enforcement, and byte-transparent handling of
non-UTF-8 payloads. All of it is in [`docs/roadmap.md`](docs/roadmap.md).

## Layout

```
crates/aprsr-core     Protocol logic — pure, synchronous, no I/O
crates/aprsr-config   Typed TOML config plus the aprsc.conf importer
crates/aprsr-store    SeaORM entities and migrations (SQLite)
crates/aprsr-server   tokio listeners, client registry, packet dispatch
crates/aprsr-web      Actix Web dashboard and JSON API
crates/aprsr          The binary
web/                  TypeScript, Tailwind and HTMX sources
www/                  The aprsr.net landing page
```

Dependencies flow one way, `aprsr` → … → `aprsr-core`.
[`docs/architecture.md`](docs/architecture.md) maps each module to the aprsc module that
inspired it.

## Development

```bash
make ci      # everything CI runs: fmt, clippy, tests, frontend
make test    # the Rust test suite
make run     # run against aprsr.example.toml
make web     # rebuild the dashboard assets (needs Node)
```

`crates/aprsr-web/static/` is generated but tracked. After editing anything under `web/`,
run `make web` and commit the result — CI fails if it is stale.

Contributions are welcome; please read [`CONTRIBUTING.md`](CONTRIBUTING.md) and
[`AGENTS.md`](AGENTS.md) first. The second one matters even if you are not an AI: it
carries the clean-room rule below.

## Credits

**aprsr would not exist without [aprsc](https://github.com/hessu/aprsc)**, the APRS-IS
server in C by **Heikki Hannikainen (OH7LZB)** and **Matti Aarnio (OH2MQK)**. aprsc has
carried the core of the APRS-IS network for well over a decade, and its module
decomposition — separating packet parsing, q construct handling, duplicate detection,
filtering, the client registry, and the incoming and outgoing packet paths into distinct,
individually testable units — is a genuinely good design that aprsr's crate layout
deliberately follows. Our thanks to the authors for publishing it openly.

**aprsr contains no aprsc source code.** It is an independent implementation written from
the public specifications at [aprs-is.net](http://www.aprs-is.net/), with aprsc consulted
as an architectural reference only. aprsc is BSD-3-Clause and aprsr is MIT OR Apache-2.0,
so that separation is maintained deliberately and is a standing rule for contributions.

APRS® was created by the late **Bob Bruninga, WB4APR** (1948–2022). The APRS-IS network and
its protocol documentation are maintained by **Pete Loveall, AE5PL** and the wider APRS-IS
community — thank you for keeping the specifications public and precise enough to
implement against. aprsr is not affiliated with or endorsed by the trademark holder.

See [`NOTICE`](NOTICE) for the full attribution.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT licence ([`LICENSE-MIT`](LICENSE-MIT))

at your option. Unless you state otherwise, any contribution you intentionally submit for
inclusion shall be dual-licensed as above, without additional terms or conditions.
