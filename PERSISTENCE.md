# Persistence and Replay Compatibility

Words with More Than Friends treats the canonical game journal as the gameplay source of truth. Snapshots and query projections are derived data.

## Immutable game inputs

Every game start event and game row pins:

- rules profile identifier and version;
- dictionary identifier, version, and content checksum; and
- the complete deterministic starting state, including tile order and private racks.

`packages/game_domain` resolves persisted rules and dictionary references through explicit version registries. Unknown references fail closed. Existing fixture bytes and behavior must never be changed in place; changing rules or dictionary data requires a new immutable version retained alongside all older versions that may still be replayed.

## Canonical payload versions

Versions 1 and 2 remain immutable and readable. Version 3 is the current writer format for events and snapshots. Version 2 introduced deterministically ordered coordinate entry arrays; version 3 adds ordered variable-size membership, active/resigned participation, completion reasons and leaders, and the complete resolved `RuleProfile` used by configurable games.

A version 3 start record pins the generated profile identity/version and its complete public rules content: board dimension, start square, premium map, tile distribution, rack/exchange/bonus/pass settings, and dictionary identity. Replay consumes that resolved content and never consults lobby rows or current runtime creation limits. Generated profile identifiers include their generation inputs and immutable generator version; changing the generator requires a new profile version rather than reinterpreting retained payloads.

Readers retain explicit decoders for versions 1, 2, and 3 and reject unknown versions.

Before advancing either writer version:

1. retain a decoder for every version present in supported databases;
2. add a deterministic conversion into the current domain model when the old representation differs;
3. prove full-journal and snapshot-plus-tail replay produce identical state;
4. add fixtures covering the old serialized bytes and the upgraded result;
5. deploy readers that understand both versions before deploying writers for the new version; and
6. only remove an old decoder after an explicit data migration and restore rehearsal proves no retained payload uses it.

Readers reject unknown versions. They must not deserialize an unknown payload into the current model by best effort.

## Schema migrations

Application schema changes use ordered `switchy_schema` code migrations and `switchy` schema/query builders only. Migrations are append-only after release. Tests install every retained migration boundary and upgrade it to the current schema.

A destructive or payload-transforming migration requires a backup, an idempotent transformation, validation against canonical replay, and a documented rollback/restore procedure before deployment.

## Projections

Game summaries, move history, per-game score rows, and per-user score totals are rebuildable. Rebuilding one game replaces that game's derived rows from canonical state/events and then regenerates aggregate user totals from completed-game score rows. Projection repair must remain idempotent and must never write gameplay state back into the journal.
