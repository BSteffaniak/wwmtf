# Deployment and Recovery

Words with More Than Friends is designed for an internet deployment behind a TLS-terminating reverse proxy. The application remains renderer-neutral; HTML/Actix is selected only by runtime wiring.

## Runtime configuration

| Variable | Required | Purpose |
| --- | --- | --- |
| `WWMTF_PRODUCTION_MODE` | Production: yes (`true`) | Enables fail-closed production configuration checks. Mutually exclusive with development mode. |
| `WWMTF_BIND_ADDRESS` | No; defaults to `0.0.0.0` | Listener address. Override with `127.0.0.1` when only a local reverse proxy should connect. |
| `WWMTF_PORT` | No; defaults to `8343` | Listener port. |
| `WWMTF_DEV_MODE` | No; defaults to disabled | Set to `true` only for local/LAN development over HTTP. This permits an HTTP public URL and emits non-`Secure` session/CSRF cookies. Never enable it in production. |
| `WWMTF_DATABASE_PATH` | Production: yes | Durable local Turso database path. Do not place it on ephemeral storage. |
| `WWMTF_PUBLIC_BASE_URL` | Production: yes | Canonical HTTPS origin used by deployment/proxy configuration, generated invitation links, and the Google callback URI (`/auth/google/callback`). Register that exact callback URI in Google Cloud. |
| `WWMTF_GOOGLE_CLIENT_ID` | Yes | Google OpenID Connect web client ID. The application fails startup when it is absent. |
| `WWMTF_GOOGLE_CLIENT_SECRET` | Yes | Google OpenID Connect web client secret. Supply through the deployment secret store; never log or commit it. The application fails startup when it is absent. |
| `WWMTF_DEFINITIONS_ENABLED` | No; defaults to enabled | Controls server-side played-word definition lookup. Set to `false`, `no`, or `0` to disable it; invalid values fail startup. |
| `WWMTF_DEFINITION_PROVIDER_BASE_URL` | No; defaults to `https://api.dictionaryapi.dev` | HTTPS Free Dictionary API-compatible endpoint used only when definitions are enabled. |
| `WWMTF_DEFINITION_TIMEOUT_MS` | No; defaults to `3000` | Connect and overall timeout for definition-provider requests. |
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

Played-word definitions are enabled by default and use Free Dictionary API data derived from Wiktionary. Set `WWMTF_DEFINITIONS_ENABLED=false` to disable lookups explicitly. Successful responses are cached for 30 days and provider-confirmed misses for 24 hours. Disabled lookup, timeouts, connection failures, rate limits, provider failures or rejected configuration, malformed or oversized responses, missing attribution, and cache failures render distinct user-visible states and emit secret-safe reason logs; transient failures are not cached. Responses must include HTTPS source and CC BY-SA license metadata, which the UI displays. The public service advertises free use but does not publish a production SLA or guaranteed rate limit, so production operators must monitor availability, keep the short timeout enabled, and disable `WWMTF_DEFINITIONS_ENABLED` if provider terms or reliability become unsuitable. Definition availability never affects gameplay or game loading.

The generic `switchy_http` client was evaluated but does not currently expose request timeouts or bounded response streaming. The application therefore uses a narrowly configured root `reqwest` dependency for this server-side integration; it remains isolated behind `DefinitionProvider` for deterministic tests and replacement.

Google sign-in uses authorization code flow with PKCE and discovers Google's OpenID Connect metadata at startup. Configure a Google **Web application** OAuth client with the exact production redirect URI `https://wwmtf.hyperchad.dev/auth/google/callback`. For local development, register only the exact origin in use, for example `http://127.0.0.1:8343/auth/google/callback`; do not register wildcard callbacks. The public base URL must be an origin without credentials, path, query, or fragment. Rotate the client secret through the protected GitHub `production` environment: the deployment workflow stages `WWMTF_GOOGLE_CLIENT_ID` and `WWMTF_GOOGLE_CLIENT_SECRET` as Fly secrets immediately before deploy without printing their values. Existing WWMTF sessions remain independently revocable and do not contain Google tokens. Google access and refresh tokens are not persisted.

The production Fly deployment, volume/bootstrap commands, OpenTofu resources, backup commands, and launch checklist are documented in `PRODUCTION.md`.

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
3. set `WWMTF_DATABASE_PATH` to that path;
4. start the application and confirm migrations are idempotent;
5. verify intended sessions still resolve according to expiry/revocation policy;
6. open multiple active games and compare board, rack, score, revision, and history with pre-backup expectations;
7. rebuild projections from canonical journals and verify dashboard/history totals are unchanged;
8. open two authenticated clients, confirm private live subscriptions reconnect, and complete a normal turn; and
9. retain the drill date, backup identity, release revision, and results outside source control.

The file-backed automated recovery tests now cover a stopped database-plus-sidecar backup restored to a new path, idempotent migration startup, intended durable sessions, canonical active-game/history recovery, private game and dashboard live rehydration for both players, and a successful persisted turn after restore. Repeat the same drill against the actual production backup mechanism for every deployment before relying on that backup operationally.

The backup archive path and restore set are validated by `scripts/test-backup.sh`, which exercises the same supervisor signal path used by production and proves the database, WAL, and SHM files round-trip together. This file-level test complements product-level recovery tests; it does not replace the required production restore drill with identity/profile/avatar data.

## Local development

Use a disposable path. For plain-HTTP access from another device on the LAN, set the public base URL to this machine's reachable LAN IP or hostname:

```sh
WWMTF_DEV_MODE=true \
WWMTF_PUBLIC_BASE_URL=http://192.168.1.20:8343 \
WWMTF_DATABASE_PATH=/tmp/wwmtf-dev.db \
cargo run -p wwmtf_app --bin wwmtf --features insecure -- serve
```

The listener defaults to `0.0.0.0:8343`. Replace `192.168.1.20` with the host's actual LAN address and allow the port through the local firewall if necessary. The `insecure` Cargo feature selects HyperChad's development UUID generator because browser Web Crypto UUID generation is unavailable on plain-HTTP LAN origins. Development mode and the insecure renderer feature intentionally weaken transport/runtime security and must never be enabled on an internet-facing or production deployment.

Delete disposable development databases only when no retained game data is needed. Never reuse production credentials or production database copies without an approved, sanitized workflow.
