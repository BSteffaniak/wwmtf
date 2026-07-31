use std::sync::Arc;

use async_trait::async_trait;
use hyperchad::{
    shared_state_models::{ChannelId, EventEnvelope, TransportInbound, TransportOutbound},
    shared_state_transport::{
        AuthenticatedTransportContext, SharedStateTransportDispatchResult,
        SharedStateTransportDispatcher,
    },
};

/// Deny-by-default application dispatcher until game membership repositories are connected.
///
/// This policy is renderer-neutral: it receives only a trusted participant context and
/// shared-state protocol values. HTTP authentication and CSRF stay in web runtime wiring.
#[derive(Debug, Default)]
pub struct DenyByDefaultDispatcher;

#[async_trait]
impl SharedStateTransportDispatcher for DenyByDefaultDispatcher {
    async fn ingest_outbound(
        &self,
        _context: &AuthenticatedTransportContext,
        outbound: TransportOutbound,
    ) -> SharedStateTransportDispatchResult<Vec<TransportInbound>> {
        let response = match outbound {
            TransportOutbound::Command(command) => TransportInbound::CommandRejected {
                command_id: command.command_id,
                reason: "game membership policy is not connected".to_string(),
            },
            TransportOutbound::Ping(ping) => TransportInbound::Pong(ping),
            TransportOutbound::Subscribe(_) | TransportOutbound::Unsubscribe(_) => {
                return Err("game membership policy is not connected".into());
            }
        };

        Ok(vec![response])
    }

    async fn subscribe_channel(
        &self,
        _context: &AuthenticatedTransportContext,
        _channel_id: &ChannelId,
    ) -> SharedStateTransportDispatchResult<flume::Receiver<EventEnvelope>> {
        Err("game membership policy is not connected".into())
    }

    fn project_event(
        &self,
        _context: &AuthenticatedTransportContext,
        _event: &EventEnvelope,
    ) -> Option<EventEnvelope> {
        None
    }
}

#[must_use]
pub fn shared_state_dispatcher() -> Arc<dyn SharedStateTransportDispatcher> {
    Arc::new(DenyByDefaultDispatcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperchad::shared_state_models::{
        CommandEnvelope, CommandId, IdempotencyKey, ParticipantId, PayloadBlob, Revision,
    };
    use std::collections::BTreeMap;

    #[test]
    fn commands_are_rejected_until_membership_is_connected() {
        let dispatcher = DenyByDefaultDispatcher;
        let context = AuthenticatedTransportContext {
            participant_id: ParticipantId::new("participant-a"),
            identity_binding: "identity-a".to_string(),
        };
        let command = CommandEnvelope {
            command_id: CommandId::new("command-a"),
            channel_id: ChannelId::new("game-a"),
            participant_id: ParticipantId::new("participant-a"),
            idempotency_key: IdempotencyKey::new("idem-a"),
            expected_revision: Revision::new(7),
            command_name: "PASS".to_string(),
            payload: PayloadBlob::from_serializable(&()).expect("payload serializes"),
            metadata: BTreeMap::new(),
            created_at_ms: 1,
        };

        let response = futures_lite::future::block_on(
            dispatcher.ingest_outbound(&context, TransportOutbound::Command(command)),
        )
        .expect("denial response is transportable");

        assert!(matches!(
            response.as_slice(),
            [TransportInbound::CommandRejected { .. }]
        ));
    }
}
