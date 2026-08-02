//! Private, non-authoritative rack-order presentation preferences.

use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use words_with_spouses_game_domain::{GameId, TileId};

/// Reconciles a preferred order with the authoritative rack.
///
/// Missing tiles are removed and newly drawn tiles are appended in authoritative rack order.
#[must_use]
pub fn reconcile_rack_order(preferred: &[u16], authoritative: &[u16]) -> Vec<u16> {
    let authoritative_members = authoritative
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen = std::collections::BTreeSet::new();
    let mut reconciled = preferred
        .iter()
        .copied()
        .filter(|tile_id| authoritative_members.contains(tile_id) && seen.insert(*tile_id))
        .collect::<Vec<_>>();
    reconciled.extend(
        authoritative
            .iter()
            .copied()
            .filter(|tile_id| seen.insert(*tile_id)),
    );
    reconciled
}

/// Loads and reconciles one authorized player's private rack preference.
///
/// # Errors
///
/// Returns membership, journal, malformed preference, or database failures.
pub async fn load_rack_order(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
) -> Result<Vec<u16>, RackPreferenceError> {
    let player = crate::player_for_user(db, game_id, user_id).await?;
    let state = crate::recover_game(db, game_id).await?;
    let authoritative = state.racks[&player]
        .iter()
        .map(|tile| tile.id.get())
        .collect::<Vec<_>>();
    let preference_id = preference_id(game_id, user_id);
    let preferred = db
        .select("rack_preferences")
        .where_eq("rack_preference_id", preference_id)
        .execute(db)
        .await?
        .first()
        .and_then(|row| row.get("tile_order"))
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .map(|value| serde_json::from_str::<Vec<u16>>(&value))
        .transpose()?
        .unwrap_or_default();
    Ok(reconcile_rack_order(&preferred, &authoritative))
}

/// Persists one authorized player's exact private rack preference after reconciliation.
///
/// # Errors
///
/// Returns membership, journal, serialization, or database failures.
pub async fn save_rack_order(
    db: &dyn Database,
    game_id: GameId,
    user_id: &str,
    preferred: &[u16],
    updated_at_ms: i64,
) -> Result<Vec<u16>, RackPreferenceError> {
    let player = crate::player_for_user(db, game_id, user_id).await?;
    let state = crate::recover_game(db, game_id).await?;
    let authoritative = state.racks[&player]
        .iter()
        .map(|tile| tile.id.get())
        .collect::<Vec<_>>();
    let reconciled = reconcile_rack_order(preferred, &authoritative);
    db.upsert("rack_preferences")
        .where_eq("rack_preference_id", preference_id(game_id, user_id))
        .value("rack_preference_id", preference_id(game_id, user_id))
        .value("game_id", game_id.to_string())
        .value("user_id", user_id)
        .value("tile_order", serde_json::to_string(&reconciled)?)
        .value("updated_at_ms", updated_at_ms)
        .execute(db)
        .await?;
    Ok(reconciled)
}

fn preference_id(game_id: GameId, user_id: &str) -> String {
    format!("{game_id}:{user_id}")
}

/// Rack-preference persistence failure.
#[derive(Debug, Error)]
pub enum RackPreferenceError {
    #[error(transparent)]
    Game(#[from] crate::GameServiceError),
    #[error(transparent)]
    Journal(#[from] crate::JournalError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

/// Swaps two rack tiles while preserving every other position.
#[must_use]
pub fn swap_rack_tiles(order: &[u16], first: TileId, second: TileId) -> Vec<u16> {
    let mut order = order.to_vec();
    let Some(first_index) = order.iter().position(|tile_id| *tile_id == first.get()) else {
        return order;
    };
    let Some(second_index) = order.iter().position(|tile_id| *tile_id == second.get()) else {
        return order;
    };
    order.swap(first_index, second_index);
    order
}

#[cfg(test)]
mod tests {
    use switchy_database::query::FilterableQuery as _;
    use words_with_spouses_game_domain::GameCommand;

    use super::*;

    #[test]
    fn reconciliation_removes_stale_duplicates_and_appends_draws() {
        assert_eq!(
            reconcile_rack_order(&[3, 2, 2, 9], &[1, 2, 3, 4]),
            vec![3, 2, 1, 4]
        );
    }

    #[test]
    fn rack_preferences_persist_privately_and_reconcile_authoritative_changes() {
        futures_lite::future::block_on(async {
            use time::OffsetDateTime;

            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens");
            crate::migrate_app(&*db).await.expect("migrations run");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = crate::register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = crate::register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let challenge = crate::create_challenge(&*db, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = crate::accept_challenge(&*db, &challenge, &bob, now, 7)
                .await
                .expect("game starts");
            let original = load_rack_order(&*db, game_id, &alice)
                .await
                .expect("Alice order loads");
            let preferred = original.iter().rev().copied().collect::<Vec<_>>();

            let saved = save_rack_order(&*db, game_id, &alice, &preferred, 1)
                .await
                .expect("Alice order saves");
            assert_eq!(saved, preferred);
            assert_eq!(
                load_rack_order(&*db, game_id, &alice)
                    .await
                    .expect("Alice order reloads"),
                preferred
            );
            let alice_player = crate::player_for_user(&*db, game_id, &alice)
                .await
                .expect("Alice is seated");
            let before_exchange = crate::recover_game(&*db, game_id)
                .await
                .expect("game recovers");
            let exchanged_tile = before_exchange.racks[&alice_player][0];
            let expected_new_tiles = before_exchange
                .bag
                .iter()
                .rev()
                .take(1)
                .map(|tile| tile.id.get())
                .collect::<Vec<_>>();
            crate::submit_game_command(
                &*db,
                game_id,
                &alice,
                "rack-order-exchange",
                "rack-order-exchange-idempotency",
                before_exchange.revision,
                &GameCommand::Exchange {
                    tile_ids: std::iter::once(exchanged_tile.id).collect(),
                },
                2,
            )
            .await
            .expect("Alice exchanges");
            let after_exchange = load_rack_order(&*db, game_id, &alice)
                .await
                .expect("order reconciles after an exchange");
            assert_eq!(
                after_exchange,
                preferred
                    .iter()
                    .copied()
                    .filter(|tile_id| *tile_id != exchanged_tile.id.get())
                    .chain(expected_new_tiles)
                    .collect::<Vec<_>>()
            );

            assert_ne!(
                load_rack_order(&*db, game_id, &bob)
                    .await
                    .expect("Bob order loads"),
                preferred,
                "one player's private order cannot overwrite the other's"
            );

            let rows = db
                .select("rack_preferences")
                .where_eq("game_id", game_id.to_string())
                .execute(&*db)
                .await
                .expect("preferences query succeeds");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0]
                    .get("user_id")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned)),
                Some(alice)
            );
        });
    }

    #[test]
    fn tile_swap_preserves_membership_and_other_positions() {
        assert_eq!(
            swap_rack_tiles(&[1, 2, 3, 4], TileId::new(2), TileId::new(4)),
            [1, 4, 3, 2]
        );
        assert_eq!(
            swap_rack_tiles(&[1, 2], TileId::new(9), TileId::new(1)),
            [1, 2]
        );
    }
}
