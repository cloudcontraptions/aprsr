//! One row per client connection, opened at login and closed at disconnect.
//!
//! The status dashboard reads the open rows; the closed ones are the connection history
//! an operator needs when someone asks why their IGate dropped.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "client_session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub callsign: String,
    /// Remote address and port, as text so IPv4 and IPv6 share one column.
    pub remote_addr: String,
    /// Name of the listener the client connected to.
    pub listener: String,
    /// Client software name and version from the login line, when supplied.
    pub software: Option<String>,
    /// Whether the client presented a valid passcode.
    pub verified: bool,
    /// The filter expression in force, for `igate` ports.
    pub filter: Option<String>,
    /// Unix seconds at login.
    #[sea_orm(indexed)]
    pub connected_at: i64,
    /// Unix seconds at disconnect; `None` while the client is still connected.
    pub disconnected_at: Option<i64>,
    pub packets_received: i64,
    pub packets_sent: i64,
    pub packets_dropped: i64,
    pub bytes_received: i64,
    pub bytes_sent: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
