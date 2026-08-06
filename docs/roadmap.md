# Roadmap

What works today, and what is missing. Kept honest deliberately: an APRS-IS server that
overstates its coverage wastes the time of the operator who deploys it.

Update this file in the same change that moves an item.

## Working

- **Framing** — TNC2 with the 512-byte limit enforced during decoding; oversized and
  undecodable lines counted and skipped without dropping the connection.
- **Login** — the full handshake, passcode verification, receive-only connections,
  per-connection filters from the login line.
- **q constructs** — all ten codes, the client-to-server algorithm, and the reject rules
  for `qAZ`, `qAC` without a TCPIP marker, self-loops and repeated callsigns.
- **Filters** — `r/ p/ b/ o/ t/ s/ d/ a/ e/ g/ u/ q/ m/ f/`, additive with `-` negation,
  bounded against hostile input.
- **Duplicate detection** — rolling window keyed on the transmission rather than the path.
- **Ports** — `fullfeed` and `igate`, per-port forced filters, per-port client caps,
  `hidden`.
- **Dispatch** — single ingest task, bounded per-client queues, slow clients drop packets
  rather than stalling the server.
- **Persistence** — SQLite via SeaORM: station positions (loaded at start, saved every
  minute and at shutdown), connection log, sampled counters with pruning.
- **Web** — server-rendered dashboard kept live with HTMX, `status.json`, `/healthz`.
- **Operations** — `run`, `check-config`, `convert-config`, `passcode`; SIGINT/SIGTERM
  graceful shutdown; text or JSON logs.

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
- **Counter graphs** — samples are already collected and pruned; the dashboard shows only
  live totals.
- **Server-to-server messaging** — APRS messages addressed to the server itself
  (aprsc's `messaging.c`).
- **`os/` strict object filter** — currently rejected at parse time rather than accepted.
- **`t/n` NWS matching** — a heuristic over callsign prefixes; see `protocol.md`.
- **SCTP** — aprsc supports it; whether it is worth carrying forward is an open question.
- **Historical position tracking** — `station_position` keeps only the latest fix per
  station, which is all `m/` and `f/` need.

## Not planned

- **Rewriting packets beyond the q construct.** APRS-IS relays payloads verbatim, and
  aprsr will not start editing them.
- **A built-in map.** Plenty of good clients already do this; `status.json` is there for
  anyone who wants to build one.
