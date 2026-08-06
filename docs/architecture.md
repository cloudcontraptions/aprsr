# Architecture

aprsr is six crates, layered strictly one way:

```
aprsr  →  aprsr-web  →  aprsr-server  →  aprsr-store  →  aprsr-config  →  aprsr-core
```

`aprsr-core` depends on nothing else in the workspace, and on no async runtime, socket, or
database. That is the load-bearing decision in the whole design: the protocol layer is a
set of total functions over borrowed data, which makes it exhaustively testable and keeps
the packet fan-out path allocation-free. Where core logic genuinely needs outside state it
is injected — the duplicate checker takes the current time as a parameter, and
position-dependent filters take a `PositionSource` trait object.

## The life of a packet

```
socket
  │  LineCodec — split on CR/LF, enforce the 512-byte limit
  ▼
client::read_submissions          crates/aprsr-server/src/client.rs
  │  comment lines and `filter` commands handled here; everything else submitted
  ▼
Dispatcher (bounded mpsc, never blocks the reader)
  ▼
dispatch::process                 crates/aprsr-server/src/dispatch.rs
  │  1. unverified sender?          → drop
  │  2. Tnc2Packet::parse           → drop if malformed
  │  3. DupeCheck                   → drop if seen in the last 30 s
  │  4. qconstruct::apply_client    → drop on loop or qAZ; otherwise rewrite the path
  │  5. record any position it carries
  ▼
ClientRegistry::broadcast         crates/aprsr-server/src/registry.rs
  │  one Arc<str> cloned per recipient; filters matched per client
  ▼
per-client bounded queue → writer task → socket
```

A single task owns the duplicate checker and runs the whole of step 3 onward. That keeps
duplicate detection consistent without a lock on the hot path, and it makes fan-out
naturally serialised. Every queue on the path is bounded and every send is non-blocking: a
client that stops draining loses packets and is counted, rather than stalling everyone
else.

## Correspondence with aprsc

aprsr's architecture follows [aprsc](https://github.com/hessu/aprsc). **No aprsc code was
copied** — see `AGENTS.md` §1 and `NOTICE`. This table records which aprsc module inspired
which part of aprsr, so a reader familiar with one can navigate the other.

| aprsc | aprsr | Notes |
|---|---|---|
| `parse_aprs.c` | `aprsr-core/src/aprs/` | Payload classification and position decoding |
| `parse_qc.c` | `aprsr-core/src/qconstruct.rs` | The q construct algorithm |
| `filter.c` | `aprsr-core/src/filter/` | Server-side filters |
| `dupecheck.c` | `aprsr-core/src/dupecheck.rs` | Rolling duplicate window |
| `passcode.c` | `aprsr-core/src/passcode.rs` | Passcode hash |
| `login.c` | `aprsr-core/src/login.rs` + `aprsr-server/src/client.rs` | Split: the line format is pure, the handshake is I/O |
| `cfgfile.c`, `config.c` | `aprsr-config/` | aprsr's native format is TOML; `aprsc.rs` imports the old one |
| `historydb.c` | `aprsr-store/` (`station_position`, `PositionCache`) | Backs `m/` and `f/` |
| `clientlist.c` | `aprsr-server/src/registry.rs` | The client registry |
| `incoming.c`, `outgoing.c` | `aprsr-server/src/dispatch.rs`, `registry.rs` | Merged: one ingest path, one fan-out |
| `accept.c`, `worker.c` | `aprsr-server/src/listener.rs`, `client.rs` | tokio tasks replace the worker-thread pool |
| `http.c`, `status.c` | `aprsr-web/` | Actix Web and Askama replace the embedded HTTP server |
| `counterdata.c` | `aprsr-server/src/metrics.rs`, `aprsr-store` (`counter_sample`) | Live atomics, sampled to the database |
| `acl.c` | `aprsr-store` (`acl_entry`) | Table exists; enforcement is on the roadmap |
| `uplink.c` | — | On the roadmap |
| `tls.c`, `sctp.c` | — | On the roadmap |
| `hmalloc.c`, `cellmalloc.c`, `keyhash.c`, `xpoll.c`, `rwlock.c` | — | No equivalent needed: Rust's allocator, `ahash`, tokio and `std::sync` cover these |

Two places where aprsr deliberately diverges:

- **One ingest path.** aprsc splits incoming and outgoing across worker threads with
  per-worker duplicate state. aprsr uses a single dispatch task, which is simpler to reason
  about and fast enough at APRS-IS packet rates (a full feed is on the order of tens of
  packets per second). If that ever stops being true, the seam is `Dispatcher`.
- **A cache in front of the database.** Position lookups happen for every packet against
  every client, and `aprsr-core` is synchronous. `PositionCache` holds the working set in
  memory and implements `PositionSource`; the database behind it is the durable copy,
  loaded at startup and written back every minute.

## Crate by crate

### `aprsr-core`

Pure protocol. `callsign`, `packet` (TNC2 framing), `path` (digipeater path and the `,I`
construct), `qconstruct`, `aprs/` (payload classification, uncompressed/compressed/Mic-E
positions), `filter/`, `dupecheck`, `passcode`, `login`, `geo`.

Every module carries the specification URL its behaviour comes from. Parsers are total —
`unwrap`, `expect`, `panic!` and slice indexing are `deny`-level lints outside tests —
because a panic in a parser reachable from an unauthenticated socket is a denial of
service.

### `aprsr-config`

Typed TOML with serde and figment, `APRSR_*` environment overrides, and aprsc-style
interval strings (`15s`, `1h30m`, `48h`). `aprsc.rs` is a quote-aware tokenizer and
directive dispatcher that converts an existing `aprsc.conf`, reporting rather than dropping
whatever has no aprsr equivalent.

### `aprsr-store`

SeaORM entities and migrations over SQLite, plus `PositionCache`. WAL and a busy timeout
are set at connect: the dashboard queries while dispatch writes, and the default rollback
journal would serialise them into lock errors. Migrations are append-only.

### `aprsr-server`

`listener` binds sockets up front so configuration errors surface at startup and tests can
use port 0. `codec` frames lines (see its module docs for why it is not
`tokio_util::codec::LinesCodec`). `client` runs the handshake and the two halves of a
connection. `registry` holds clients and fans out. `dispatch` decides each packet's fate.
`metrics` is relaxed atomics shared with the web crate.

### `aprsr-web`

Askama templates compiled into the binary, HTMX polling per panel, Tailwind for styling.
`status.rs` captures one snapshot that both the HTML and `status.json` render from, so they
cannot disagree. `view.rs` formats every number in Rust, keeping the templates to structure
alone.

`static/` is generated by the npm build and **committed**, so `cargo run` produces a styled
page on a machine with no Node toolchain.

### `aprsr`

clap subcommands, tracing setup, signal handling, and the maintenance task that persists
positions and samples counters once a minute.
