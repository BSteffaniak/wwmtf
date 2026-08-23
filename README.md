# Words with More Than Friends

A private, asynchronous multiplayer word-tile game built in Rust with HyperChad.

Authenticated players can create configurable private lobbies, invite multiple participants, choose board and tile-supply settings, and manually start deterministic server-authoritative games. Public board, scores, turn state, activity, and standings are shared while each participant receives only their own private rack.

The repository is under active implementation. See `INVARIANTS.md` for durable architectural boundaries, `PERSISTENCE.md` for replay/payload compatibility rules, `DEPLOYMENT.md` for runtime and recovery obligations, and `AGENTS.md` for contributor validation requirements.
