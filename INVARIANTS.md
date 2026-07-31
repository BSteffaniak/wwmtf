# Architectural invariants

These are durable conditions of a valid Words with Spouses implementation. They describe product and architectural truth rather than contributor workflow.

## Gameplay authority

Only the server-authoritative deterministic game aggregate and its canonical journal decide gameplay outcomes. Client, rendered, transport, and projection state never become gameplay authority.

## Hidden information

A player's rack, the tile bag, credentials, sessions, invitations, and other secrets never reach unauthorized clients, rendered output, transport payloads, replay streams, or logs.

## Renderer neutrality

Application code uses HyperChad abstractions for routing, actions, rendering, shared state, and transport. It contains no custom JavaScript, direct Actix integration, renderer-specific behavior, or parallel live-update path.

## Persistence portability

All application schema and query access uses `switchy` builders. Application code contains no raw SQL or backend-specific persistence branch.

## Replay compatibility

Every persisted game pins immutable rules and dictionary identities and versions sufficient to preserve deterministic replay across application upgrades.

## Derived projections

Dashboard, history, and score projections are rebuildable from canonical records and never become a second gameplay source of truth.
