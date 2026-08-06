//! The last known position of each station.
//!
//! Backs the `m/` and `f/` filters, which are relative to a station's most recent
//! position rather than to anything in the packet being matched.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "station_position")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Callsign with SSID. Unique — one row per station, updated in place.
    #[sea_orm(unique, indexed)]
    pub callsign: String,
    pub latitude: f64,
    pub longitude: f64,
    /// Symbol table selector, one character.
    pub symbol_table: Option<String>,
    /// Symbol code, one character.
    pub symbol_code: Option<String>,
    /// Unix seconds when this position was last heard.
    pub heard_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
