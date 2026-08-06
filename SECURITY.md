# Security

## Reporting a vulnerability

Please report security issues privately through GitHub's
[private vulnerability reporting](https://github.com/cloudcontraptions/aprsr/security/advisories/new)
rather than in a public issue.

Include what you can: the version or commit, a packet or input that triggers it, and what
you observed. A reproduction is worth more than a description.

## What aprsr is exposed to

An APRS-IS server accepts connections from anyone. Every byte on the socket is untrusted
before it is parsed, and the parsers are reachable before authentication — the login line
itself is attacker-controlled. The measures that follow from that:

- **No panics in production paths.** `unwrap`, `expect`, `panic!` and slice indexing are
  `deny`-level clippy lints outside tests. A panic in a parser reachable from an
  unauthenticated peer is a denial of service.
- **Everything is bounded before it is allocated.** The 512-byte line limit is enforced
  while decoding, not after assembling a line. Filter expressions cap the number of filters
  and the length of each list.
- **Parsers are property-tested** against arbitrary bytes for exactly this reason.
- **Queues are bounded and sends never block.** A client that stops draining loses packets
  and is counted; it cannot stall the server for everyone else.
- **Passcodes are never logged.**

## What the passcode is and is not

The APRS-IS passcode is a 15-bit hash of a callsign. It stops casual impersonation and
accidental misconfiguration. It is **not** authentication in any meaningful sense — it is
trivially computable by anyone, and it is documented publicly. Do not build anything on the
assumption that a verified connection proves identity.

## Deployment notes

- Run aprsr as an unprivileged user. It needs no elevated privileges; if you want it on a
  port below 1024, use capabilities or a redirect rather than root.
- The status dashboard exposes connected callsigns, their addresses and their filters.
  Bind `http.status_bind` to a private interface, or put it behind a reverse proxy with
  access control, if that is not information you want public. `hidden = true` on a listener
  keeps that port off the page.
- The SQLite database holds a connection log with client IP addresses. Treat it as personal
  data.

## Supported versions

aprsr is pre-1.0. Only the latest commit on the default branch receives fixes.
