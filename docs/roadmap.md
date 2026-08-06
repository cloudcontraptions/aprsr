# Roadmap

What works today, and what is missing. Kept honest deliberately: an APRS-IS server that
overstates its coverage wastes the time of the operator who deploys it.

Update this file in the same change that moves an item.

## Working

- **Framing** — TNC2 with the 512-byte limit enforced during decoding; oversized and
  undecodable lines counted and skipped without dropping the connection.
- **Login** — the full handshake, passcode verification, receive-only connections,
  per-connection filters from the login line.
- **q constructs** — all ten codes, both halves of the algorithm (client-to-server and
  server-to-server, including `qAS` and the `qAI` trace), and the reject rules for `qAZ`,
  `qAC` without a TCPIP marker, self-loops and repeated callsigns.
- **Filters** — `r/ p/ b/ o/ os/ t/ s/ d/ a/ e/ g/ u/ q/ m/ f/`, additive with `-` negation,
  bounded against hostile input.
- **Duplicate detection** — rolling window keyed on the transmission rather than the path.
- **Gating rules** — packets marked `NOGATE`/`RFONLY`, third-party packets that have already
  been on APRS-IS, and general queries are refused at ingest.
- **Ports** — `fullfeed` and `igate`, per-port forced filters, per-port client caps,
  `hidden`.
- **Dispatch** — single ingest task, bounded per-client queues, slow clients drop packets
  rather than stalling the server.
- **Persistence** — SQLite via SeaORM: station positions (loaded at start, saved every
  minute and at shutdown), connection log, sampled counters with pruning.
- **Web** — server-rendered dashboard driven by the status stream rather than timers, with
  history sparklines, a station map, a searchable and sortable client table with per-client
  detail, and light and dark themes; `status.json`, `/healthz`.
- **Observability** — server-sent event streams for status and for the live packet feed,
  `/metrics` in Prometheus text format, and `/api/history` over the sampled counters.
- **Operations** — `run`, `check-config`, `convert-config`, `passcode`; graceful shutdown on
  SIGINT/SIGTERM and on the Windows console-control events a service host actually sends;
  text or JSON logs.
- **Cross-platform** — built and tested on Linux, macOS and Windows in CI. Listener sockets
  set `IPV6_V6ONLY` explicitly so `[::]` means the same thing everywhere, and
  `limits.file_limit` raises the descriptor limit where the platform has one.
- **Configuration reload** — `SIGHUP`, or `POST /admin/reload` behind a token, re-reads the
  file without dropping clients. Settings a running server cannot adopt are named in the
  response rather than silently ignored; an invalid file changes nothing at all.

## Next

**Uplink and peer links** — the largest gap. Without them aprsr is a standalone server
rather than a participant in the APRS-IS mesh. Configuration is already parsed and
validated (`[[uplink]]`), and `check-config` says plainly that it is not connected.
Needs: outbound connection with reconnect and rotation, the server-to-server half of the q
algorithm (`qAS`, `qAr` via an intermediate server), and the `UpstreamTimeout` failover.

**UDP** — `udpsubmit` ports and UDP delivery to clients that asked for it in their login
(`UDP <port>`). The configuration is parsed; `bind_all` logs that UDP listeners are not
served and skips them.

**Byte-transparent payloads** — aprsr decodes lines as UTF-8 and counts anything else as
invalid. APRS is historically byte-transparent, so a comment field with high bytes is
currently dropped rather than relayed. Fixing this means moving the packet types off `&str`
onto `&[u8]`, which touches every parser in `aprsr-core`.

**TLS** — for client connections and uplinks.

**ACL enforcement** — the `acl_entry` table and the `acl` option in `aprsc.conf` are
recognised, but nothing consults them yet.

## Later

- **`dupefeed` ports** — the port kind is configurable and clients on it currently receive
  nothing. Delivering the packets duplicate detection dropped needs a second fan-out path.
- **Server-to-server messaging** — APRS messages addressed to the server itself
  (aprsc's `messaging.c`).
- **`t/n` NWS matching** — a heuristic over callsign prefixes; see `protocol.md`.
- **SCTP** — aprsc supports it; whether it is worth carrying forward is an open question.
- **Historical position tracking** — `station_position` keeps only the latest fix per
  station, which is all `m/` and `f/` need.

## Not planned

- **Rewriting packets beyond the q construct.** APRS-IS relays payloads verbatim, and
  aprsr will not start editing them.

### Reversed

**A built-in map** was listed here. The reasoning was that plenty of good clients already
plot APRS stations, which is true — but they plot *the network*, and none of them answers
the question an operator actually has, which is what *this server* is hearing. That is a
different question, and the dashboard is where it belongs.

Tiles come from a configurable server, and setting `http.map_tile_url = ""` draws stations
on a plain background and contacts nobody, so the feature does not force a closed network
to open.
