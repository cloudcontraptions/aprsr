<!--
Thanks for contributing to aprsr.

CONTRIBUTING.md and AGENTS.md carry the full detail. The checklist below is the short
version — it is what a reviewer will actually look for.
-->

## What this changes

<!-- What behaviour is different after this than before it? One paragraph is usually
     enough. If it fixes a bug, say what the bug did. -->

## Why

<!-- The reasoning, not a restatement of the diff. If this implements protocol behaviour,
     link the specification section it comes from. -->

## How it was verified

<!-- What you actually ran, and what it said. "make ci passes" is fine if it does; if
     something is untested, say which part and why rather than leaving it implied. -->

## Checklist

- [ ] `make ci` passes, and it did **not** print the cargo-deny SKIPPED banner
- [ ] New behaviour has tests, in the same change
- [ ] New protocol behaviour cites its specification URL in a doc comment
- [ ] No aprsc code was copied — see [`docs/attribution.md`](../docs/attribution.md)
- [ ] `crates/aprsr-web/static/` was regenerated with `make web` if anything under `web/`
      changed
- [ ] [`docs/roadmap.md`](../docs/roadmap.md) updated if this completes or adds an item
- [ ] [`CHANGELOG.md`](../CHANGELOG.md) has an entry under *Unreleased*
- [ ] No `unwrap`/`expect`/`panic!` outside tests
