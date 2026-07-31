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

fn remove_database_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-shm", "-wal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
    }
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
            let command = CommandEnvelope {
                command_id: CommandId::new("e2e-pass"),
                channel_id: game_channel(game_id),
                participant_id: ParticipantId::new(&alice),
                idempotency_key: IdempotencyKey::new("e2e-pass-idempotency"),
                expected_revision: Revision::new(state.revision),
                command_name: "PASS".to_string(),
                payload: PayloadBlob::from_serializable(&GameCommand::Pass)
                    .expect("command encodes"),
                metadata: BTreeMap::new(),
                created_at_ms: 1,
            };
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
