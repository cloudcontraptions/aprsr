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

## q constructs

Implemented from [the q Algorithm](http://www.aprs-is.net/qalgorithm.aspx), in this order:

1. **Reject** `qAZ`; a `qAC` with no `TCPIP`/`TCPXX` marker; a path in which this server
   already appears after the q construct; a path with a repeated callsign-SSID.
2. **UDP entry** → replace any existing construct with `qAU,SERVERLOGIN`, or append it.
3. **Unverified** → `qAX,SERVERLOGIN`.
4. **Trailing `,VIACALL,I`** → rewrite to `,qAR,VIACALL` when VIACALL is the logged-in
   station, otherwise `,qAr,VIACALL`.
5. **Verified, FROMCALL ≠ login** → downgrade an existing construct (`qAR`/`qAr` → `qAo`;
   `qAS`/`qAC` → `qAO`), or append `qAO,login`.
6. **Verified, FROMCALL = login** → append `qAC,SERVERLOGIN`, or `qAO,SERVERLOGIN` on a
   send-only port.

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
`t` telemetry, `u` user-defined, `n` NWS, `w` weather.

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

## Known gaps

Tracked in [`roadmap.md`](roadmap.md). The one worth stating here: **payloads must be valid
UTF-8**. APRS is historically byte-transparent, and a packet with high bytes in its comment
text is currently counted as invalid and dropped rather than relayed.
