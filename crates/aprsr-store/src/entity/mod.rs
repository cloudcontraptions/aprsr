//! SeaORM entities.
//!
//! Each module's `Model` must match the corresponding migration exactly — a mismatch
//! compiles cleanly and fails at runtime on the first query, so the store tests insert
//! and read back every field.

pub mod acl_entry;
pub mod client_session;
pub mod counter_sample;
pub mod station_position;

pub mod prelude {
    pub use super::acl_entry::Entity as AclEntry;
    pub use super::client_session::Entity as ClientSession;
    pub use super::counter_sample::Entity as CounterSample;
    pub use super::station_position::Entity as StationPosition;
}
