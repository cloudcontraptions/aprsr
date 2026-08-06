//! Periodic samples of the server's traffic counters.
//!
//! Sampling into a table rather than keeping only live totals is what lets the dashboard
//! draw history, and what lets an operator answer "was it like this an hour ago?".

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "counter_sample")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Counter name, e.g. `packets_received` or `clients_connected`.
    #[sea_orm(indexed)]
    pub name: String,
    /// Unix seconds the sample was taken.
    #[sea_orm(indexed)]
    pub sampled_at: i64,
    pub value: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
