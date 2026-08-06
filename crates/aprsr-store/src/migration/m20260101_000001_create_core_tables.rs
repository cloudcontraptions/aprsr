//! The initial schema.
//!
//! Migrations are append-only: never edit this file once it has shipped. To change a
//! table, add a new migration that alters it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(super) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_station_position(manager).await?;
        create_client_session(manager).await?;
        create_counter_sample(manager).await?;
        create_acl_entry(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse creation order so a future migration adding foreign keys still drops
        // cleanly.
        manager
            .drop_table(Table::drop().table(AclEntry::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(CounterSample::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ClientSession::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(StationPosition::Table).to_owned())
            .await?;
        Ok(())
    }
}

async fn create_station_position(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(StationPosition::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(StationPosition::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                // Nine characters is the APRS-IS maximum callsign-SSID length.
                .col(
                    ColumnDef::new(StationPosition::Callsign)
                        .string_len(9)
                        .not_null()
                        .unique_key(),
                )
                .col(
                    ColumnDef::new(StationPosition::Latitude)
                        .double()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(StationPosition::Longitude)
                        .double()
                        .not_null(),
                )
                .col(ColumnDef::new(StationPosition::SymbolTable).string_len(1))
                .col(ColumnDef::new(StationPosition::SymbolCode).string_len(1))
                .col(
                    ColumnDef::new(StationPosition::HeardAt)
                        .big_integer()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;

    // The m/ and f/ filters look up by callsign for every packet they consider.
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_station_position_callsign")
                .table(StationPosition::Table)
                .col(StationPosition::Callsign)
                .to_owned(),
        )
        .await?;

    Ok(())
}

async fn create_client_session(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(ClientSession::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(ClientSession::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(ClientSession::Callsign)
                        .string_len(9)
                        .not_null(),
                )
                .col(
                    ColumnDef::new(ClientSession::RemoteAddr)
                        .string()
                        .not_null(),
                )
                .col(ColumnDef::new(ClientSession::Listener).string().not_null())
                .col(ColumnDef::new(ClientSession::Software).string())
                .col(ColumnDef::new(ClientSession::Verified).boolean().not_null())
                .col(ColumnDef::new(ClientSession::Filter).string())
                .col(
                    ColumnDef::new(ClientSession::ConnectedAt)
                        .big_integer()
                        .not_null(),
                )
                .col(ColumnDef::new(ClientSession::DisconnectedAt).big_integer())
                .col(
                    ColumnDef::new(ClientSession::PacketsReceived)
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .col(
                    ColumnDef::new(ClientSession::PacketsSent)
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .col(
                    ColumnDef::new(ClientSession::PacketsDropped)
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .col(
                    ColumnDef::new(ClientSession::BytesReceived)
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .col(
                    ColumnDef::new(ClientSession::BytesSent)
                        .big_integer()
                        .not_null()
                        .default(0),
                )
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_client_session_callsign")
                .table(ClientSession::Table)
                .col(ClientSession::Callsign)
                .to_owned(),
        )
        .await?;

    // The dashboard lists the most recent sessions first.
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_client_session_connected_at")
                .table(ClientSession::Table)
                .col(ClientSession::ConnectedAt)
                .to_owned(),
        )
        .await?;

    Ok(())
}

async fn create_counter_sample(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(CounterSample::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(CounterSample::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(ColumnDef::new(CounterSample::Name).string().not_null())
                .col(
                    ColumnDef::new(CounterSample::SampledAt)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(CounterSample::Value)
                        .big_integer()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;

    // Graphs query one counter over a time range, so index the pair.
    manager
        .create_index(
            Index::create()
                .if_not_exists()
                .name("idx_counter_sample_name_time")
                .table(CounterSample::Table)
                .col(CounterSample::Name)
                .col(CounterSample::SampledAt)
                .to_owned(),
        )
        .await?;

    Ok(())
}

async fn create_acl_entry(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AclEntry::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AclEntry::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(AclEntry::Cidr)
                        .string()
                        .not_null()
                        .unique_key(),
                )
                .col(ColumnDef::new(AclEntry::Allow).boolean().not_null())
                .col(ColumnDef::new(AclEntry::Note).string())
                .col(ColumnDef::new(AclEntry::CreatedAt).big_integer().not_null())
                .to_owned(),
        )
        .await?;

    Ok(())
}

#[derive(DeriveIden)]
enum StationPosition {
    Table,
    Id,
    Callsign,
    Latitude,
    Longitude,
    SymbolTable,
    SymbolCode,
    HeardAt,
}

#[derive(DeriveIden)]
enum ClientSession {
    Table,
    Id,
    Callsign,
    RemoteAddr,
    Listener,
    Software,
    Verified,
    Filter,
    ConnectedAt,
    DisconnectedAt,
    PacketsReceived,
    PacketsSent,
    PacketsDropped,
    BytesReceived,
    BytesSent,
}

#[derive(DeriveIden)]
enum CounterSample {
    Table,
    Id,
    Name,
    SampledAt,
    Value,
}

#[derive(DeriveIden)]
enum AclEntry {
    Table,
    Id,
    Cidr,
    Allow,
    Note,
    CreatedAt,
}
