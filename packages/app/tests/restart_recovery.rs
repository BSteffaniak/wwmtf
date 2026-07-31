use futures_lite::future::block_on;
use switchy_database::query::FilterableQuery as _;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use words_with_spouses_app::{
    accept_challenge, create_challenge, create_session, dashboard_projection, load_events,
    migrate_app, rebuild_game_projections, recover_game, register, resolve_session, store_snapshot,
    user_game_summaries,
};
use words_with_spouses_game_domain::{GameId, GameState};

fn database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("words-with-spouses-restart-{}.db", Uuid::new_v4()))
}

async fn open_database(path: &std::path::Path) -> Box<dyn switchy_database::Database> {
    switchy_database_connection::builder()
        .turso()
        .with_path(path)
        .with_busy_timeout(std::time::Duration::from_secs(5))
        .build()
        .await
        .expect("file-backed Turso opens")
}

async fn rebuild(db: &dyn switchy_database::Database, game_id: GameId, state: &GameState) {
    let events = load_events(db, game_id, 0)
        .await
        .expect("canonical events load")
        .into_iter()
        .map(|event| event.event)
        .collect::<Vec<_>>();
    let tx = db.begin_transaction().await.expect("transaction begins");
    rebuild_game_projections(&*tx, state, &events, 0)
        .await
        .expect("projections rebuild");
    tx.commit().await.expect("projection transaction commits");
}

fn remove_database_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-shm", "-wal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
    }
}

#[test]
fn simultaneous_same_revision_commands_have_one_winner() {
    block_on(async {
        let path = database_path();
        let now = OffsetDateTime::UNIX_EPOCH;
        let game_id = {
            let db = open_database(&path).await;
            migrate_app(&*db).await.expect("migrations run");
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let challenge = create_challenge(&*db, &alice, &bob, now)
                .await
                .expect("challenge creates");
            accept_challenge(&*db, &challenge, &bob, now, 7)
                .await
                .expect("game starts")
        };

        let first_db = open_database(&path).await;
        let second_db = open_database(&path).await;
        let state = recover_game(&*first_db, game_id)
            .await
            .expect("game recovers");
        let event = words_with_spouses_game_domain::GameEvent::TurnPassed {
            player_id: state.active_player,
        };
        let first = words_with_spouses_app::append_events_transactionally(
            &*first_db,
            game_id,
            "same-revision-a",
            "same-revision-idem-a",
            state.revision,
            std::slice::from_ref(&event),
        );
        let second = words_with_spouses_app::append_events_transactionally(
            &*second_db,
            game_id,
            "same-revision-b",
            "same-revision-idem-b",
            state.revision,
            std::slice::from_ref(&event),
        );
        let (first, second) = futures_lite::future::zip(first, second).await;
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);

        let recovered = recover_game(&*first_db, game_id)
            .await
            .expect("winning command recovers");
        assert_eq!(recovered.revision, state.revision + 1);
        assert_eq!(
            load_events(&*first_db, game_id, state.revision)
                .await
                .expect("tail loads")
                .len(),
            1
        );

        drop(first_db);
        drop(second_db);
        remove_database_files(&path);
    });
}

#[test]
fn accounts_concurrent_games_and_projections_survive_restart() {
    block_on(async {
        let path = database_path();
        let now = OffsetDateTime::UNIX_EPOCH;
        let (alice, session, expected_games) = {
            let db = open_database(&path).await;
            migrate_app(&*db).await.expect("migrations run before use");
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let session = create_session(&*db, &alice, now, Duration::days(30))
                .await
                .expect("session creates");

            let mut expected_games = Vec::new();
            for seed in 1..=3 {
                let challenge = create_challenge(&*db, &alice, &bob, now)
                    .await
                    .expect("challenge creates");
                let game_id = accept_challenge(&*db, &challenge, &bob, now, seed)
                    .await
                    .expect("challenge starts one game");
                let state = recover_game(&*db, game_id).await.expect("new game replays");
                rebuild(&*db, game_id, &state).await;
                store_snapshot(&*db, game_id, &state, 0)
                    .await
                    .expect("snapshot stores");
                expected_games.push((game_id, state));
            }

            let dashboard = dashboard_projection(&*db, &alice)
                .await
                .expect("dashboard projects");
            assert_eq!(dashboard.games.len(), 3);
            (alice, session.expose().to_string(), expected_games)
        };

        let db = open_database(&path).await;
        migrate_app(&*db)
            .await
            .expect("migrations remain idempotent after restart");
        assert_eq!(
            resolve_session(&*db, &session, now)
                .await
                .expect("session survives restart"),
            alice
        );

        for (game_id, expected) in &expected_games {
            let recovered = recover_game(&*db, *game_id)
                .await
                .expect("snapshot and journal recover after restart");
            assert_eq!(&recovered, expected);
            rebuild(&*db, *game_id, &recovered).await;
        }

        let summaries = user_game_summaries(&*db, &alice)
            .await
            .expect("rebuilt summaries load");
        assert_eq!(summaries.len(), 3);
        for (game_id, state) in &expected_games {
            let summary = summaries
                .iter()
                .find(|summary| summary.game_id == game_id.to_string())
                .expect("each stable game ID remains resumable");
            assert_eq!(summary.canonical_revision, state.revision);
            let history = db
                .select("move_history")
                .where_eq("game_id", game_id.to_string())
                .execute(&*db)
                .await
                .expect("rebuilt history loads");
            assert_eq!(
                history.len(),
                usize::try_from(state.revision).expect("revision fits")
            );
        }

        drop(db);
        remove_database_files(&path);
    });
}
