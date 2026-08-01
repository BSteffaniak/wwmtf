use futures_lite::future::block_on;
use hyperchad::{
    shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, PayloadBlob, Revision,
        TransportInbound, TransportOutbound,
    },
    shared_state_transport::{AuthenticatedTransportContext, SharedStateTransportDispatcher as _},
};
use std::{collections::BTreeMap, sync::Arc};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use words_with_spouses_app::{
    GameSharedStateDispatcher, accept_challenge, create_challenge, create_session, game_channel,
    load_authorized_game_page, migrate_app, recover_game, register,
};
use words_with_spouses_game_domain::GameCommand;

fn database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("words-with-spouses-e2e-{}.db", Uuid::new_v4()))
}

async fn open_database(path: &std::path::Path) -> Arc<dyn switchy_database::Database> {
    Arc::from(
        switchy_database_connection::builder()
            .turso()
            .with_path(path)
            .with_busy_timeout(std::time::Duration::from_secs(5))
            .build()
            .await
            .expect("file-backed Turso opens"),
    )
}

fn context(user_id: &str, binding: &str) -> AuthenticatedTransportContext {
    AuthenticatedTransportContext {
        participant_id: ParticipantId::new(user_id),
        identity_binding: binding.to_string(),
    }
}

fn command(
    game_id: words_with_spouses_game_domain::GameId,
    user_id: &str,
    sequence: u64,
    revision: u64,
    command: &GameCommand,
) -> CommandEnvelope {
    let command_name = match command {
        GameCommand::Play { .. } => "PLAY",
        GameCommand::Exchange { .. } => "EXCHANGE",
        GameCommand::Pass => "PASS",
        GameCommand::Resign => "RESIGN",
    };
    CommandEnvelope {
        command_id: CommandId::new(format!("e2e-command-{sequence}")),
        channel_id: game_channel(game_id),
        participant_id: ParticipantId::new(user_id),
        idempotency_key: IdempotencyKey::new(format!("e2e-idempotency-{sequence}")),
        expected_revision: Revision::new(revision),
        command_name: command_name.to_string(),
        payload: PayloadBlob::from_serializable(command).expect("command encodes"),
        metadata: BTreeMap::new(),
        created_at_ms: i64::try_from(sequence).expect("sequence fits"),
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
fn two_authenticated_clients_play_to_completion_with_private_live_views() {
    block_on(async {
        let database: Arc<dyn switchy_database::Database> = Arc::from(
            switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("Turso opens"),
        );
        migrate_app(&*database).await.expect("migrations run");
        let now = OffsetDateTime::UNIX_EPOCH;
        let alice = register(&*database, "alice", "correct horse battery staple", now)
            .await
            .expect("Alice registers");
        let bob = register(&*database, "bob", "another correct horse battery", now)
            .await
            .expect("Bob registers");
        let alice_session = create_session(&*database, &alice, now, Duration::days(30))
            .await
            .expect("Alice session creates")
            .expose()
            .to_string();
        let bob_session = create_session(&*database, &bob, now, Duration::days(30))
            .await
            .expect("Bob session creates")
            .expose()
            .to_string();
        let challenge = create_challenge(&*database, &alice, &bob, now)
            .await
            .expect("challenge creates");
        let game_id = accept_challenge(&*database, &challenge, &bob, now, 41)
            .await
            .expect("game starts");
        let dispatcher = GameSharedStateDispatcher::new(database.clone());
        let alice_context = context(&alice, "alice-browser");
        let bob_context = context(&bob, "bob-browser");
        let alice_events = dispatcher
            .subscribe_channel(&alice_context, &game_channel(game_id))
            .await
            .expect("Alice subscribes");
        let bob_events = dispatcher
            .subscribe_channel(&bob_context, &game_channel(game_id))
            .await
            .expect("Bob subscribes");
        let _ = alice_events.recv_async().await.expect("Alice initial view");
        let _ = bob_events.recv_async().await.expect("Bob initial view");

        let mut sequence = 1;
        loop {
            let state = recover_game(&*database, game_id)
                .await
                .expect("game replays");
            if state.status == words_with_spouses_game_domain::GameStatus::Completed {
                break;
            }
            let actor = if state.active_player == state.players[0] {
                (&alice, &alice_context)
            } else {
                (&bob, &bob_context)
            };
            let result = dispatcher
                .ingest_outbound(
                    actor.1,
                    TransportOutbound::Command(command(
                        game_id,
                        actor.0,
                        sequence,
                        state.revision,
                        &GameCommand::Pass,
                    )),
                )
                .await
                .expect("pass dispatches");
            assert!(matches!(
                result.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ));
            let alice_event = alice_events.recv_async().await.expect("Alice live update");
            let bob_event = bob_events.recv_async().await.expect("Bob live update");
            let alice_view: words_with_spouses_app::GameView = dispatcher
                .project_event(&alice_context, &alice_event)
                .expect("Alice projection")
                .payload
                .deserialize()
                .expect("Alice view decodes");
            let bob_view: words_with_spouses_app::GameView = dispatcher
                .project_event(&bob_context, &bob_event)
                .expect("Bob projection")
                .payload
                .deserialize()
                .expect("Bob view decodes");
            assert_eq!(alice_view.revision, bob_view.revision);
            assert_eq!(alice_view.board, bob_view.board);
            assert_ne!(alice_view.rack, bob_view.rack);
            sequence += 1;
        }

        let completed = recover_game(&*database, game_id)
            .await
            .expect("completed game replays");
        assert_eq!(
            completed.status,
            words_with_spouses_game_domain::GameStatus::Completed
        );
        for (session, expected_user) in [(&alice_session, &alice), (&bob_session, &bob)] {
            let cookies = BTreeMap::from([(
                words_with_spouses_app::SESSION_COOKIE_NAME.to_string(),
                session.clone(),
            )]);
            let page = load_authorized_game_page(&*database, &cookies, &game_id.to_string(), now)
                .await
                .expect("completed game page loads");
            assert!(page.completed);
            assert_eq!(page.user_id, *expected_user);
            assert!(
                page.history
                    .iter()
                    .any(|entry| entry.kind == "GAME_COMPLETED")
            );
            assert_eq!(
                page.view.status,
                words_with_spouses_game_domain::GameStatus::Completed
            );
        }
    });
}

#[test]
fn two_authenticated_clients_reconnect_across_restart_and_inspect_history() {
    block_on(async {
        let path = database_path();
        let now = OffsetDateTime::UNIX_EPOCH;
        let (game_id, alice, bob, alice_session, bob_session, expected_revision) = {
            let database = open_database(&path).await;
            migrate_app(&*database).await.expect("migrations run");
            let alice = register(&*database, "alice", "correct horse battery staple", now)
                .await
                .expect("Alice registers");
            let bob = register(&*database, "bob", "another correct horse battery", now)
                .await
                .expect("Bob registers");
            let alice_session = create_session(&*database, &alice, now, Duration::days(30))
                .await
                .expect("Alice session creates")
                .expose()
                .to_string();
            let bob_session = create_session(&*database, &bob, now, Duration::days(30))
                .await
                .expect("Bob session creates")
                .expose()
                .to_string();
            let challenge = create_challenge(&*database, &alice, &bob, now)
                .await
                .expect("challenge creates");
            let game_id = accept_challenge(&*database, &challenge, &bob, now, 23)
                .await
                .expect("game starts");
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let alice_context = context(&alice, "alice-tab-1");
            let bob_context = context(&bob, "bob-tab-1");
            let alice_events = dispatcher
                .subscribe_channel(&alice_context, &game_channel(game_id))
                .await
                .expect("Alice subscribes");
            let bob_events = dispatcher
                .subscribe_channel(&bob_context, &game_channel(game_id))
                .await
                .expect("Bob subscribes");
            let _ = alice_events.recv_async().await.expect("Alice initial view");
            let _ = bob_events.recv_async().await.expect("Bob initial view");
            let state = recover_game(&*database, game_id).await.expect("game loads");
            let command = command(game_id, &alice, 1, state.revision, &GameCommand::Pass);
            let result = dispatcher
                .ingest_outbound(&alice_context, TransportOutbound::Command(command))
                .await
                .expect("command dispatches");
            assert!(matches!(
                result.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ));
            let alice_update = alice_events.recv_async().await.expect("Alice update");
            let bob_update = bob_events.recv_async().await.expect("Bob update");
            assert!(
                dispatcher
                    .project_event(&alice_context, &alice_update)
                    .is_some()
            );
            assert!(
                dispatcher
                    .project_event(&bob_context, &bob_update)
                    .is_some()
            );
            (
                game_id,
                alice,
                bob,
                alice_session,
                bob_session,
                state.revision + 1,
            )
        };

        let database = open_database(&path).await;
        migrate_app(&*database)
            .await
            .expect("migrations remain idempotent after restart");
        let dispatcher = GameSharedStateDispatcher::new(database.clone());
        for (user, binding) in [
            (&alice, "alice-tab-2"),
            (&alice, "alice-tab-3"),
            (&bob, "bob-tab-2"),
        ] {
            let receiver = dispatcher
                .subscribe_channel(&context(user, binding), &game_channel(game_id))
                .await
                .expect("reconnected tab subscribes");
            let event = receiver.recv_async().await.expect("rehydrated update");
            let projected = dispatcher
                .project_event(&context(user, binding), &event)
                .expect("private update projects");
            let view: words_with_spouses_app::GameView =
                projected.payload.deserialize().expect("view decodes");
            assert_eq!(view.revision, expected_revision);
            assert_eq!(view.rack.len(), 7);
        }

        for session in [&alice_session, &bob_session] {
            let cookies = BTreeMap::from([(
                words_with_spouses_app::SESSION_COOKIE_NAME.to_string(),
                session.clone(),
            )]);
            let page = load_authorized_game_page(&*database, &cookies, &game_id.to_string(), now)
                .await
                .expect("history loads after restart");
            assert_eq!(page.view.revision, expected_revision);
            assert_eq!(
                page.history.len(),
                usize::try_from(expected_revision).expect("revision fits")
            );
        }

        drop(database);
        remove_database_files(&path);
    });
}
