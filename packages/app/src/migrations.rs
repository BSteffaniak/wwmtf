//! Application-owned schema migrations built exclusively with `switchy` builders.

use switchy_database::{
    Database,
    schema::{Column, DataType, create_index, create_table, drop_index, drop_table},
};
use switchy_schema::{
    discovery::code::{CodeMigration, CodeMigrationSource},
    runner::MigrationRunner,
};

/// Returns the versioned application schema migration source.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn app_migrations() -> CodeMigrationSource<'static> {
    let mut source = CodeMigrationSource::new();
    source.add_migration(table_migration(
        "001_users",
        "users",
        vec![
            text("user_id"),
            text("username_normalized"),
            text("username_display"),
            bigint("created_at_ms"),
        ],
        "user_id",
    ));
    source.add_migration(index_migration(
        "002_users_username_unique",
        "idx_users_username_normalized",
        "users",
        vec!["username_normalized"],
        true,
    ));
    source.add_migration(table_migration(
        "003_password_credentials",
        "password_credentials",
        vec![
            text("user_id"),
            text("password_hash"),
            bigint("updated_at_ms"),
        ],
        "user_id",
    ));
    source.add_migration(table_migration(
        "004_auth_sessions",
        "auth_sessions",
        vec![
            text("session_hash"),
            text("user_id"),
            bigint("expires_at_ms"),
            nullable_bigint("revoked_at_ms"),
            bigint("created_at_ms"),
        ],
        "session_hash",
    ));
    source.add_migration(table_migration(
        "005_games",
        "games",
        vec![
            text("game_id"),
            text("rules_id"),
            bigint("rules_version"),
            text("dictionary_id"),
            bigint("dictionary_version"),
            text("dictionary_checksum"),
            bigint("canonical_revision"),
            text("status"),
            bigint("created_at_ms"),
            bigint("updated_at_ms"),
        ],
        "game_id",
    ));
    source.add_migration(table_migration(
        "006_game_players",
        "game_players",
        vec![
            text("game_player_id"),
            text("game_id"),
            text("user_id"),
            bigint("seat"),
        ],
        "game_player_id",
    ));
    source.add_migration(index_migration(
        "007_game_players_unique",
        "idx_game_players_game_user",
        "game_players",
        vec!["game_id", "user_id"],
        true,
    ));
    source.add_migration(table_migration(
        "008_challenges",
        "challenges",
        vec![
            text("challenge_id"),
            text("challenger_user_id"),
            text("challenged_user_id"),
            text("status"),
            bigint("created_at_ms"),
            bigint("updated_at_ms"),
        ],
        "challenge_id",
    ));
    source.add_migration(table_migration(
        "009_invitations",
        "invitations",
        vec![
            text("invitation_id"),
            text("creator_user_id"),
            text("token_hash"),
            text("status"),
            bigint("expires_at_ms"),
            nullable_text("redeemed_by_user_id"),
            bigint("created_at_ms"),
        ],
        "invitation_id",
    ));
    source.add_migration(index_migration(
        "010_invitations_token_unique",
        "idx_invitations_token_hash",
        "invitations",
        vec!["token_hash"],
        true,
    ));
    source.add_migration(table_migration(
        "011_projection_checkpoints",
        "projection_checkpoints",
        vec![
            text("projection_id"),
            text("game_id"),
            bigint("revision"),
            bigint("updated_at_ms"),
        ],
        "projection_id",
    ));
    source.add_migration(table_migration(
        "012_game_summaries",
        "game_summaries",
        vec![
            text("game_id"),
            text("status"),
            nullable_text("active_player_user_id"),
            bigint("canonical_revision"),
            nullable_bigint("last_score"),
            nullable_text("winner_user_id"),
            bigint("updated_at_ms"),
        ],
        "game_id",
    ));
    source.add_migration(table_migration(
        "013_move_history",
        "move_history",
        vec![
            text("move_id"),
            text("game_id"),
            bigint("revision"),
            nullable_text("player_user_id"),
            text("event_kind"),
            bigint("score_delta"),
            bigint("created_at_ms"),
        ],
        "move_id",
    ));
    source.add_migration(index_migration(
        "014_move_history_revision_unique",
        "idx_move_history_game_revision",
        "move_history",
        vec!["game_id", "revision"],
        true,
    ));
    source.add_migration(table_migration(
        "015_user_score_totals",
        "user_score_totals",
        vec![
            text("user_id"),
            bigint("completed_games"),
            bigint("wins"),
            bigint("ties"),
            bigint("total_score"),
            bigint("updated_at_ms"),
        ],
        "user_id",
    ));
    source.add_migration(table_migration(
        "020_game_journal",
        "game_journal",
        vec![
            text("event_id"),
            text("game_id"),
            bigint("revision"),
            text("command_id"),
            text("idempotency_key"),
            bigint("payload_version"),
            text("payload"),
        ],
        "event_id",
    ));
    source.add_migration(index_migration(
        "021_game_journal_revision_unique",
        "idx_game_journal_game_revision",
        "game_journal",
        vec!["game_id", "revision"],
        true,
    ));
    source.add_migration(index_migration(
        "022_game_journal_command_unique",
        "idx_game_journal_game_command",
        "game_journal",
        vec!["game_id", "command_id"],
        true,
    ));
    source.add_migration(index_migration(
        "023_game_journal_idempotency_unique",
        "idx_game_journal_game_idempotency",
        "game_journal",
        vec!["game_id", "idempotency_key"],
        true,
    ));
    source.add_migration(table_migration(
        "024_game_snapshots",
        "game_snapshots",
        vec![
            text("snapshot_id"),
            text("game_id"),
            bigint("revision"),
            bigint("payload_version"),
            text("payload"),
            bigint("created_at_ms"),
        ],
        "snapshot_id",
    ));
    source.add_migration(CodeMigration::new(
        "025_drop_journal_command_unique".to_string(),
        Box::new(drop_index("idx_game_journal_game_command", "game_journal")),
        Some(Box::new(
            create_index("idx_game_journal_game_command")
                .table("game_journal")
                .columns(vec!["game_id", "command_id"])
                .unique(true),
        )),
    ));
    source.add_migration(CodeMigration::new(
        "026_drop_journal_idempotency_unique".to_string(),
        Box::new(drop_index(
            "idx_game_journal_game_idempotency",
            "game_journal",
        )),
        Some(Box::new(
            create_index("idx_game_journal_game_idempotency")
                .table("game_journal")
                .columns(vec!["game_id", "idempotency_key"])
                .unique(true),
        )),
    ));
    source.add_migration(table_migration(
        "027_game_commands",
        "game_commands",
        vec![
            text("game_command_id"),
            text("game_id"),
            text("command_id"),
            text("idempotency_key"),
            bigint("expected_revision"),
            bigint("resulting_revision"),
        ],
        "game_command_id",
    ));
    source.add_migration(index_migration(
        "028_game_commands_command_unique",
        "idx_game_commands_game_command",
        "game_commands",
        vec!["game_id", "command_id"],
        true,
    ));
    source.add_migration(index_migration(
        "029_game_commands_idempotency_unique",
        "idx_game_commands_game_idempotency",
        "game_commands",
        vec!["game_id", "idempotency_key"],
        true,
    ));
    source.add_migration(table_migration(
        "030_game_scores",
        "game_scores",
        vec![
            text("game_player_score_id"),
            text("game_id"),
            text("user_id"),
            bigint("score"),
            text("outcome"),
            bigint("updated_at_ms"),
        ],
        "game_player_score_id",
    ));
    source.add_migration(index_migration(
        "031_game_scores_user",
        "idx_game_scores_user",
        "game_scores",
        vec!["user_id"],
        false,
    ));
    source.add_migration(table_migration(
        "032_rack_preferences",
        "rack_preferences",
        vec![
            text("rack_preference_id"),
            text("game_id"),
            text("user_id"),
            text("tile_order"),
            bigint("updated_at_ms"),
        ],
        "rack_preference_id",
    ));
    source.add_migration(index_migration(
        "033_rack_preferences_game_user_unique",
        "idx_rack_preferences_game_user",
        "rack_preferences",
        vec!["game_id", "user_id"],
        true,
    ));
    source.add_migration(table_migration(
        "034_definition_cache",
        "definition_cache",
        vec![
            text("definition_cache_id"),
            text("provider"),
            bigint("provider_version"),
            text("language"),
            text("word"),
            text("status"),
            nullable_text("payload"),
            bigint("fetched_at_ms"),
            bigint("expires_at_ms"),
        ],
        "definition_cache_id",
    ));
    source
}

/// Runs all application-owned code migrations before traffic is accepted.
///
/// `HyperChad` shared-state migrations remain owned and executed by `HyperChad`; this runner uses a
/// separate metadata table for application schema versions.
///
/// # Errors
///
/// * Returns a schema migration error when migration discovery or execution fails.
pub async fn migrate_app(db: &dyn Database) -> switchy_schema::Result<()> {
    MigrationRunner::new(Box::new(app_migrations()))
        .with_table_name("__wwmtf_migrations")
        .run(db)
        .await
}

fn table_migration(
    id: &str,
    table: &'static str,
    columns: Vec<Column>,
    primary_key: &'static str,
) -> CodeMigration<'static> {
    let mut statement = create_table(table);
    for column in columns {
        statement = statement.column(column);
    }
    CodeMigration::new(
        id.to_string(),
        Box::new(statement.primary_key(primary_key)),
        Some(Box::new(drop_table(table).if_exists(true))),
    )
}

fn index_migration(
    id: &str,
    index: &'static str,
    table: &'static str,
    columns: Vec<&'static str>,
    unique: bool,
) -> CodeMigration<'static> {
    let statement = create_index(index)
        .table(table)
        .columns(columns)
        .unique(unique);
    CodeMigration::new(
        id.to_string(),
        Box::new(statement),
        Some(Box::new(drop_index(index, table).if_exists())),
    )
}

fn text(name: &str) -> Column {
    column(name, DataType::Text, false)
}

fn nullable_text(name: &str) -> Column {
    column(name, DataType::Text, true)
}

fn bigint(name: &str) -> Column {
    column(name, DataType::BigInt, false)
}

fn nullable_bigint(name: &str) -> Column {
    column(name, DataType::BigInt, true)
}

fn column(name: &str, data_type: DataType, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        nullable,
        auto_increment: false,
        data_type,
        default: None,
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use switchy_schema::{
        migration::MigrationSource as _,
        runner::{ExecutionStrategy, MigrationRunner},
    };

    use super::*;

    #[test]
    fn application_schema_has_stable_migration_count() {
        let source = app_migrations();
        let migrations = block_on(source.migrations()).expect("migrations are discoverable");
        assert_eq!(migrations.len(), 30);
        assert_eq!(migrations[0].id(), "001_users");
        assert_eq!(migrations[29].id(), "034_definition_cache");
    }

    #[test]
    fn every_retained_migration_version_upgrades_to_the_current_schema() {
        block_on(async {
            let migrations = app_migrations()
                .migrations()
                .await
                .expect("migrations are discoverable");
            for retained in migrations.iter().take(migrations.len() - 1) {
                let db = switchy_database_connection::builder()
                    .turso()
                    .with_in_memory()
                    .build()
                    .await
                    .expect("in-memory Turso opens");
                MigrationRunner::new(Box::new(app_migrations()))
                    .with_table_name("__wwmtf_migrations")
                    .with_strategy(ExecutionStrategy::UpTo(retained.id().to_string()))
                    .run(&*db)
                    .await
                    .expect("retained schema version installs");
                migrate_app(&*db)
                    .await
                    .expect("retained schema upgrades to current");

                for table in [
                    "users",
                    "password_credentials",
                    "auth_sessions",
                    "games",
                    "game_players",
                    "challenges",
                    "invitations",
                    "projection_checkpoints",
                    "game_summaries",
                    "move_history",
                    "user_score_totals",
                    "game_journal",
                    "game_commands",
                    "game_snapshots",
                    "game_scores",
                    "rack_preferences",
                    "definition_cache",
                ] {
                    assert!(
                        db.table_exists(table).await.expect("schema query succeeds"),
                        "{table} must exist after upgrading from {}",
                        retained.id()
                    );
                }
            }
        });
    }

    #[test]
    fn migrations_execute_on_the_turso_backend() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            migrate_app(&*db).await.expect("migrations run");

            for table in [
                "users",
                "auth_sessions",
                "games",
                "game_journal",
                "game_commands",
                "game_snapshots",
                "projection_checkpoints",
                "game_scores",
                "rack_preferences",
                "definition_cache",
            ] {
                assert!(db.table_exists(table).await.expect("schema query succeeds"));
            }
        });
    }
}
