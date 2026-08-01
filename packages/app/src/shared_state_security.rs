use std::{
    collections::BTreeMap,
    str::FromStr as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use hyperchad::{
    shared_state_models::{
        ChannelId, EventEnvelope, EventId, PayloadBlob, Revision, SnapshotEnvelope,
        TransportInbound, TransportOutbound,
    },
    shared_state_transport::{
        AuthenticatedTransportContext, SharedStateTransportDispatchResult,
        SharedStateTransportDispatcher,
    },
};
use serde::{Deserialize, Serialize};
use switchy_database::{Database, query::FilterableQuery as _};
use words_with_spouses_game_domain::{GameCommand, GameId, PlayerId};

use crate::{
    DashboardProjection, GameView, UserScoreTotals, dashboard_projection, game_view,
    player_for_user, recover_game, submit_game_command, user_score_totals,
};

const GAME_CHANNEL_PREFIX: &str = "game:";
const DASHBOARD_CHANNEL_PREFIX: &str = "dashboard:";
const GAME_VIEW_EVENT: &str = "GAME_VIEW_UPDATED_V1";
const DASHBOARD_EVENT: &str = "DASHBOARD_UPDATED_V1";
const PRIVATE_PARTICIPANT_METADATA: &str = "private-participant-id";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardLiveView {
    pub projection: DashboardProjection,
    pub score_totals: Option<UserScoreTotals>,
}

#[derive(Debug, Clone)]
struct ServerGameUpdate {
    views_by_user: BTreeMap<String, GameView>,
    dashboards_by_user: BTreeMap<String, DashboardLiveView>,
}

#[derive(Debug)]
struct GameSubscriber {
    user_id: String,
    sender: flume::Sender<EventEnvelope>,
}

/// Membership-authorized renderer-neutral gameplay dispatcher.
///
/// Canonical state remains server-internal. Before an update enters the in-process subscriber bus,
/// it is reduced to the exact authorized [`GameView`] for each subscriber. Every queued event is
/// tagged to one participant, and [`Self::project_event`] verifies that tag before transport.
#[derive(Debug)]
pub struct GameSharedStateDispatcher {
    database: Arc<dyn Database>,
    subscribers: Mutex<BTreeMap<ChannelId, Vec<GameSubscriber>>>,
}

impl GameSharedStateDispatcher {
    /// Creates a dispatcher backed by the authoritative application database.
    #[must_use]
    pub fn new(database: Arc<dyn Database>) -> Self {
        Self {
            database,
            subscribers: Mutex::new(BTreeMap::new()),
        }
    }

    async fn dashboard_view(
        &self,
        user_id: &str,
    ) -> SharedStateTransportDispatchResult<DashboardLiveView> {
        Ok(DashboardLiveView {
            projection: dashboard_projection(&*self.database, user_id).await?,
            score_totals: user_score_totals(&*self.database, user_id).await?,
        })
    }

    async fn authorize(
        &self,
        context: &AuthenticatedTransportContext,
        channel_id: &ChannelId,
    ) -> SharedStateTransportDispatchResult<(GameId, PlayerId)> {
        let game_id = game_id(channel_id)?;
        let player = player_for_user(&*self.database, game_id, context.participant_id.as_str())
            .await
            .map_err(|_| "game channel is unknown or unauthorized")?;
        Ok((game_id, player))
    }

    async fn server_update(
        &self,
        game_id: GameId,
        state: &words_with_spouses_game_domain::GameState,
    ) -> SharedStateTransportDispatchResult<ServerGameUpdate> {
        let rows = self
            .database
            .select("game_players")
            .where_eq("game_id", game_id.to_string())
            .execute(&*self.database)
            .await?;
        let mut views_by_user = BTreeMap::new();
        for row in rows {
            let user_id = row
                .get("user_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or("game membership user is malformed")?;
            let player_id = row
                .get("game_player_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or("game membership player is malformed")?;
            let player = PlayerId::from_str(&player_id)
                .map_err(|_| "game membership player is malformed")?;
            let view = game_view(state, player).ok_or("game membership player is not seated")?;
            views_by_user.insert(user_id, view);
        }
        if views_by_user.len() != 2 {
            return Err("game must have exactly two members".into());
        }
        let mut dashboards_by_user = BTreeMap::new();
        for user_id in views_by_user.keys() {
            dashboards_by_user.insert(
                user_id.clone(),
                DashboardLiveView {
                    projection: dashboard_projection(&*self.database, user_id).await?,
                    score_totals: user_score_totals(&*self.database, user_id).await?,
                },
            );
        }
        Ok(ServerGameUpdate {
            views_by_user,
            dashboards_by_user,
        })
    }

    fn projected_event(
        game_id: GameId,
        view: &GameView,
        participant_id: &str,
        command_id: Option<&hyperchad::shared_state_models::CommandId>,
        created_at_ms: i64,
    ) -> SharedStateTransportDispatchResult<EventEnvelope> {
        let revision = Revision::new(view.revision);
        Ok(EventEnvelope {
            event_id: EventId::new(format!("{game_id}:{}:{participant_id}", revision.value())),
            channel_id: game_channel(game_id),
            revision,
            command_id: command_id.cloned(),
            event_name: GAME_VIEW_EVENT.to_string(),
            payload: PayloadBlob::from_serializable(view)?,
            metadata: BTreeMap::from([(
                PRIVATE_PARTICIPANT_METADATA.to_string(),
                participant_id.to_string(),
            )]),
            created_at_ms,
        })
    }

    fn dashboard_event(
        user_id: &str,
        view: &DashboardLiveView,
        revision: Revision,
        command_id: Option<&hyperchad::shared_state_models::CommandId>,
        created_at_ms: i64,
    ) -> SharedStateTransportDispatchResult<EventEnvelope> {
        Ok(EventEnvelope {
            event_id: EventId::new(format!("dashboard:{user_id}:{}", revision.value())),
            channel_id: dashboard_channel(user_id),
            revision,
            command_id: command_id.cloned(),
            event_name: DASHBOARD_EVENT.to_string(),
            payload: PayloadBlob::from_serializable(view)?,
            metadata: BTreeMap::from([(
                PRIVATE_PARTICIPANT_METADATA.to_string(),
                user_id.to_string(),
            )]),
            created_at_ms,
        })
    }

    fn publish(
        &self,
        game_id: GameId,
        update: &ServerGameUpdate,
        command_id: Option<&hyperchad::shared_state_models::CommandId>,
        created_at_ms: i64,
    ) -> SharedStateTransportDispatchResult<()> {
        let channel_id = game_channel(game_id);
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| "game subscriber registry is unavailable")?;
        if let Some(channel_subscribers) = subscribers.get_mut(&channel_id) {
            channel_subscribers.retain(|subscriber| {
                let Some(view) = update.views_by_user.get(&subscriber.user_id) else {
                    return false;
                };
                Self::projected_event(
                    game_id,
                    view,
                    &subscriber.user_id,
                    command_id,
                    created_at_ms,
                )
                .is_ok_and(|event| subscriber.sender.send(event).is_ok())
            });
        }
        let revision = update
            .views_by_user
            .values()
            .next()
            .map_or(Revision::new(0), |view| Revision::new(view.revision));
        for (user_id, view) in &update.dashboards_by_user {
            if let Some(channel_subscribers) = subscribers.get_mut(&dashboard_channel(user_id)) {
                channel_subscribers.retain(|subscriber| {
                    subscriber.user_id == *user_id
                        && Self::dashboard_event(user_id, view, revision, command_id, created_at_ms)
                            .is_ok_and(|event| subscriber.sender.send(event).is_ok())
                });
            }
        }
        let live_subscribers = subscribers.values().map(Vec::len).sum();
        crate::observability::set_live_subscribers(live_subscribers);
        drop(subscribers);
        Ok(())
    }
}

#[async_trait]
impl SharedStateTransportDispatcher for GameSharedStateDispatcher {
    async fn ingest_outbound(
        &self,
        context: &AuthenticatedTransportContext,
        outbound: TransportOutbound,
    ) -> SharedStateTransportDispatchResult<Vec<TransportInbound>> {
        match outbound {
            TransportOutbound::Command(command) => {
                if command.participant_id != context.participant_id {
                    return Ok(vec![TransportInbound::CommandRejected {
                        command_id: command.command_id,
                        reason: "command participant does not match authenticated user".to_string(),
                    }]);
                }
                let (game_id, _) = self.authorize(context, &command.channel_id).await?;
                let game_command: GameCommand = match command.payload.deserialize() {
                    Ok(game_command) => game_command,
                    Err(error) => {
                        return Ok(vec![TransportInbound::CommandRejected {
                            command_id: command.command_id,
                            reason: format!("invalid gameplay command payload: {error}"),
                        }]);
                    }
                };
                if !command_name_matches(&command.command_name, &game_command) {
                    return Ok(vec![TransportInbound::CommandRejected {
                        command_id: command.command_id,
                        reason: "gameplay command name does not match payload".to_string(),
                    }]);
                }
                let state = match submit_game_command(
                    &*self.database,
                    game_id,
                    context.participant_id.as_str(),
                    command.command_id.as_str(),
                    command.idempotency_key.as_str(),
                    command.expected_revision.value(),
                    &game_command,
                    command.created_at_ms,
                )
                .await
                {
                    Ok(state) => state,
                    Err(error) => {
                        return Ok(vec![TransportInbound::CommandRejected {
                            command_id: command.command_id,
                            reason: error.to_string(),
                        }]);
                    }
                };
                let resulting_revision = Revision::new(state.revision);
                let update = self.server_update(game_id, &state).await?;
                self.publish(
                    game_id,
                    &update,
                    Some(&command.command_id),
                    command.created_at_ms,
                )?;
                Ok(vec![TransportInbound::CommandAccepted {
                    command_id: command.command_id,
                    resulting_revision,
                }])
            }
            TransportOutbound::Subscribe(subscribe) => {
                if dashboard_user(&subscribe.channel_id) == Some(context.participant_id.as_str()) {
                    let view = self.dashboard_view(context.participant_id.as_str()).await?;
                    let revision = view
                        .projection
                        .games
                        .iter()
                        .map(|game| game.canonical_revision)
                        .max()
                        .unwrap_or(0);
                    return Ok(vec![TransportInbound::Snapshot(SnapshotEnvelope {
                        channel_id: subscribe.channel_id,
                        revision: Revision::new(revision),
                        payload: PayloadBlob::from_serializable(&view)?,
                        created_at_ms: 0,
                    })]);
                }
                let (game_id, player_id) = self.authorize(context, &subscribe.channel_id).await?;
                let state = recover_game(&*self.database, game_id).await?;
                let view =
                    game_view(&state, player_id).ok_or("authorized game view is unavailable")?;
                Ok(vec![TransportInbound::Snapshot(SnapshotEnvelope {
                    channel_id: subscribe.channel_id,
                    revision: Revision::new(state.revision),
                    payload: PayloadBlob::from_serializable(&view)?,
                    created_at_ms: 0,
                })])
            }
            TransportOutbound::Unsubscribe(unsubscribe) => {
                if dashboard_user(&unsubscribe.channel_id) == Some(context.participant_id.as_str())
                {
                    return Ok(Vec::new());
                }
                self.authorize(context, &unsubscribe.channel_id).await?;
                Ok(Vec::new())
            }
            TransportOutbound::Ping(ping) => Ok(vec![TransportInbound::Pong(ping)]),
        }
    }

    async fn subscribe_channel(
        &self,
        context: &AuthenticatedTransportContext,
        channel_id: &ChannelId,
    ) -> SharedStateTransportDispatchResult<flume::Receiver<EventEnvelope>> {
        if dashboard_user(channel_id) == Some(context.participant_id.as_str()) {
            let (sender, receiver) = flume::unbounded();
            self.subscribers
                .lock()
                .map_err(|_| "dashboard subscriber registry is unavailable")?
                .entry(channel_id.clone())
                .or_default()
                .push(GameSubscriber {
                    user_id: context.participant_id.as_str().to_string(),
                    sender: sender.clone(),
                });
            let view = self.dashboard_view(context.participant_id.as_str()).await?;
            let revision = view
                .projection
                .games
                .iter()
                .map(|game| game.canonical_revision)
                .max()
                .unwrap_or(0);
            sender.send(Self::dashboard_event(
                context.participant_id.as_str(),
                &view,
                Revision::new(revision),
                None,
                0,
            )?)?;
            return Ok(receiver);
        }
        let (game_id, player) = self.authorize(context, channel_id).await?;
        let (sender, receiver) = flume::unbounded();
        self.subscribers
            .lock()
            .map_err(|_| "game subscriber registry is unavailable")?
            .entry(channel_id.clone())
            .or_default()
            .push(GameSubscriber {
                user_id: context.participant_id.as_str().to_string(),
                sender: sender.clone(),
            });
        let live_subscribers = self
            .subscribers
            .lock()
            .map_err(|_| "game subscriber registry is unavailable")?
            .values()
            .map(Vec::len)
            .sum();
        crate::observability::set_live_subscribers(live_subscribers);

        // Register before loading so a command racing subscription is duplicated at worst, never
        // lost. Revision-aware clients converge on the newest update.
        let state = recover_game(&*self.database, game_id).await?;
        let view = game_view(&state, player).ok_or("authorized game view is unavailable")?;
        sender.send(Self::projected_event(
            game_id,
            &view,
            context.participant_id.as_str(),
            None,
            0,
        )?)?;
        Ok(receiver)
    }

    fn project_event(
        &self,
        context: &AuthenticatedTransportContext,
        event: &EventEnvelope,
    ) -> Option<EventEnvelope> {
        if event
            .metadata
            .get(PRIVATE_PARTICIPANT_METADATA)
            .map(String::as_str)
            != Some(context.participant_id.as_str())
        {
            return None;
        }
        if event.event_name == DASHBOARD_EVENT {
            let view: DashboardLiveView = event.payload.deserialize().ok()?;
            return Some(EventEnvelope {
                event_id: event.event_id.clone(),
                channel_id: event.channel_id.clone(),
                revision: event.revision,
                command_id: event.command_id.clone(),
                event_name: DASHBOARD_EVENT.to_string(),
                payload: PayloadBlob::from_serializable(&view).ok()?,
                metadata: BTreeMap::new(),
                created_at_ms: event.created_at_ms,
            });
        }
        if event.event_name != GAME_VIEW_EVENT {
            return None;
        }
        let view: GameView = event.payload.deserialize().ok()?;
        Some(EventEnvelope {
            event_id: event.event_id.clone(),
            channel_id: event.channel_id.clone(),
            revision: event.revision,
            command_id: event.command_id.clone(),
            event_name: GAME_VIEW_EVENT.to_string(),
            payload: PayloadBlob::from_serializable(&view).ok()?,
            metadata: BTreeMap::new(),
            created_at_ms: event.created_at_ms,
        })
    }
}

fn command_name_matches(name: &str, command: &GameCommand) -> bool {
    matches!(
        (name, command),
        ("PLAY", GameCommand::Play { .. })
            | ("EXCHANGE", GameCommand::Exchange { .. })
            | ("PASS", GameCommand::Pass)
            | ("RESIGN", GameCommand::Resign)
    )
}

fn dashboard_user(channel_id: &ChannelId) -> Option<&str> {
    channel_id.as_str().strip_prefix(DASHBOARD_CHANNEL_PREFIX)
}

/// Returns the private shared-state dashboard channel for one authenticated user.
#[must_use]
pub fn dashboard_channel(user_id: &str) -> ChannelId {
    ChannelId::new(format!("{DASHBOARD_CHANNEL_PREFIX}{user_id}"))
}

fn game_id(channel_id: &ChannelId) -> SharedStateTransportDispatchResult<GameId> {
    channel_id
        .as_str()
        .strip_prefix(GAME_CHANNEL_PREFIX)
        .ok_or_else(|| "shared-state channel is not a game channel".into())
        .and_then(|value| GameId::from_str(value).map_err(Into::into))
}

/// Returns the stable shared-state channel for one game.
#[must_use]
pub fn game_channel(game_id: GameId) -> ChannelId {
    ChannelId::new(format!("{GAME_CHANNEL_PREFIX}{game_id}"))
}

/// Builds the membership-aware application shared-state dispatcher.
#[must_use]
pub fn shared_state_dispatcher(
    database: Arc<dyn Database>,
) -> Arc<dyn SharedStateTransportDispatcher> {
    Arc::new(GameSharedStateDispatcher::new(database))
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use hyperchad::shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, TransportSubscribe,
        TransportUnsubscribe,
    };
    use time::OffsetDateTime;

    use super::*;
    use crate::{accept_challenge, create_challenge, migrate_app, register};

    async fn fixture() -> (Arc<dyn Database>, GameId, String, String, String) {
        let database: Arc<dyn Database> = Arc::from(
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
        let mallory = register(&*database, "mallory", "a third correct password", now)
            .await
            .expect("Mallory registers");
        let challenge = create_challenge(&*database, &alice, &bob, now)
            .await
            .expect("challenge creates");
        let game_id = accept_challenge(&*database, &challenge, &bob, now, 3)
            .await
            .expect("game starts");
        (database, game_id, alice, bob, mallory)
    }

    fn context(user_id: &str) -> AuthenticatedTransportContext {
        AuthenticatedTransportContext {
            participant_id: ParticipantId::new(user_id),
            identity_binding: format!("session:{user_id}"),
        }
    }

    fn gameplay_command(
        game_id: GameId,
        participant: &str,
        command_id: &str,
        revision: u64,
        command: &GameCommand,
    ) -> CommandEnvelope {
        let name = match command {
            GameCommand::Play { .. } => "PLAY",
            GameCommand::Exchange { .. } => "EXCHANGE",
            GameCommand::Pass => "PASS",
            GameCommand::Resign => "RESIGN",
        };
        CommandEnvelope {
            command_id: CommandId::new(command_id),
            channel_id: game_channel(game_id),
            participant_id: ParticipantId::new(participant),
            idempotency_key: IdempotencyKey::new(format!("{command_id}-idempotency")),
            expected_revision: Revision::new(revision),
            command_name: name.to_string(),
            payload: PayloadBlob::from_serializable(command).expect("command encodes"),
            metadata: BTreeMap::new(),
            created_at_ms: 1,
        }
    }

    #[test]
    fn dashboard_subscribers_receive_private_turn_updates_and_rehydrate() {
        block_on(async {
            let (database, game_id, alice, bob, mallory) = fixture().await;
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let alice_context = context(&alice);
            let bob_context = context(&bob);
            let alice_dashboard = dispatcher
                .subscribe_channel(&alice_context, &dashboard_channel(&alice))
                .await
                .expect("Alice dashboard subscribes");
            let bob_dashboard = dispatcher
                .subscribe_channel(&bob_context, &dashboard_channel(&bob))
                .await
                .expect("Bob dashboard subscribes");
            assert!(
                dispatcher
                    .subscribe_channel(&context(&mallory), &dashboard_channel(&alice))
                    .await
                    .is_err()
            );
            let alice_initial = alice_dashboard.recv_async().await.expect("Alice dashboard");
            let bob_initial = bob_dashboard.recv_async().await.expect("Bob dashboard");
            let alice_view: DashboardLiveView = dispatcher
                .project_event(&alice_context, &alice_initial)
                .expect("Alice dashboard projects")
                .payload
                .deserialize()
                .expect("dashboard decodes");
            assert_eq!(alice_view.projection.games[0].canonical_revision, 1);
            assert!(
                dispatcher
                    .project_event(&bob_context, &alice_initial)
                    .is_none()
            );
            assert!(
                dispatcher
                    .project_event(&bob_context, &bob_initial)
                    .is_some()
            );

            let state = recover_game(&*database, game_id)
                .await
                .expect("state loads");
            let response = dispatcher
                .ingest_outbound(
                    &alice_context,
                    TransportOutbound::Command(gameplay_command(
                        game_id,
                        &alice,
                        "dashboard-pass",
                        state.revision,
                        &GameCommand::Pass,
                    )),
                )
                .await
                .expect("turn dispatches");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ));
            for (context, receiver) in [
                (&alice_context, &alice_dashboard),
                (&bob_context, &bob_dashboard),
            ] {
                let event = receiver.recv_async().await.expect("dashboard updates");
                let view: DashboardLiveView = dispatcher
                    .project_event(context, &event)
                    .expect("private dashboard projects")
                    .payload
                    .deserialize()
                    .expect("dashboard decodes");
                assert_eq!(
                    view.projection.games[0].canonical_revision,
                    state.revision + 1
                );
                assert_eq!(
                    view.projection.games[0].active_player_user_id.as_deref(),
                    Some(bob.as_str())
                );
            }

            let reconnected = dispatcher
                .subscribe_channel(&alice_context, &dashboard_channel(&alice))
                .await
                .expect("dashboard reconnects");
            let event = reconnected
                .recv_async()
                .await
                .expect("dashboard rehydrates");
            let view: DashboardLiveView = dispatcher
                .project_event(&alice_context, &event)
                .expect("rehydrated dashboard projects")
                .payload
                .deserialize()
                .expect("dashboard decodes");
            assert_eq!(
                view.projection.games[0].canonical_revision,
                state.revision + 1
            );
        });
    }

    #[test]
    fn rejects_forged_identity_channel_state_and_out_of_turn_commands() {
        block_on(async {
            let (database, game_id, alice, bob, mallory) = fixture().await;
            let dispatcher = GameSharedStateDispatcher::new(database.clone());
            let state = recover_game(&*database, game_id)
                .await
                .expect("state loads");

            let forged_identity = gameplay_command(
                game_id,
                &bob,
                "forged-identity",
                state.revision,
                &GameCommand::Pass,
            );
            let response = dispatcher
                .ingest_outbound(
                    &context(&alice),
                    TransportOutbound::Command(forged_identity),
                )
                .await
                .expect("forged identity is rejected normally");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandRejected { .. }]
            ));

            let guessed_channel = ChannelId::new(format!("game:{}", uuid::Uuid::new_v4()));
            assert!(
                dispatcher
                    .ingest_outbound(
                        &context(&mallory),
                        TransportOutbound::Subscribe(TransportSubscribe {
                            channel_id: guessed_channel.clone(),
                            last_seen_revision: None,
                        }),
                    )
                    .await
                    .is_err()
            );
            assert!(
                dispatcher
                    .ingest_outbound(
                        &context(&mallory),
                        TransportOutbound::Unsubscribe(TransportUnsubscribe {
                            channel_id: guessed_channel,
                        }),
                    )
                    .await
                    .is_err()
            );

            let out_of_turn = gameplay_command(
                game_id,
                &bob,
                "out-of-turn",
                state.revision,
                &GameCommand::Pass,
            );
            let response = dispatcher
                .ingest_outbound(&context(&bob), TransportOutbound::Command(out_of_turn))
                .await
                .expect("out-of-turn command rejects");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandRejected { reason, .. }]
                    if reason.contains("turn")
            ));

            let mut command = gameplay_command(
                game_id,
                &alice,
                "forged-name",
                state.revision,
                &GameCommand::Pass,
            );
            command.command_name = "PLAY".to_string();
            command.payload = PayloadBlob::from_serializable(&serde_json::json!({
                "Play": {
                    "placements": [{
                        "tile_id": 65535,
                        "coordinate": { "x": 7, "y": 7 },
                        "blank_letter": null
                    }],
                    "scores": { "forged": 999_999 },
                    "rack": [65535]
                }
            }))
            .expect("forged payload encodes");
            let response = dispatcher
                .ingest_outbound(&context(&alice), TransportOutbound::Command(command))
                .await
                .expect("mismatched command rejects");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandRejected { .. }]
            ));
            assert_eq!(
                recover_game(&*database, game_id)
                    .await
                    .expect("rejected commands do not mutate state")
                    .revision,
                state.revision
            );
        });
    }

    #[test]
    fn membership_controls_replay_and_projects_distinct_private_racks() {
        block_on(async {
            let (database, game_id, alice, bob, mallory) = fixture().await;
            let dispatcher = GameSharedStateDispatcher::new(database);
            let alice_context = context(&alice);
            let alice_inbound = dispatcher
                .ingest_outbound(
                    &alice_context,
                    TransportOutbound::Subscribe(TransportSubscribe {
                        channel_id: game_channel(game_id),
                        last_seen_revision: None,
                    }),
                )
                .await
                .expect("Alice subscribes");
            let bob_context = context(&bob);
            let bob_inbound = dispatcher
                .ingest_outbound(
                    &bob_context,
                    TransportOutbound::Subscribe(TransportSubscribe {
                        channel_id: game_channel(game_id),
                        last_seen_revision: None,
                    }),
                )
                .await
                .expect("Bob subscribes");
            let mallory_context = context(&mallory);
            assert!(
                dispatcher
                    .ingest_outbound(
                        &mallory_context,
                        TransportOutbound::Subscribe(TransportSubscribe {
                            channel_id: game_channel(game_id),
                            last_seen_revision: None,
                        }),
                    )
                    .await
                    .is_err()
            );

            let [TransportInbound::Snapshot(alice_snapshot)] = alice_inbound.as_slice() else {
                panic!("Alice receives one snapshot");
            };
            let [TransportInbound::Snapshot(bob_snapshot)] = bob_inbound.as_slice() else {
                panic!("Bob receives one snapshot");
            };
            let alice_view: crate::GameView = alice_snapshot
                .payload
                .deserialize()
                .expect("Alice view decodes");
            let bob_view: crate::GameView = bob_snapshot
                .payload
                .deserialize()
                .expect("Bob view decodes");
            assert_ne!(alice_view.rack, bob_view.rack);
            assert_eq!(alice_view.board, bob_view.board);
            let alice_payload =
                serde_json::to_string(alice_snapshot).expect("private snapshot serializes");
            for forbidden in ["\"bag\"", "password", "session", "invitation"] {
                assert!(!alice_payload.contains(forbidden));
            }
        });
    }

    #[test]
    fn accepted_command_fans_out_member_specific_views() {
        block_on(async {
            let (database, game_id, alice, bob, _) = fixture().await;
            let dispatcher = GameSharedStateDispatcher::new(database);
            let alice_events = dispatcher
                .subscribe_channel(&context(&alice), &game_channel(game_id))
                .await
                .expect("Alice live subscription opens");
            let bob_events = dispatcher
                .subscribe_channel(&context(&bob), &game_channel(game_id))
                .await
                .expect("Bob live subscription opens");
            let alice_initial = alice_events.recv_async().await.expect("initial update");
            let bob_initial = bob_events.recv_async().await.expect("initial update");
            let alice_payload = serde_json::to_string(&alice_initial)
                .expect("private transport envelope serializes");
            assert!(!alice_payload.contains("\"state\""));
            assert!(!alice_payload.contains("\"bag\""));
            let state_for_privacy = recover_game(&*dispatcher.database, game_id)
                .await
                .expect("state loads for privacy assertion");
            let bob_player = dispatcher
                .authorize(&context(&bob), &game_channel(game_id))
                .await
                .expect("Bob is authorized")
                .1;
            for tile in &state_for_privacy.racks[&bob_player] {
                let forbidden = format!("[{},", tile.id.get());
                assert!(
                    !alice_payload.contains(&forbidden),
                    "Alice transport payload must not contain Bob's rack tile IDs"
                );
            }
            assert!(
                dispatcher
                    .project_event(&context(&alice), &alice_initial)
                    .is_some()
            );
            assert!(
                dispatcher
                    .project_event(&context(&bob), &alice_initial)
                    .is_none()
            );
            assert!(
                dispatcher
                    .project_event(&context(&bob), &bob_initial)
                    .is_some()
            );

            let state = recover_game(&*dispatcher.database, game_id)
                .await
                .expect("state loads");
            let command = CommandEnvelope {
                command_id: CommandId::new("pass-command"),
                channel_id: game_channel(game_id),
                participant_id: ParticipantId::new(&alice),
                idempotency_key: IdempotencyKey::new("pass-idempotency"),
                expected_revision: Revision::new(state.revision),
                command_name: "PASS".to_string(),
                payload: PayloadBlob::from_serializable(&GameCommand::Pass)
                    .expect("command encodes"),
                metadata: BTreeMap::new(),
                created_at_ms: 1,
            };
            let response = dispatcher
                .ingest_outbound(&context(&alice), TransportOutbound::Command(command))
                .await
                .expect("command dispatches");
            assert!(matches!(
                response.as_slice(),
                [TransportInbound::CommandAccepted { .. }]
            ));

            for (user, receiver) in [(&alice, alice_events), (&bob, bob_events)] {
                let canonical = receiver.recv_async().await.expect("live update arrives");
                let projected = dispatcher
                    .project_event(&context(user), &canonical)
                    .expect("member update projects");
                assert_eq!(projected.event_name, GAME_VIEW_EVENT);
                let view: crate::GameView = projected.payload.deserialize().expect("view decodes");
                assert_eq!(view.revision, state.revision + 1);
                assert_eq!(view.rack.len(), 7);
            }
        });
    }
}
