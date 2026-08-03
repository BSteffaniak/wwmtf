# Production deployment

The first production topology deliberately uses one Fly Machine and one local Turso database on a Fly Volume. It does not require Turso Cloud, a remote Switchy backend, or Cloudflare Tunnel.

```text
Browser -> Cloudflare -> Fly Proxy -> one Machine -> /data Fly Volume
```

The canonical URL is `https://wwmtf.hyperchad.dev`. The path `https://hyperchad.dev/games/wwmtf` redirects to it when shared zone rules are enabled.

## Prerequisites

- Fly organization and an app name available globally.
- `hyperchad.dev` already active in Cloudflare.
- `flyctl`, `jq`, `aws`, `age`, OpenTofu, and credentials with narrow scopes.
- R2 enabled on the Cloudflare account. The repository bootstrap creates the private state and encrypted-backup buckets.

Copy `fly.toml` and OpenTofu variable defaults if the chosen Fly app name differs from `words-with-spouses`.

## Bootstrap Fly

```sh
FLY_APP_NAME=words-with-spouses \
FLY_ORG=personal \
./scripts/deploy.sh bootstrap
```

This creates the app, a 1 GiB `wwmtf_data` volume in `iad`, 60-day scheduled snapshot retention, public Fly IPs, and the Fly certificate attachment for `wwmtf.hyperchad.dev`. It is idempotent. The committed Fly hostname is `words-with-spouses.fly.dev`; if the app name changes, update `fly.toml`, `Dockerfile`, `config/nginx.conf`, and the OpenTofu variable together.

Deploy and test the Fly origin:

```sh
FLY_APP_NAME=words-with-spouses ./scripts/deploy.sh deploy
```

The container's internal nginx proxy exposes port 8080, enforces an allowlist for the canonical and Fly hosts, validates same-origin requests, limits account POSTs, applies a 64 KiB body limit, strips upstream permissive CORS headers, adds CSP/privacy/security headers, and forwards streaming traffic to the application on `127.0.0.1:8343`. The CSP permits `script-src 'unsafe-eval'` because HyperChad's renderer evaluates its serialized action expressions; inline scripts and third-party script origins remain blocked. Nginx logs only the URI path—not query strings—and production disables the upstream request logger target. The database is always `/data/words-with-spouses.db`; do not run more than one Machine against this volume.

The default deployment keeps the Machine running (`auto_stop_machines = "off"`) so long-lived HyperChad SSE sessions remain predictable. Revisit suspension only after production reconnect testing proves it acceptable.

## Cloudflare/OpenTofu

### One-time R2 bootstrap

Use a temporary Cloudflare API token with only account R2 bucket write access. Set unique bucket names and run the interactive bootstrap:

```sh
export TF_VAR_cloudflare_api_token='...'
export TF_VAR_cloudflare_account_id='...'
export TF_VAR_state_bucket_name='unique-wwmtf-opentofu-state'
export TF_VAR_backup_bucket_name='unique-wwmtf-encrypted-backups'
./scripts/bootstrap-cloudflare.sh
```

The bootstrap creates private Standard-class R2 buckets. The state bucket's immutable `history/` prefix has a 365-day object lock and matching expiration policy; the live state and `.tflock` keys remain writable. The backup bucket has a 180-day object lock and matching expiration policy. Both bucket resources also use `prevent_destroy`. R2 does not provide S3 bucket versioning, so `scripts/archive-opentofu-state.sh archive` makes a server-side encrypted-state copy before every CI apply. OpenTofu's lock object prevents concurrent applies; native OpenTofu AES-GCM encryption protects the state before upload.

`infra/bootstrap/terraform.tfstate` intentionally remains local. Encrypt and store an offline copy before removing it. After bootstrap, revoke the temporary management token and create two distinct bucket-scoped R2 API tokens in the Cloudflare dashboard:

- State: Object Read & Write for the state bucket only.
- Backups: Object Read & Write for the backup bucket only.

Creating the two bucket-scoped S3 credentials is deliberately a manual trust-bootstrap step: it keeps credential material out of OpenTofu state and avoids granting the bootstrap token account token-management rights.

### Main Cloudflare stack

The bootstrap writes the non-secret `infra/deploy/backend.hcl`. Provide its state-bucket credentials through `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`. Generate a high-entropy state encryption passphrase, store it separately from the R2 credentials, and supply it through `TF_VAR_state_encryption_passphrase`. Losing this passphrase makes the state unrecoverable. Before local applies, run `scripts/archive-opentofu-state.sh archive` with `TOFU_STATE_BUCKET` and `TOFU_STATE_S3_ENDPOINT`; CI does this automatically. The script also supports `list` and an interactive `restore <history-key>` recovery operation.

Supply the main provider inputs through `TF_VAR_cloudflare_api_token` and `TF_VAR_cloudflare_account_id`:

```sh
cd infra/deploy
export AWS_ACCESS_KEY_ID='...'
export AWS_SECRET_ACCESS_KEY='...'
export TF_VAR_state_encryption_passphrase='...'
export TF_VAR_cloudflare_api_token='...'
export TF_VAR_cloudflare_account_id='...'
tofu init -backend-config=backend.hcl
tofu plan
```

The initial apply manages the app DNS records and strict origin TLS. The committed origin values (`fly_ipv6_address` and `fly_ownership_txt`) came from `fly certs setup wwmtf.hyperchad.dev`; refresh them if Fly resources are recreated. Shared zone-phase rulesets are intentionally disabled initially. `hyperchad.dev` may contain unrelated services, and Cloudflare has one entry-point ruleset per zone phase. Inventory or import existing redirects, WAF, rate-limit, cache, and response-header rules before setting `manage_zone_rulesets = true`. Once adopted, keep those resources authoritative in OpenTofu rather than editing them in the dashboard.

For GitHub, configure the protected `production` environment with:

- Secrets: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, `TOFU_BACKEND_HCL`, `TOFU_STATE_ACCESS_KEY_ID`, `TOFU_STATE_SECRET_ACCESS_KEY`, `TOFU_STATE_ENCRYPTION_PASSPHRASE`, `TOFU_STATE_BUCKET`, and `TOFU_STATE_S3_ENDPOINT`.
- Backup secrets: `BACKUP_AGE_RECIPIENT`, `BACKUP_DESTINATION` (`s3://<backup-bucket>/wwmtf`), `BACKUP_ACCESS_KEY_ID`, `BACKUP_SECRET_ACCESS_KEY`, and `BACKUP_S3_ENDPOINT` (`https://<account-id>.r2.cloudflarestorage.com`).

Use a distinct Cloudflare provider token for the main stack. Scope it to the `hyperchad.dev` zone resources in this configuration plus account Turnstile access only if Turnstile is enabled.

Turnstile provisioning is also disabled because the current renderer has no registration integration. Setting `manage_turnstile = true` only creates the widget; do not enable it until verification is implemented. Managed WAF execution is likewise opt-in through `cloudflare_managed_ruleset_id`; query the zone's available managed rulesets and use only an ID supported by its Free plan.

## Deployments and snapshots

For an ordinary release:

```sh
./scripts/deploy.sh deploy
./scripts/deploy.sh smoke
```

Before a release that changes schema or persisted payloads:

```sh
./scripts/deploy.sh snapshot
./scripts/deploy.sh deploy
```

Fly scheduled volume snapshots and the application-consistent archive are complementary. The Fly snapshot captures the stopped volume, while `/data/backups/database.tar.gz` is the portable database-plus-sidecars export.

## Off-Fly backups

Fly snapshots are not the sole long-term backup. Create a stopped database-plus-sidecar archive:

```sh
BACKUP_AGE_RECIPIENT='age1...' \
BACKUP_DESTINATION='s3://private-bucket/wwmtf' \
./scripts/backup-production.sh
```

For Cloudflare R2, configure the AWS CLI with the backup bucket's dedicated S3 endpoint and bucket-scoped credentials. Keep the private age identity outside GitHub so a compromise of Cloudflare plus the repository's production secrets still does not reveal backup plaintext. Never put encryption keys or bucket credentials in source control.

Restore a Fly snapshot into a new, unattached volume:

```sh
SNAPSHOT_ID=vs_... ./scripts/restore-volume-snapshot.sh
```

Then follow the printed drill and the recovery obligations in `DEPLOYMENT.md`.

## Required production settings

The container fails closed unless these are configured outside development mode:

- `WORDS_WITH_SPOUSES_PRODUCTION_MODE=true`
- `WORDS_WITH_SPOUSES_DATABASE_PATH=/data/words-with-spouses.db`
- `WORDS_WITH_SPOUSES_PUBLIC_BASE_URL=https://wwmtf.hyperchad.dev`
- `WORDS_WITH_SPOUSES_DEV_MODE` absent/false

## Launch checks

- `/health/live` returns process liveness.
- `/health/ready` verifies the migration table, canonical journal table, and a database query.
- Registration, login, invitation creation/redeeming, two-player live updates, and a persisted turn work through Cloudflare.
- A restart and redeploy preserve accounts and games.
- A restored volume passes the complete recovery drill.
- Cloudflare does not cache application responses.
- Logs do not expose invitation, session, CSRF, rack, bag, or canonical-event secrets.

The direct `<app>.fly.dev` origin remains reachable. Cloudflare controls are defense in depth, not the application's only authentication or authorization boundary.
