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
use words_with_spouses_game_domain::{GameCommand, GameId, GameState, PlayerId};

use crate::{game_view, player_for_user, recover_game, submit_game_command};

const GAME_CHANNEL_PREFIX: &str = "game:";
const GAME_STATE_EVENT: &str = "GAME_STATE_V1";
const GAME_VIEW_EVENT: &str = "GAME_VIEW_UPDATED_V1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerGameUpdate {
    state: GameState,
    players_by_user: BTreeMap<String, PlayerId>,
}

/// Membership-authorized renderer-neutral gameplay dispatcher.
///
/// Canonical state remains server-internal. [`Self::project_event`] converts each update to the
/// requesting member's public board and private rack before it crosses the transport boundary.
#[derive(Debug)]
pub struct GameSharedStateDispatcher {
    database: Arc<dyn Database>,
    subscribers: Mutex<BTreeMap<ChannelId, Vec<flume::Sender<EventEnvelope>>>>,
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
        state: GameState,
    ) -> SharedStateTransportDispatchResult<ServerGameUpdate> {
        let rows = self
            .database
            .select("game_players")
            .where_eq("game_id", game_id.to_string())
            .execute(&*self.database)
            .await?;
        let mut players_by_user = BTreeMap::new();
        for row in rows {
            let user_id = row
                .get("user_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or("game membership user is malformed")?;
            let player_id = row
                .get("game_player_id")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or("game membership player is malformed")?;
            players_by_user.insert(
                user_id,
                PlayerId::from_str(&player_id)
                    .map_err(|_| "game membership player is malformed")?,
            );
        }
        if players_by_user.len() != 2 {
            return Err("game must have exactly two members".into());
        }
        Ok(ServerGameUpdate {
            state,
            players_by_user,
        })
    }

    fn canonical_event(
        update: &ServerGameUpdate,
        command_id: Option<hyperchad::shared_state_models::CommandId>,
        created_at_ms: i64,
    ) -> SharedStateTransportDispatchResult<EventEnvelope> {
        let game_id = update.state.metadata.id();
        let revision = Revision::new(update.state.revision);
        Ok(EventEnvelope {
            event_id: EventId::new(format!("{game_id}:{}", revision.value())),
            channel_id: game_channel(game_id),
            revision,
            command_id,
            event_name: GAME_STATE_EVENT.to_string(),
            payload: PayloadBlob::from_serializable(update)?,
            metadata: BTreeMap::new(),
            created_at_ms,
        })
    }

    fn publish(&self, event: &EventEnvelope) -> SharedStateTransportDispatchResult<()> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| "game subscriber registry is unavailable")?;
        if let Some(channel_subscribers) = subscribers.get_mut(&event.channel_id) {
            channel_subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        }
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
                let update = self.server_update(game_id, state).await?;
                let event = Self::canonical_event(
                    &update,
                    Some(command.command_id.clone()),
                    command.created_at_ms,
                )?;
                self.publish(&event)?;
                Ok(vec![TransportInbound::CommandAccepted {
                    command_id: command.command_id,
                    resulting_revision,
                }])
            }
            TransportOutbound::Subscribe(subscribe) => {
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
        let (game_id, _) = self.authorize(context, channel_id).await?;
        let (sender, receiver) = flume::unbounded();
        self.subscribers
            .lock()
            .map_err(|_| "game subscriber registry is unavailable")?
            .entry(channel_id.clone())
            .or_default()
            .push(sender.clone());

        // Register before loading so a command racing subscription is duplicated at worst, never
        // lost. Revision-aware clients converge on the newest update.
        let state = recover_game(&*self.database, game_id).await?;
        let update = self.server_update(game_id, state).await?;
        sender.send(Self::canonical_event(&update, None, 0)?)?;
        Ok(receiver)
    }

    fn project_event(
        &self,
        context: &AuthenticatedTransportContext,
        event: &EventEnvelope,
    ) -> Option<EventEnvelope> {
        if event.event_name != GAME_STATE_EVENT {
            return None;
        }
        let update: ServerGameUpdate = event.payload.deserialize().ok()?;
        let player = update
            .players_by_user
            .get(context.participant_id.as_str())
            .copied()?;
        let view = game_view(&update.state, player)?;
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
            assert!(
                dispatcher
                    .project_event(&context(&alice), &alice_initial)
                    .is_some()
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
