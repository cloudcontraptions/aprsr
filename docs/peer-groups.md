# Peer groups: why aprsr does not have them

aprsc can join a peer group — a mesh of servers at the same tier exchanging traffic
sideways rather than up a tree. aprsr does not, and this records why, alongside what aprsr
does instead.

**Short version:** APRS-IS does not describe peer groups, the specification tells servers to
do the opposite, and the only route to a wire-compatible implementation runs straight through
this project's clean-room rule.

## The specification says the opposite

[ServerDesign](http://www.aprs-is.net/ServerDesign.aspx) is the document listing what an
APRS-IS server must do. On inter-server links it says exactly one thing:

> "Servers should only connect to a single upstream server and should never be connected to
> more than one server at a time. This is critical to preventing loops."

[Specification.aspx](https://www.aprs-is.net/Specification.aspx) indexes nine documents —
Connecting, Client UDP, Send-only Ports, Server Filter Commands, IGate Design, IGate Details,
Server Design, q Construct, q Algorithm. **None of them describes a peer group, a peer
protocol, or any server-to-server transport beyond that single upstream connection.**

The q algorithm does have a `qAS` construct for "a packet from another server", which is what
an uplink produces and what aprsr already implements. That is the whole of the published
server-to-server story.

## The framing is not published

The closest thing to a description in public is a wiki page of "collected server
requirements", which sketches a design rather than specifying one: a `@xy@` prefix tag where
"the 'x' is flag encoder, and 'y' is reserved for extension in case 6 peers is not enough".
It reads as a proposal. It also says, of the transport the plan for this work assumed:

> "Usage of UDP packets carrying each peer packet is considerably less efficient in network
> terms than TCP or SCTP, and thus not recommended"

So the one public description of peer framing recommends against the design it would have
been built to.

Being *compatible* with the servers that actually run peer groups needs their real wire
format. There are three ways to get it, and aprsr can use none of them:

1. **A published specification.** There isn't one.
2. **Reading aprsc's source.** Forbidden — `AGENTS.md` §1, and the reason this project can be
   MIT OR Apache-2.0 at all.
3. **Observing a live peer mesh.** Peer groups interconnect the core servers. There is no
   way to join one and watch, which is what "inferred from observed behaviour" requires
   everywhere else aprsr uses that phrase.

Inventing a format instead would produce a peer group of exactly one implementation, which
is not a peer group.

## Who it is for, anyway

Per [the APRS-IS overview](https://www.aprs-is.net/), "There are now 3 to 4 core servers
splitting the connection load across the USA." Peer groups are how that handful of core
servers interconnect. A server run by a sysop joins the network the documented way — one
uplink, up the tree — and that is what aprsr does.

## What aprsr does instead

Several `[[uplink]]` entries are a **failover list, in the order you wrote them** — not a
mesh, and not several simultaneous connections. One is connected at a time, which is what the
rule above requires.

```toml
[[uplink]]
name = "Core rotate"
kind = "readonly"
address = "rotate.aprs.net:10152"

[[uplink]]
name = "Fallback"
kind = "readonly"
address = "t2finland.aprs2.net:14580"
```

The first entry is tried first. If it cannot be reached, the next is tried **immediately** —
no backoff inside a pass, because the whole point of configuring an alternative is that it is
probably up. Only when a complete pass has failed does the backoff apply, doubling from five
seconds to a minute, so a total outage is patient while a single dead server costs nothing.

A session that lasts long enough to count as healthy resets the list, so your first choice is
tried first again once it comes back.

Two layers of redundancy stack here, and they are different things:

- **Within one uplink**, the address is re-resolved on every attempt and the answers are
  walked round. `rotate.aprs.net` is a DNS rotation; this is what makes it work as one.
- **Across uplinks**, the list above. Use it for genuinely different servers.

`aprsr check-config` lists the uplinks in the order they will be tried.

## What would change the answer

- **aprs-is.net publishes a peer protocol.** Then it is protocol conformance, implementable
  from the specification like everything else here, and worth doing.
- **aprsr is asked to be a core server.** That is not a code decision.
