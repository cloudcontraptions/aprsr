# Attribution

The short version is in [`NOTICE`](../NOTICE); this explains the reasoning.

## aprsc

[aprsc](https://github.com/hessu/aprsc) is the APRS-IS server in C written by **Heikki
Hannikainen (OH7LZB)** and **Matti Aarnio (OH2MQK)**, copyright 2007–2012, licensed
BSD-3-Clause. It has carried the core of the APRS-IS network for well over a decade.

aprsr's architecture follows it. Not by accident and not vaguely: aprsc's decomposition —
separating packet parsing, q construct handling, duplicate detection, server-side
filtering, the client registry, and the incoming and outgoing packet paths into distinct,
individually testable units — is the right shape for this problem, and having it published
openly meant aprsr could start from a proven design instead of rediscovering it.
[`architecture.md`](architecture.md) records the correspondence module by module.

Our thanks to the authors.

## Why aprsr contains no aprsc code

aprsc is BSD-3-Clause. aprsr is MIT OR Apache-2.0. Those are compatible in the direction
that matters for redistribution, but mixing them would mean carrying BSD-3-Clause
obligations — including its no-endorsement clause — through a codebase that does not
otherwise have them, and would make the provenance of any given line a question rather than
a fact.

So the rule is simple and absolute: **no aprsc source code, comments, constant tables,
configuration files, documentation prose, or web assets appear in this repository.** aprsc
is consulted for architecture — how responsibilities divide, what problems need solving —
never for implementation.

Everything aprsr does on the wire is implemented from the public specifications:

- [Connecting to APRS-IS](http://www.aprs-is.net/Connecting.aspx)
- [The q Algorithm](http://www.aprs-is.net/qalgorithm.aspx) and
  [q constructs](http://www.aprs-is.net/q.aspx)
- [javAPRSFilter](http://www.aprs-is.net/javAPRSFilter.aspx)
- [APRS Protocol Reference 1.0.1](http://www.aprs.org/doc/APRS101.PDF)

Each protocol behaviour in the source cites the document it comes from, so a reviewer can
check it against the specification without taking anyone's word for it. Where a behaviour
is genuinely undocumented and had to be inferred, the comment says so rather than
presenting the inference as fact — [`protocol.md`](protocol.md) collects those in one
place.

This rule is stated in [`AGENTS.md`](../AGENTS.md) §1 and is enforced in review. If you are
contributing and have aprsc open in another window, that is fine for understanding the
problem; it is not fine as a source to translate from.

## APRS

APRS® — the Automatic Packet Reporting System — was created by the late **Bob Bruninga,
WB4APR** (1948–2022). APRS is a registered trademark of Bob Bruninga's estate. aprsr is not
affiliated with or endorsed by the trademark holder.

The APRS-IS network, its protocol documentation and the q construct specification are
maintained by **Pete Loveall, AE5PL** and the wider APRS-IS community at
[aprs-is.net](http://www.aprs-is.net/). Public, precise specifications are what make an
independent implementation possible at all — thank you.

## Dependencies

aprsr stands on a lot of other people's work: tokio, Actix Web, SeaORM, Askama, clap,
serde, Tailwind, HTMX, and everything beneath them. `cargo deny check licenses` enforces
that everything in the tree is under a permissive licence compatible with this project's;
run it to see the full list.
