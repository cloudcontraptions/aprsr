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
- **Uplinks** — outbound links to other servers, `full` or `readonly`, with DNS rotation,
  reconnection with backoff, and `upstream_timeout` failover. The peer's identity comes from
  its own handshake, never from configuration. Several configured uplinks are a *failover
  list* tried in order — exactly one is connected at a time, as
  <http://www.aprs-is.net/ServerDesign.aspx> requires.
- **UDP** — `udpsubmit` ports, which are the only way a `qAU` construct is ever produced, and
  feed delivery to a client whose login carried `UDP <port>`.
- **Access control** — CIDR allow and deny lists with longest-prefix matching, a callsign
  blocklist, and a per-client token-bucket rate limit on submissions.
- **TLS** — listening ports and outbound uplinks, over rustls. Certificates load when the
  server binds, so a bad path or a mismatched pair is a startup failure naming the file.
  An uplink verifies the upstream certificate against the built-in Mozilla roots or a
  private authority, and there is no option to skip that.
- **`dupefeed` ports** — the packets duplicate detection suppressed, delivered verbatim to
  the clients that asked for them.
- **Messaging** — a message reaches the client it is addressed to and every client that gated
  its addressee, whatever their filters say, and the sender's next position follows it. The
  three obligations at <http://www.aprs-is.net/ServerDesign.aspx>, without which messaging
  works only on unfiltered full feeds.
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

**Byte-transparent payloads** — aprsr decodes lines as UTF-8 and counts anything else as
invalid. APRS is historically byte-transparent, so a comment field with high bytes is
currently dropped rather than relayed. Fixing this means moving the packet types off `&str`
onto `&[u8]`, which touches every parser in `aprsr-core`.

**Duplicate-normalisation variants** — aprsr detects an exact repeat and one differing only
by trailing whitespace. aprsc detects several more, including payloads that differ only in
whether the high bit of each byte survived a gateway. Those are genuinely different packets
to aprsr today, and both get relayed.

## Later

- **A `SERVER`-addressed command channel** — a client sending an APRS *message* addressed to
  `SERVER` and getting a reply. Not described anywhere at aprs-is.net, so aprsr does not
  invent one; server commands travel on the connection itself, as
  <http://www.aprs-is.net/Connecting.aspx> describes.
- **`t/n` NWS matching** — a heuristic over callsign prefixes; see `protocol.md`.
- **Historical position tracking** — `station_position` keeps only the latest fix per
  station, which is all `m/` and `f/` need.

## Not planned

- **Rewriting packets beyond the q construct.** APRS-IS relays payloads verbatim, and
  aprsr will not start editing them.
- **Peer groups.** aprsc can join a mesh of same-tier servers; APRS-IS does not describe one,
  and ServerDesign says the opposite — a server "should only connect to a single upstream
  server and should never be connected to more than one server at a time". The framing is not
  published either, and the only other route to it is reading aprsc's source, which this
  project does not do. See [`peer-groups.md`](peer-groups.md).
- **SCTP.** aprsc offers it; APRS-IS does not describe it. It is also untestable in this
  project's CI — GitHub's hosted runners will not load the kernel module — and Linux-only.
  The full reasoning, the crates that were evaluated, and the three things that would change
  the answer are in [`sctp.md`](sctp.md). **This resolves what the roadmap previously listed
  as an open question.**

### Reversed

**A built-in map** was listed here. The reasoning was that plenty of good clients already
plot APRS stations, which is true — but they plot *the network*, and none of them answers
the question an operator actually has, which is what *this server* is hearing. That is a
different question, and the dashboard is where it belongs.

Tiles come from a configurable server, and setting `http.map_tile_url = ""` draws stations
on a plain background and contacts nobody, so the feature does not force a closed network
to open.
