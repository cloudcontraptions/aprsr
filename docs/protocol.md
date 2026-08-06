# Protocol notes

What aprsr implements, and where each behaviour comes from. Every entry here is
implemented from a public specification; aprsr contains no aprsc code (see `AGENTS.md` §1).

## Sources

| Document | Covers |
|---|---|
| [Connecting to APRS-IS](http://www.aprs-is.net/Connecting.aspx) | Login handshake, port types, line format and limits |
| [The q Algorithm](http://www.aprs-is.net/qalgorithm.aspx) | Which q construct applies when, and the reject rules |
| [q constructs](http://www.aprs-is.net/q.aspx) | What each construct means |
| [javAPRSFilter](http://www.aprs-is.net/javAPRSFilter.aspx) | Server-side filter syntax |
| [APRS Protocol Reference 1.0.1](http://www.aprs.org/doc/APRS101.PDF) | Payload formats, positions, symbols |

## Framing

Packets are TNC2: `SOURCE>DESTINATION[,PATH]:information field`, terminated with CR/LF, at
most 512 bytes including the terminator. aprsr enforces the limit in the codec, before the
line is assembled, so an endless line cannot exhaust memory.

Everything after the first `:` is the information field and is relayed untouched — APRS-IS
forwards payload formats it does not understand, so the framing parser deliberately stops
at the colon.

Two deliberate decisions beyond the specification:

- **An empty information field is rejected.** `SRC>DEST:` is syntactically legal but
  carries nothing to relay, and rejecting it keeps the classification and filter code free
  of empty-input cases.
- **Control characters are rejected.** CR and LF would break framing; NUL would truncate
  the packet for any downstream C consumer on the network.

## Callsigns

One to nine characters, uppercase alphanumeric, with an optional `-SSID` of one or two
alphanumeric characters. Login callsigns must be at least three characters
([Connecting](http://www.aprs-is.net/Connecting.aspx)).

SSIDs are **not** restricted to the AX.25 range 0–15: servers use alphanumeric SSIDs such
as `AE5PL-TS`, and that example appears in the specification's own login sample.

## Login

```
user mycall[-ss] pass passcode [vers softwarename softwarevers [UDP udpport] [servercommand]]
```

aprsr sends a comment line identifying itself, reads one login line, and answers with
`# logresp CALL verified|unverified, server SERVERID`. Comment lines from the client before
the login are ignored; keywords are case-insensitive; unknown server commands are skipped
rather than refused, because clients have historically sent commands servers do not
implement and failing the login over one would be worse.

A passcode of `-1` requests a receive-only connection. An incorrect passcode is answered
rather than dropped — the connection stays usable read-only, which is what a misconfigured
client needs in order to diagnose itself. Either way, **only verified clients may inject
packets**; unverified submissions are counted and dropped.

### TLS

Not part of the APRS-IS specification, which describes a plaintext protocol on well-known
ports. aprsr offers it as an additional port kind rather than a change to any documented one:
a `[[listen]]` with a `tls` block speaks exactly the handshake above, wrapped in TLS, and
every other port stays plaintext. Nothing in the protocol changes — the login line, the
comment lines, the framing and the q algorithm are identical, and a client cannot tell which
transport it is on except by having dialled it.

What it protects is the login line, which carries a passcode in the clear, and the client's
ability to know it reached the server it meant to. The packets themselves are public data
broadcast over the air by design, and encrypting them is not the point.

There is no registered TLS port for APRS-IS. `aprsr.example.toml` suggests 24580 and 24152 —
the familiar numbers plus ten thousand — purely as a convention that does not collide with
anything.

An uplink may also dial out over TLS, and always verifies the upstream certificate. That is
not configurable: the identity aprsr takes off an upstream handshake goes into every `qAS`
construct for traffic arriving over that link, so a server it did not authenticate would be
wrong information injected into the whole network.

## q constructs

Two algorithms, not one. Which runs is decided by where the packet came from, and by nothing
else: `IngestSource::Client` takes the client half, `IngestSource::Uplink` the server half.
They are separate functions with separate context types rather than one function with a flag,
because the same fact means different things on the two paths — `via_udp` on a client
connection means "submitted to a `udpsubmit` port" and earns `qAU`, while traffic from a peer
server that happens to travel over UDP means nothing of the sort.

### From a client

Implemented from [the q Algorithm](http://www.aprs-is.net/qalgorithm.aspx), in this order:

1. **Reject** `qAZ`; a `qAC` with no `TCPIP`/`TCPXX` marker; a path in which this server
   already appears after the q construct; a path with a repeated callsign-SSID.
2. **UDP entry** → replace any existing construct with `qAU,SERVERLOGIN`, or append it.
3. **Unverified** → `qAX,SERVERLOGIN`.
4. **Client-only port, FROMCALL ≠ login** → downgrade an existing construct (`qAR`/`qAr` →
   `qAo`; `qAS` → `qAO`; `qAC` → `qAO` *only* when its callsign is neither this server nor
   the login), rewrite `,VIACALL,I` to `,qAo,VIACALL`, or append `,qAO,login`.
   **No port aprsr offers is a client-only port** — see below.
5. **Any existing construct is otherwise left alone.** It records where the packet entered
   APRS-IS, and no later server may replace that.
6. **Trailing `,VIACALL,I`** → rewrite to `,qAR,VIACALL` when VIACALL is the logged-in
   station, otherwise `,qAr,VIACALL`.
7. **FROMCALL = login** → append `qAC,SERVERLOGIN`, or `qAO,SERVERLOGIN` on a send-only port.
8. **Otherwise** → append `,qAS,login`.

**On "client-only port".** The specification gates rule 4 on the packet having "entered the
server from a verified *client-only* connection", and never defines the term. The live
network does: `qAR` constructs whose callsign differs from the packet's source —
`OH2RCH>APRX28,WIDE1-1,qAR,OH2RCH-10:` and the thousands like it — are the most common shape
on APRS-IS, and every one of them would have been downgraded to `qAo` at its first server if
the ordinary filtered port 14580 were client-only. aprsr therefore treats none of its ports
as one. The rules are implemented and tested anyway, because a partial implementation of a
published algorithm is worse than a complete one nothing currently reaches.
*Undocumented; inferred from observed behaviour of the core servers.*

### From an uplink or a peer

1. **Reject** on the same grounds as rule 1 above — those are facts about the packet and the
   path, not about how it arrived.
2. **A `qAI` trace** accumulates the sending server's login and then this server's.
3. **Trailing `,I`** → `,qAr,VIACALL`. Always lowercase: the uppercase `qAR` means the IGate
   was directly connected to *this* server, and a packet that reached us through another
   server by definition was not.
4. **An existing construct is left alone.**
5. **Otherwise** → append `,qAS,<peer login>`, where the login is the identity the upstream
   server gave in its own handshake — never a value from configuration. An operator configures
   a hostname, and `rotate.aprs.net` answers as a different server on every connection.

Loop detection treats `WIDEn-N`, `TRACEn-N`, `RELAY`, `ECHO`, `GATE`, `TCPIP` and `TCPXX`
as routing aliases that may legitimately repeat within one path; any other repeated
callsign is a loop.

## Duplicate detection

Rolling 30-second window, configurable with `limits.dupecheck_window`. The digest covers
**source, destination and the information field** with trailing whitespace trimmed — not
the path. One RF transmission heard by three IGates arrives three times with three
different paths and is one transmission.

Time is passed in rather than read from a clock, and a timestamp that goes backwards is
ignored, so a clock adjustment cannot rewind the window into re-admitting duplicates.

## Filters

Space-separated, additive: a packet is delivered if any filter matches. A leading `-`
negates, and **a negation wins wherever it appears** — if any negated filter matches, the
packet is dropped regardless of what else matched.

| Code | Syntax | Matches |
|---|---|---|
| `r/` | `r/lat/lon/dist` | Positions within `dist` km. Inclusive at the boundary |
| `p/` | `p/aa/bb/…` | Source callsign starting with any prefix |
| `b/` | `b/call/…` | Source callsign exactly; `*` suffix wildcards |
| `o/` | `o/name/…` | Object and item names. Spaces are not allowed, so a name containing one is unreachable |
| `os/` | `os/name/…` | The same, but the argument may contain spaces. Takes the rest of the line, so only one is allowed and it must come last |
| `t/` | `t/poimqstunw` or `t/…/call/km` | Packet categories, optionally within `km` of a station |
| `s/` | `s/pri/alt/over` | Symbol code on the primary table, alternate table, or an overlay |
| `d/` | `d/call/…` | Stations that actually digipeated the packet — the used (`*`) flag |
| `a/` | `a/latN/lonW/latS/lonE` | Positions inside a box. **North and west come first** |
| `e/` | `e/call/…` | The station that injected the packet, i.e. the callsign after the q construct |
| `g/` | `g/call/…` | Message and bulletin addressees |
| `u/` | `u/dest/…` | Destination (unproto) address |
| `q/` | `q/con[/ana]` | q constructs by their third character, case-sensitive; `I` in `ana` also passes IGate positions |
| `m/` | `m/dist` | Within `dist` km of the client's own last known position |
| `f/` | `f/call/dist` | Within `dist` km of another station's last known position |

Type letters: `p` position, `o` object, `i` item, `m` message, `q` query, `s` status,
`t` telemetry, `u` user-defined, `n` NWS, `w` weather, and `c` CWOP.

`c` is **not** in the specification's letter set, which is `poimqstunw`. aprsc accepts it,
so a client filter string that works against the reference server would otherwise fail here
— a compatibility break rather than useful strictness. It is implemented as an extension and
marked as such in the source.

Bounds, because filter strings arrive from unauthenticated clients: at most 64 filters per
expression, 64 entries per list-valued filter, and 9 `a/` filters (the specification's own
limit).

`os/` is the one filter whose argument may contain spaces, which is why the specification
requires it last and allows only one. An object name is a fixed nine-character field and may
well have a space in it — `NET MTG` — and a whitespace-separated expression cannot otherwise
express that. aprsr therefore gives `os/` the whole remainder of the line, and rejects an
expression that puts another filter after it rather than quietly absorbing that filter into
an object name. The specification's note that "objects are always 9 characters and items are
3 to 9" describes the packet format, which the parser already applies when it extracts the
name; it is not a second test applied at match time, since the name is trimmed of its
padding by then.

### Where aprsr is approximate

- **`t/n` (NWS).** APRS-IS does not define NWS traffic by a data type identifier — these
  are ordinary objects and bulletins distinguished by who sends them. aprsr matches source
  callsigns, addressees and object names beginning `NWS`, `SKY` or `CWA`. That is a
  heuristic over the conventions, not a specified format.
- **`q/…/I`.** The analysis field is documented only as "I passes IGATE positions". aprsr
  reads that as: also pass position packets whose construct is `qAR` or `qAr`.
- **`t/c` (CWOP).** Undocumented, and like `t/n` a heuristic over callsigns: the Citizen
  Weather Observer Program issues `CW`, `DW` and `EW` series calls followed by digits. The
  digit test is what keeps the National Weather Service's `CWA` prefix out of it.

## Position encodings

All three from APRS101:

- **Uncompressed** (ch. 6) — `4903.50N/07201.75W-`. Position ambiguity blanks minute digits
  with spaces from the right; aprsr reads blanked digits as zero, placing the station at
  the south-west corner of the ambiguity box, because a range filter still has to compare
  against something.
- **Compressed** (ch. 9) — 13 bytes of base-91.
- **Mic-E** (ch. 10) — latitude in the AX.25 destination, longitude in the first three
  bytes of the information field, symbol at bytes 7 and 8.

A decoder returns nothing rather than a partial result: a filter that cannot establish a
position must not match a range filter by accident.

## Messaging, and why a filter is not the whole story

Per [ServerDesign](http://www.aprs-is.net/ServerDesign.aspx):

> "If filtering of packets to the client is to be done, the server must properly support APRS
> messaging. APRS messaging requires that the client receive any APRS messages destined for
> the client or any station the client has gated to APRS-IS. The client must also receive the
> next available position packet for the sending station of those message packets."

Three obligations, all of which **override the client's filter**:

| | Rule |
|---|---|
| 1 | A message addressed to a client's own callsign reaches that client |
| 2 | A message addressed to a station a client has *gated* reaches that client |
| 3 | Having delivered such a message, the server owes that client the **next** position packet from the message's *sender* |

The reason this is a server concern rather than an IGate one: a filter is written around a
*place* — `r/60/25/100` — and a message from the other side of the world to a station standing
next to the IGate matches none of it. Without these rules the messaging half of APRS would
work only on unfiltered full feeds.

Rule 3 is the one that is easy to leave out and impossible to notice missing from inside a
server: messages get through, replies get through, and the only symptom is that an IGate
cannot tell its operator where the station calling them is.

How aprsr implements it, in `crates/aprsr-server/src/heard.rs`:

- A client that **submits** a packet has gated its *source* station. Uplinks are excluded: a
  packet arriving over an uplink was forwarded from upstream, not gated here, and routing
  replies back up the link would send them to a server rather than to a radio.
- The record lasts `limits.heard_window`, default 30 minutes. **The specification names no
  window**; the default is a judgement about beacon intervals, long enough that a station
  stays reachable between beacons and short enough that a mobile out of range stops having
  its messages sent to a gateway that can no longer hear it.
- The courtesy position is owed for five minutes and settled by one packet. Again undocumented
  — "the next available position packet" has no stated deadline — but a fix delivered twenty
  minutes after the message it explains is noise rather than context.
- Both are per-station, bounded by their window and by a cap of 16 clients per station, and
  pruned by the periodic maintenance task.

`status.json` reports the table size as `stations_gated`, and `/metrics` as
`aprsr_stations_gated`. "Why do messages to my station not arrive" is answered first by
whether the station is in the table at all.

**Not implemented: a `SERVER`-addressed command channel.** aprsr accepts server commands on
the connection itself, as [Connecting](http://www.aprs-is.net/Connecting.aspx) describes
(`filter …`, in the login line or afterwards). A channel where a client sends an APRS
*message* addressed to `SERVER` and receives a reply is not described anywhere at
aprs-is.net, so aprsr does not invent one.

## Packets that are not relayed

Three classes of packet are refused at ingest, before the q algorithm runs — a packet nobody
may relay should not be given a construct recording that it entered APRS-IS here.

| Refused | Why |
|---|---|
| `NOGATE` or `RFONLY` anywhere in the path | The sending station asked for the packet to stay off the internet |
| A third-party packet (`}`) whose **inner header** contains `TCPIP` or `TCPXX` | It has already been on APRS-IS; relaying it loops it back wearing different framing, which duplicate detection cannot see through |
| A general query (`?`) | Per [IGating](http://www.aprs-is.net/IGating.aspx), queries are not gated to or from APRS-IS |

Two details worth stating, because both are easy to get subtly wrong:

- Markers are matched as **whole path elements**. A digipeater called `NOGATEWAY` is an
  ordinary station, and `TCPIPX` in a third-party header is not `TCPIP`.
- Only the third-party **header** is examined, never the comment text. A station whose
  status message mentions TCPIP has made no routing claim, and dropping their traffic for it
  would be a bug almost impossible to diagnose from outside.

These rules are written in the specification for *IGates* rather than for servers
([IGating](http://www.aprs-is.net/IGating.aspx),
[IGate details](http://www.aprs-is.net/IGateDetails.aspx)). aprsr applies them anyway,
because each describes a packet that should never have reached APRS-IS: if one arrives, an
IGate upstream has misbehaved, and passing it on spreads the mistake to every other server.

## Known gaps

Tracked in [`roadmap.md`](roadmap.md). The one worth stating here: **payloads must be valid
UTF-8**. APRS is historically byte-transparent, and a packet with high bytes in its comment
text is currently counted as invalid and dropped rather than relayed.
