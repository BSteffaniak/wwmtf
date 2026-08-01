# Deployment and Recovery

Words with Spouses is currently designed for a private deployment behind a TLS-terminating reverse proxy. The application remains renderer-neutral; HTML/Actix is selected only by runtime wiring.

## Runtime configuration

| Variable | Required | Purpose |
| --- | --- | --- |
| `WORDS_WITH_SPOUSES_BIND_ADDRESS` | No; defaults to `127.0.0.1` | Internal listener address. Keep loopback when using a local reverse proxy. |
| `WORDS_WITH_SPOUSES_PORT` | No; defaults to `8343` | Internal listener port. |
| `WORDS_WITH_SPOUSES_DATABASE_PATH` | Production: yes | Durable local Turso database path. Do not place it on ephemeral storage. |
| `WORDS_WITH_SPOUSES_PUBLIC_BASE_URL` | Production: yes operationally | Canonical HTTPS origin used by deployment/proxy configuration. The current runtime does not generate absolute links. |
| `RUST_LOG` | No | Logging filter. Logs must never include passwords, session/invitation tokens, racks, bags, or canonical event payloads. |

The pinned `switchy_database_connection` Turso backend is local/file-backed and does not accept a Turso Cloud URL/token. A remote Turso deployment therefore requires a generic upstream `switchy` connection capability before this application may support `TURSO_DATABASE_URL`/`TURSO_AUTH_TOKEN`; do not add a backend-specific application bypass.

The session cookie carries only a random opaque token whose hash is persisted. Cookie and CSRF transport policy is renderer-owned by HyperChad. Any future cookie-signing/encryption key must come from a secret manager or environment and must never be committed or logged.

## Reverse proxy and TLS

- Terminate TLS at the trusted reverse proxy and forward only to the configured internal listener.
- Preserve streaming for HyperChad SSE/WebSocket endpoints; disable response buffering and use timeouts suitable for long-lived connections.
- Forward the canonical host/protocol and reject untrusted host headers at the edge.
- Restrict direct access to the internal Actix listener.
- Serve one HTTPS origin so secure, HTTP-only, same-site session cookies and renderer-owned CSRF behavior remain effective.

## Startup and migrations

The binary opens the configured database and runs all application code migrations before constructing or accepting traffic. A migration failure aborts startup. Application schema/query access remains builder-only through `switchy`.

Before deploying a release with migrations:

1. stop writers or drain the old instance;
2. create and verify a database backup;
3. start one new instance and allow migrations to finish;
4. run health/product-path checks; and
5. only then admit normal traffic.

Persisted gameplay payload changes additionally follow `PERSISTENCE.md`.

## Backup and restore

Back up the database while the application is stopped, or use a storage-level snapshot that atomically includes the database and its `-wal`/`-shm` sidecars. Copying only the main file while writes are active is not a valid backup.

Restore drill:

1. stop the application and retain the original database files;
2. restore the backed-up database and sidecars to a new path;
3. set `WORDS_WITH_SPOUSES_DATABASE_PATH` to that path;
4. start the application and confirm migrations are idempotent;
5. verify intended sessions still resolve according to expiry/revocation policy;
6. open multiple active games and compare board, rack, score, revision, and history with pre-backup expectations;
7. rebuild projections from canonical journals and verify dashboard/history totals are unchanged;
8. open two authenticated clients, confirm private live subscriptions reconnect, and complete a normal turn; and
9. retain the drill date, backup identity, release revision, and results outside source control.

The file-backed automated recovery tests now cover a stopped database-plus-sidecar backup restored to a new path, idempotent migration startup, intended durable sessions, canonical active-game/history recovery, private game and dashboard live rehydration for both players, and a successful persisted turn after restore. Repeat the same drill against the actual production backup mechanism for every deployment before relying on that backup operationally.

## Local development

Use a disposable path, for example:

```sh
WORDS_WITH_SPOUSES_DATABASE_PATH=/tmp/words-with-spouses-dev.db \
WORDS_WITH_SPOUSES_BIND_ADDRESS=127.0.0.1 \
WORDS_WITH_SPOUSES_PORT=8343 \
cargo run -p words_with_spouses_app --bin words-with-spouses
```

Delete disposable development databases only when no retained game data is needed. Never reuse production credentials or production database copies without an approved, sanitized workflow.
