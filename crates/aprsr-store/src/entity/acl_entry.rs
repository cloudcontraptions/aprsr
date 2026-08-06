//! Address-based access control entries.
//!
//! Enforcement is on the roadmap rather than in this release; the table exists now so the
//! schema does not have to change underneath an operator who has already populated it.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "acl_entry")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// CIDR block in text form, e.g. `192.0.2.0/24` or `2001:db8::/32`.
    #[sea_orm(unique)]
    pub cidr: String,
    /// True to allow the block, false to deny it.
    pub allow: bool,
    /// Why this entry exists — invaluable a year later.
    pub note: Option<String>,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
