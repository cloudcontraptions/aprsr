//! Schema migrations.
//!
//! Migrations run automatically when the server starts, so a broken one means a server
//! that will not boot. Ordering comes from this list, not from the file names.
//!
//! **Migrations are append-only.** Never edit one that has shipped — deployments that
//! already ran it would silently diverge from fresh installations. To change a table, add
//! a new migration.

pub use sea_orm_migration::prelude::*;

mod m20260101_000001_create_core_tables;

#[derive(Debug)]
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260101_000001_create_core_tables::Migration)]
    }
}
