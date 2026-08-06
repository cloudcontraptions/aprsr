---
name: add-filter
description: Add an APRS-IS server-side filter type to aprsr end to end — spec check, parse, match, tests, docs. Use when implementing or fixing a filter letter code such as r/, b/, t/, s/, a/, m/, or f/.
---

# Adding an APRS-IS server-side filter

Server-side filters let a client on an `igate` port describe the subset of the feed it
wants. They are space-separated, **additive** (each one adds packets to what the client
receives), and any filter may be negated with a leading `-` to subtract instead.

Work in this order. It keeps the test suite green at every step.

## 1. Read the specification first

Fetch <http://www.aprs-is.net/javAPRSFilter.aspx> and find the exact syntax for the letter
code you are implementing. Do not work from memory — the argument order is the usual
source of bugs. Two that catch people out:

- `a/latN/lonW/latS/lonE` — **north and west come first**, so `latN > latS` and
  `lonW < lonE` for a normal box.
- `t/poimqstunw/call/km` — the optional call and distance apply to the *type* filter, not
  to a separate filter.

Note whether the filter needs a station's last known position (`m/` and `f/` do). Those
resolve through the `PositionSource` trait, never through a direct database call.

## 2. Add the variant

`crates/aprsr-core/src/filter/mod.rs` — add a variant to `enum Filter` holding the parsed
arguments in already-validated form. Parse once at filter-set time; matching runs per
packet per client and must stay cheap. Prefer `Box<[T]>` over `Vec<T>` for the fixed lists
and store pre-lowercased or pre-uppercased forms if matching is case-insensitive.

## 3. Parse it

Extend `Filter::parse`. Reject malformed input with a *specific* `FilterError` variant
that names what was wrong — a sysop reading a log line should be able to fix their filter
string from the message alone. Bound anything unbounded: the spec caps some filters (nine
area filters, for instance), and an unbounded list from a client is a memory-exhaustion
vector.

## 4. Match it

Extend `Filter::matches`. It receives the parsed packet and a `&dyn PositionSource`.
Return early on the cheapest discriminator — checking a packet type flag before decoding a
position saves real work at fan-out scale.

Distance uses the existing great-circle helper in `crates/aprsr-core/src/geo.rs`; do not
write a second one, and keep the comparison inclusive (`<=`) to match the reference
behaviour.

## 5. Test it

In the same file's `#[cfg(test)] mod tests`, add `rstest` cases covering at minimum:

- a packet that matches
- a packet that does not match
- the negated form (`-x/...`) inverting the outcome
- the filter combined with another filter, confirming additive semantics
- at least two parse errors, asserting the specific variant
- a boundary case — exactly at the range limit, empty argument list, maximum count

Use real APRS packet text in fixtures. If the filter needs positions, use the in-memory
`PositionSource` test double already in that module.

## 6. Document it

Add the letter code, its syntax, and its semantics to the filter table in
`docs/protocol.md`, with the spec URL. If the filter was listed as unimplemented in
`docs/roadmap.md`, move it.

## 7. Verify

```bash
cargo test -p aprsr-core filter::
cargo clippy --all-targets -- -D warnings
```

Then confirm end to end against a running server:

```bash
cargo run -p aprsr -- run --config aprsr.example.toml &
printf 'user N0CALL pass -1 vers test 0.1 filter <your filter>\r\n' | nc localhost 14580
```
