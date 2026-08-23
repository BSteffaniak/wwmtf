use futures_lite::future::block_on;
use hyperchad::{
    shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, PayloadBlob, Revision,
        TransportInbound, TransportOutbound,
    },
    shared_state_transport::{AuthenticatedTransportContext, SharedStateTransportDispatcher as _},
};
use std::{collections::BTreeMap, sync::Arc};
use switchy_database::query::FilterableQuery as _;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use wwmtf_app::{
    FirstPlayerPolicy, GameCreationPolicy, GameSharedStateDispatcher, LobbySettings,
    accept_challenge, create_challenge, create_lobby, create_session, dashboard_channel,
    dashboard_projection, game_channel, join_lobby, load_events, load_lobby, migrate_app,
    rebuild_game_projections, recover_game, register, resolve_session, start_lobby, store_snapshot,
    submit_game_command, user_game_summaries,
};
use wwmtf_game_domain::{GameCommand, GameId, GameState};

fn database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wwmtf-restart-{}.db", Uuid::new_v4()))
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

async fn open_database_arc(path: &std::path::Path) -> Arc<dyn switchy_database::Database> {
    Arc::from(open_database(path).await)
}

fn context(user_id: &str, binding: &str) -> AuthenticatedTransportContext {
    AuthenticatedTransportContext {
        participant_id: ParticipantId::new(user_id),
        identity_binding: binding.to_string(),
    }
}

fn pass_command(
    game_id: GameId,
    user_id: &str,
    revision: u64,
    command_id: &str,
) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::new(command_id),
        channel_id: game_channel(game_id),
        participant_id: ParticipantId::new(user_id),
        idempotency_key: IdempotencyKey::new(format!("{command_id}-idempotency")),
        expected_revision: Revision::new(revision),
        command_name: "PASS".to_string(),
        payload: PayloadBlob::from_serializable(&GameCommand::Pass).expect("pass encodes"),
        metadata: BTreeMap::new(),
        created_at_ms: 1,
    }
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

fn copy_database_files(source: &std::path::Path, restored: &std::path::Path) {
    std::fs::copy(source, restored).expect("main database copies");
    for suffix in ["-shm", "-wal"] {
        let mut source_sidecar = source.as_os_str().to_owned();
        source_sidecar.push(suffix);
        let source_sidecar = std::path::PathBuf::from(source_sidecar);
        if source_sidecar.exists() {
            let mut restored_sidecar = restored.as_os_str().to_owned();
            restored_sidecar.push(suffix);
            std::fs::copy(source_sidecar, std::path::PathBuf::from(restored_sidecar))
                .expect("database sidecar copies");
        }
    }
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
        let event = wwmtf_game_domain::GameEvent::TurnPassed {
            player_id: state.active_player,
        };
        let first = wwmtf_app::append_events_transactionally(
            &*first_db,
            game_id,
            "same-revision-a",
            "same-revision-idem-a",
            state.revision,
            std::slice::from_ref(&event),
        );
        let second = wwmtf_app::append_events_transactionally(
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

#[test]
fn active_lobby_and_multiplayer_lifecycle_survive_file_backed_restart() {
    block_on(async {
        let path = database_path();
        let now = OffsetDateTime::UNIX_EPOCH;
        let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
        let (lobby_id, game_id, users, active_expected, resigned_expected, completed_expected) = {
            let db = open_database(&path).await;
            migrate_app(&*db).await.expect("migrations run");
            let mut users = Vec::new();
            for (username, password) in [
                ("alice", "correct horse battery staple"),
                ("bob", "another correct horse battery"),
                ("carol", "third correct horse battery"),
            ] {
                users.push(
                    register(&*db, username, password, now)
                        .await
                        .expect("register"),
                );
            }
            let settings = LobbySettings {
                max_players: 6,
                board_size: 21,
                tile_set_count: 2,
                first_player: FirstPlayerPolicy::Creator,
            };
            let (waiting_lobby_id, waiting_token) = create_lobby(
                &*db,
                &users[0],
                settings.clone(),
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("waiting lobby creates");
            join_lobby(&*db, waiting_token.expose(), &users[1], policy, now)
                .await
                .expect("waiting member joins");

            let (started_lobby_id, token) =
                create_lobby(&*db, &users[0], settings, policy, now, Duration::days(1))
                    .await
                    .expect("started lobby creates");
            for user in &users[1..] {
                join_lobby(&*db, token.expose(), user, policy, now)
                    .await
                    .expect("member joins");
            }
            let game_id = start_lobby(&*db, &started_lobby_id, &users[0], policy, now, 17)
                .await
                .expect("starts");
            let active_expected = recover_game(&*db, game_id).await.expect("active recovers");
            let resigned_expected = submit_game_command(
                &*db,
                game_id,
                &users[0],
                "restart-resign",
                "restart-resign-idem",
                active_expected.revision,
                &GameCommand::Resign,
                1,
            )
            .await
            .expect("non-current member resigns");
            let mut completed_expected = resigned_expected.clone();
            while completed_expected.status == wwmtf_game_domain::GameStatus::Active {
                let active_index = completed_expected
                    .players
                    .iter()
                    .position(|player| *player == completed_expected.active_player)
                    .expect("active player seated");
                completed_expected = submit_game_command(
                    &*db,
                    game_id,
                    &users[active_index],
                    &format!("restart-pass-{}", completed_expected.revision),
                    &format!("restart-pass-idem-{}", completed_expected.revision),
                    completed_expected.revision,
                    &GameCommand::Pass,
                    i64::try_from(completed_expected.revision).expect("revision fits"),
                )
                .await
                .expect("pass completes");
            }
            drop(db);
            (
                waiting_lobby_id,
                game_id,
                users,
                active_expected,
                resigned_expected,
                completed_expected,
            )
        };

        let db = open_database(&path).await;
        migrate_app(&*db).await.expect("restart migrations run");
        let lobby = load_lobby(&*db, &lobby_id, &users[0])
            .await
            .expect("active lobby survives");
        assert_eq!(lobby.status, "OPEN");
        assert_eq!(lobby.members.len(), 2);
        assert_eq!(active_expected.players.len(), 3);
        assert!(
            !resigned_expected
                .active_players
                .contains(&resigned_expected.players[0])
        );
        assert_eq!(
            recover_game(&*db, game_id)
                .await
                .expect("completed game survives"),
            completed_expected
        );
        drop(db);
        remove_database_files(&path);
    });
}

#[test]
fn restored_database_preserves_sessions_history_and_live_reconnect() {
    block_on(async {
        let source = database_path();
        let restored = database_path();
        let now = OffsetDateTime::UNIX_EPOCH;
        let (game_id, alice, bob, alice_session, expected_revision) = {
            let db = open_database_arc(&source).await;
            migrate_app(&*db).await.expect("migrations run");
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let alice_session = create_session(&*db, &alice, now, Duration::days(30))
                .await
                .expect("session creates")
                .expose()
                .to_string();
            let challenge = create_challenge(&*db, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = accept_challenge(&*db, &challenge, &bob, now, 73)
                .await
                .expect("game starts");
            let dispatcher = GameSharedStateDispatcher::new(db.clone());
            let state = recover_game(&*db, game_id).await.expect("game loads");
            let response = dispatcher
                .ingest_outbound(
                    &context(&alice, "source-alice"),
                    TransportOutbound::Command(pass_command(
                        game_id,
                        &alice,
                        state.revision,
                        "restore-drill-before-backup",
                    )),
                )
                .await
                .expect("turn dispatches");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ));
            drop(dispatcher);
            drop(db);
            (game_id, alice, bob, alice_session, state.revision + 1)
        };

        copy_database_files(&source, &restored);
        let db = open_database_arc(&restored).await;
        migrate_app(&*db)
            .await
            .expect("restored migrations remain idempotent");
        assert_eq!(
            resolve_session(&*db, &alice_session, now)
                .await
                .expect("intended session survives restore"),
            alice
        );
        let restored_state = recover_game(&*db, game_id)
            .await
            .expect("active game survives restore");
        assert_eq!(restored_state.revision, expected_revision);
        assert_eq!(
            load_events(&*db, game_id, 0)
                .await
                .expect("history survives restore")
                .len(),
            usize::try_from(expected_revision).expect("revision fits")
        );

        let dispatcher = GameSharedStateDispatcher::new(db.clone());
        let alice_context = context(&alice, "restored-alice");
        let bob_context = context(&bob, "restored-bob");
        let alice_game = dispatcher
            .subscribe_channel(&alice_context, &game_channel(game_id))
            .await
            .expect("Alice live game reconnects");
        let bob_game = dispatcher
            .subscribe_channel(&bob_context, &game_channel(game_id))
            .await
            .expect("Bob live game reconnects");
        let alice_dashboard = dispatcher
            .subscribe_channel(&alice_context, &dashboard_channel(&alice))
            .await
            .expect("Alice dashboard reconnects");
        for receiver in [&alice_game, &bob_game, &alice_dashboard] {
            let event = receiver
                .recv_async()
                .await
                .expect("restored state rehydrates");
            assert_eq!(event.revision.value(), expected_revision);
        }

        let active_user = if restored_state.active_player == restored_state.players[0] {
            &alice
        } else {
            &bob
        };
        let active_context = if active_user == &alice {
            &alice_context
        } else {
            &bob_context
        };
        let response = dispatcher
            .ingest_outbound(
                active_context,
                TransportOutbound::Command(pass_command(
                    game_id,
                    active_user,
                    expected_revision,
                    "restore-drill-after-restore",
                )),
            )
            .await
            .expect("normal turn works after restore");
        assert!(
            matches!(
                response.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ),
            "{response:?}"
        );
        assert_eq!(
            recover_game(&*db, game_id)
                .await
                .expect("post-restore turn persists")
                .revision,
            expected_revision + 1
        );

        drop(dispatcher);
        drop(db);
        remove_database_files(&source);
        remove_database_files(&restored);
    });
}
