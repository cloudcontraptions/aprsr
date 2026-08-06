# SCTP: why aprsr does not offer it

aprsc can listen on SCTP. aprsr does not, and this is the record of why — written down
rather than left as an absence, because "the other server has it" is a reasonable question
and deserves a real answer.

**Short version:** it is not part of APRS-IS, it cannot be tested in this project's CI, and
it cannot be cross-platform. Any one of those would be survivable. Together they mean
shipping a transport nobody could verify and few could use.

## It is not in the specification

The APRS-IS specification at <http://www.aprs-is.net/> describes two transports.
[Connecting](http://www.aprs-is.net/Connecting.aspx) covers TCP — including the advice to
disable Nagle's algorithm — and UDP submission.
[ServerDesign](http://www.aprs-is.net/ServerDesign.aspx), which is the document listing what
a server must do, says "Send server identification line at connect time on **TCP ports**"
and names no other transport at all.

Neither page mentions SCTP anywhere.

That matters for this project specifically. aprsr is a clean-room implementation *of the
published protocol*, not of aprsc — see `AGENTS.md` §1. A feature that exists only in one
implementation, that the specification does not describe, and that no client is told to
expect is implementation parity rather than feature parity, and implementation parity was
explicitly out of scope from the start.

## It cannot be tested

`AGENTS.md` §5 is not a preference: **no change lands without tests.** For a transport that
means an integration test that binds a socket, connects to it, and pushes a packet through —
the same thing `tests/protocol.rs`, `tests/udp.rs` and `tests/tls.rs` already do for the
transports aprsr does offer.

That test cannot run.

- **Not in development.** The kernel this was developed against has no SCTP support
  compiled or available as a module: `/proc/net/protocols` lists none, there is no
  `kernel/net/sctp` module directory, and `socket(AF_INET, SOCK_STREAM, IPPROTO_SCTP)` fails
  with `EPROTONOSUPPORT`.
- **Not in CI.** SCTP is a loadable module that is blacklisted by default on several
  distributions, and GitHub's hosted runners do not permit loading kernel modules —
  `modprobe` returns `Operation not permitted`. GitHub has stated it does not plan to add
  further modules to the hosted images. A self-hosted runner would work; this project does
  not have one.

So an SCTP listener would be code that has never carried a single packet, in a server that is
meant to stay up for months holding thousands of sockets. Adding a `#[cfg(feature = "sctp")]`
around it does not change that — it just means the untested code is untested *and* off by
default, which is a worse deal than not having it.

## It cannot be cross-platform

Windows has no SCTP stack. macOS ships none. tokio has no SCTP support. It would be a
Linux-only feature in a project whose cross-platform behaviour is one of its stated goals —
the reason listener sockets set `IPV6_V6ONLY` explicitly, the reason signals cover the Windows
console-control events, the reason the whole test suite runs on three operating systems.

## The crates, since somebody will ask

Four crates bind the Linux kernel SCTP stack. All were checked against this workspace's
`deny.toml` in August 2026.

| Crate | Licence | Last release | Notes |
|---|---|---|---|
| `tokio-sctp` 0.2.0 | MIT | Mar 2025 | The most usable. Builds against this tree and passes `cargo deny`. Pulls `socket2 0.4` and `bitflags 1`, both a major version behind what aprsr already has, so the tree would carry two copies of each. |
| `sctp-rs` 0.3.1 | Apache-2.0 OR MIT | Nov 2023 | Nearly three years without a release. |
| `lksctp` 0.1.0 | MIT | Jul 2026 | Minimal dependencies (`libc`, optional `tokio`) and no C library needed, which is attractive. Fewer than a hundred downloads and no track record. |
| `async-sctp` 1.0.0 | MIT | Jul 2026 | Same profile: promising, brand new, unproven. |

**Nothing here is blocked by the `unsafe_code = "forbid"` lint.** That was the risk the plan
flagged, and it turned out not to be the problem: the lint applies to aprsr's own crates, and
every candidate encapsulates the syscalls behind a safe API. `tokio-sctp` compiles against
this workspace today and `cargo deny check` reports `advisories ok, bans ok, licenses ok,
sources ok`.

The blocker is not "can it be written". It is "can it be verified", and the answer is no.

## What would change the answer

Any of these, and this is worth revisiting:

1. **aprs-is.net documents SCTP** as an APRS-IS transport. Then it is protocol conformance
   rather than aprsc compatibility, and the calculus changes completely.
2. **A CI runner with SCTP.** A self-hosted Linux runner, or hosted runners gaining the
   module, makes the integration test possible — and with a real test this becomes an
   ordinary optional feature behind a cargo flag.
3. **A concrete user.** An operator who actually needs an SCTP port, on a host where it can
   be exercised, is a reason to build it and a place to test it.

Until then the honest position is the one in `docs/roadmap.md`: not implemented, and here is
why.
