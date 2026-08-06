# AGENTS.md

Instructions for AI coding agents working on **aprsr**, an APRS-IS server written in Rust.

This is the canonical agent instruction file. `CLAUDE.md` and
`.github/copilot-instructions.md` defer to it — change this file first, then mirror
anything tool-specific.

---

## 1. The rule that matters most: aprsr is clean-room

aprsr's architecture is modelled on [aprsc](https://github.com/hessu/aprsc), an
excellent APRS-IS server in C. **aprsc is BSD-3-Clause. aprsr is MIT OR Apache-2.0.**

Therefore:

- **Never copy aprsc source code, comments, configuration files, documentation prose,
  or web assets into this repository.** Not a function, not a lookup table, not a
  constant list, not a config file, not an HTML template.
- aprsc may be consulted for *architecture* — how responsibilities are split across
  modules, what the data flow looks like, which problems need solving. It must not be
  consulted as a source of implementation.
- Implement protocol behaviour from the **public specifications** at
  <http://www.aprs-is.net/>:
  - [Connecting to APRS-IS](http://www.aprs-is.net/Connecting.aspx) — login handshake,
    port types, line format
  - [The q Algorithm](http://www.aprs-is.net/q.aspx) — q constructs and when each applies
  - [javAPRSFilter](http://www.aprs-is.net/javAPRSFilter.aspx) — server-side filter syntax
  - [APRS Protocol Reference 1.0.1](http://www.aprs.org/doc/APRS101.PDF) — packet payloads
- **Every protocol behaviour needs a citation.** When you implement or change something
  that is dictated by the protocol, cite the source in a doc comment, e.g.
  `/// Per <http://www.aprs-is.net/q.aspx>: a packet from a verified client whose ...`.
  A reviewer must be able to check the behaviour against the spec without guessing.
- If a behaviour is genuinely undocumented and you can only infer it from observing a
  live server, say so explicitly in the comment: `// Undocumented; inferred from
  observed behaviour of the core servers.` Do not silently invent semantics.

`NOTICE` records the acknowledgement to aprsc's authors. Keep it accurate.

---

## 2. Layout

```
crates/aprsr-core     Protocol logic. Pure, synchronous, no I/O, no async, no tokio.
crates/aprsr-config   Typed TOML config + the aprsc.conf importer.
crates/aprsr-store    SeaORM entities and migrations (SQLite).
crates/aprsr-server   tokio listeners, client registry, packet dispatch.
crates/aprsr-web      actix-web dashboard and status API (Askama templates).
crates/aprsr          The binary: CLI, wiring, tracing, shutdown.
web/                  TypeScript + Tailwind sources for the dashboard and landing page.
www/                  The aprsr.net landing page (GitHub Pages).
docs/                 Architecture, protocol notes, roadmap, attribution.
```

The dependency direction is strictly one-way:

```
aprsr  →  aprsr-web  →  aprsr-server  →  aprsr-store  →  aprsr-config  →  aprsr-core
```

`aprsr-core` depends on nothing in the workspace. **Do not add tokio, actix, sea-orm, or
any I/O to `aprsr-core`.** That constraint is what makes the protocol layer fast to test
and impossible to get subtly wrong through async ordering. If core logic seems to need
the clock or the database, inject it: take `now: u64` as a parameter, or take a
`&dyn PositionSource`. There are existing examples of both.

`docs/architecture.md` maps each aprsr module to the aprsc module that inspired it.

---

## 3. Commands

```bash
make ci            # everything CI runs — run this before you say you are done
make test          # cargo test --workspace --all-features
make lint          # clippy, warnings denied
make deny          # licences and advisories (needs `cargo install cargo-deny --locked`)
make fmt           # rustfmt
make web           # rebuild the dashboard assets into crates/aprsr-web/static/
make run           # run the server against aprsr.example.toml

cargo test -p aprsr-core                       # one crate
cargo test -p aprsr-core filter::              # one module
cargo insta review                             # review changed snapshots
```

---

## 4. Conventions

**Errors.** Libraries (`aprsr-core`, `aprsr-config`, `aprsr-store`, `aprsr-server`,
`aprsr-web`) return typed errors built with `thiserror`. Only the `aprsr` binary uses
`anyhow`. An error type should say what was wrong with the *input*, not just that
something failed — `ParseError::PayloadTooLong { len }` beats `ParseError::Invalid`.

**No panicking in production paths.** `unwrap`, `expect`, `panic!`, and raw slice
indexing are `deny`-level clippy lints workspace-wide, relaxed automatically inside
`#[cfg(test)]`. This is a server that stays up for months holding thousands of sockets;
a panic in a parser is a denial of service. Use `get()`, `?`, and total functions. If you
genuinely need an invariant assertion, document why it cannot fire.

**Untrusted input.** Every byte arriving from a socket is hostile until parsed. Enforce
the 512-byte APRS-IS line limit at the codec, validate before allocating, and never let a
malformed packet abort a connection task in a way that affects other clients.

**Allocation on the hot path.** `dispatch` runs for every packet against every connected
client. Prefer borrowing (`&str`, `Tnc2Packet<'a>`) over owning, prefer `Arc<str>` for the
one copy fanned out to many clients, and do not format strings you might not send.

**Tracing.** Use `tracing`, not `println!`. Spans for connection lifecycles, structured
fields rather than interpolated strings: `tracing::info!(callsign = %call, "client
logged in")`. Never log a passcode.

**Naming.** Use APRS-IS vocabulary as-is — `callsign`, `passcode`, `igate`, `q_construct`,
`unproto`, `fullfeed`, `dupecheck`. A reader who knows the network should recognise every
identifier. Do not invent synonyms.

---

## 5. Testing — non-negotiable

**No change lands without tests.** This is the project's hard rule. If you add a filter
type, a packet format, a route, or a config directive, you add tests for it in the same
change.

Where tests go:

| Kind | Location |
|---|---|
| Unit tests | `#[cfg(test)] mod tests` at the bottom of the module under test |
| Property tests | Same file, `proptest!` blocks — used for parsers and the dupe checker |
| Crate integration tests | `crates/<crate>/tests/*.rs` |
| Snapshots | `insta`, snapshots committed under `snapshots/` |
| TypeScript | `web/src/ts/**/*.test.ts`, run by vitest |

What good tests look like here:

- **Table-driven with `rstest`** for anything with many input shapes — filters, packet
  types, callsign validation. One `#[case]` per documented behaviour.
- **Real packets.** Use genuine APRS packet text in fixtures, not `"foo>bar:baz"`. There
  are examples in `crates/aprsr-core/src/aprs/mod.rs` tests.
- **Both directions.** A parser test that only checks the happy path is half a test.
  Assert the specific error for malformed input too.
- **Determinism.** Never use wall-clock time or real sleeps. `dupecheck` takes the time
  as a parameter for exactly this reason. Server integration tests bind port 0 and read
  the assigned port.

---

## 6. Recipes

**Adding a server-side filter type**

1. Read the syntax at <http://www.aprs-is.net/javAPRSFilter.aspx>.
2. Add the variant to `Filter` in `crates/aprsr-core/src/filter/mod.rs`.
3. Parse it in `Filter::parse`, rejecting malformed input with a specific `FilterError`.
4. Implement matching in `Filter::matches`. If it needs station positions, go through
   the `PositionSource` trait — do not reach for the database directly.
5. Add `#[case]` rows covering: a match, a non-match, the negated form (`-`), and at
   least two parse errors.
6. Document the letter code in `docs/protocol.md`.

**Adding a database table**

1. New migration in `crates/aprsr-store/src/migration/`, named `m<date>_<seq>_<what>.rs`.
2. Register it in `Migrator::migrations()`.
3. Add the entity under `crates/aprsr-store/src/entity/`.
4. Test it in `crates/aprsr-store/tests/` against `sqlite::memory:` — migrate, insert,
   read back.
5. Migrations are append-only. Never edit one that has shipped.

**Changing the dashboard**

1. Templates are Askama, in `crates/aprsr-web/templates/`. They are compile-time checked,
   so a bad template is a build error — that is intentional.
2. Interactivity is HTMX first. Reach for TypeScript only when HTMX cannot express it.
3. Styling is Tailwind utility classes. Shared design tokens live in
   `web/src/css/theme.css` and are used by both the dashboard and the landing page.
4. **After touching anything under `web/`, run `make web` and commit the regenerated
   `crates/aprsr-web/static/` output.** CI fails if it is stale. The assets are committed
   so that `cargo run` works on a clone with no Node installed.

---

## 7. Before you finish

- [ ] `make ci` passes — and it did not print the cargo-deny SKIPPED banner
- [ ] New behaviour has tests; new protocol behaviour cites its spec URL
- [ ] No aprsc code was copied
- [ ] `crates/aprsr-web/static/` regenerated if `web/` changed
- [ ] `docs/roadmap.md` updated if you completed or added a roadmap item
- [ ] No `unwrap`/`expect`/`panic!` outside tests
