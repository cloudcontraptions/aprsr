---
name: add-migration
description: Add or alter a SQLite table in aprsr — SeaORM migration, entity, registration, and test. Use when persisted state needs a new table or column.
---

# Adding a database table or column

aprsr persists to SQLite through SeaORM. Migrations run automatically at server start, so
a broken migration means a server that will not boot — and an *edited* migration means
existing deployments silently diverge from new ones.

**Migrations are append-only. Never edit a migration that has shipped.** To change a
shipped table, add a new migration that alters it.

## 1. Write the migration

Create `crates/aprsr-store/src/migration/m<YYYYMMDD>_<NNNNNN>_<what>.rs`, following the
naming of the existing files so ordering stays lexicographic. Implement both `up` and
`down` — `down` is what makes the migration testable, even if you never run it in
production.

Guidance specific to this project:

- Every table gets an explicit primary key. Use an autoincrement integer unless there is a
  natural key.
- Timestamps are stored as integer Unix seconds, not text, and are named `*_at`.
- Callsign columns are `VARCHAR(9)` — that is the APRS-IS maximum.
- Add the index in the same migration as the column it serves. `station_position` is
  queried by callsign on the `m/` and `f/` filter path and needs to stay fast.
- SQLite has limited `ALTER TABLE` support: adding a column is fine, changing or dropping
  one requires the create-copy-drop-rename dance. Write it out rather than assuming.

## 2. Register it

Add the module to `crates/aprsr-store/src/migration/mod.rs` and append it to the vector
returned by `Migrator::migrations()`. Order matters and is not inferred from the filename.

## 3. Add or update the entity

`crates/aprsr-store/src/entity/<table>.rs` — the `Model`, `Column`, `PrimaryKey`, and
`Relation` definitions must match the migration exactly. A mismatch compiles fine and
fails at runtime on the first query.

Re-export it from `crates/aprsr-store/src/entity/mod.rs`.

## 4. Test it

`crates/aprsr-store/tests/` — against `sqlite::memory:`:

1. Run `Migrator::up` and assert it succeeds.
2. Insert a row through the entity, read it back, assert every field round-trips —
   this is what catches migration/entity mismatches.
3. Exercise whatever query the new column exists to serve.
4. Run `Migrator::down` far enough to prove the rollback works.

## 5. Wire it up

If the table backs a trait — `station_position` backs `PositionSource` for the `m/` and
`f/` filters — implement or extend that trait and test it through the trait, not only
through the entity.

## 6. Verify

```bash
cargo test -p aprsr-store
cargo run -p aprsr -- run --config aprsr.example.toml   # migrations apply on a fresh DB
```

Delete any local `data/aprsr.sqlite` first so you are genuinely testing a fresh migration
run, then start the server a second time to confirm re-running is a no-op.
