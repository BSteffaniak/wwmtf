# Production deployment

Production uses one Fly Machine and one local Turso database on a Fly Volume:

```text
Browser -> Cloudflare -> Fly Proxy -> one Machine -> /data Fly Volume
```

The canonical URL is `https://wwmtf.hyperchad.dev`. The direct Fly hostname remains reachable for origin diagnostics.

## One-time setup

The intended setup is one protected GitHub workflow after credentials are added. No local OpenTofu or Fly bootstrap is required.

### 1. Create credentials

Create these credentials without placing their values in source control:

- A Cloudflare API token for the account containing `hyperchad.dev`. It needs account R2 bucket edit plus zone read, DNS edit, and zone settings edit for `hyperchad.dev`.
- R2 S3 credentials with object read/write access. Because the workflow creates the state bucket, these credentials must initially be account-scoped.
- A Fly organization-scoped token that can create the application and its resources.
- A high-entropy OpenTofu state-encryption passphrase. Losing it makes state unreadable.

The Cloudflare management token, R2 S3 credentials, and Fly token are separate credentials.

### 2. Add GitHub production secrets

Create or open the protected GitHub environment named `production`, restrict it to the production branch, and add:

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`
- `TOFU_STATE_ACCESS_KEY_ID`
- `TOFU_STATE_SECRET_ACCESS_KEY`
- `TOFU_STATE_ENCRYPTION_PASSPHRASE`
- `FLY_API_TOKEN`
- `FLY_ORG`

### 3. Run Bootstrap Production

Run the `Bootstrap Production` workflow and approve the `production` environment deployment. The workflow is idempotent and:

1. Creates or adopts the private `words-with-spouses-opentofu-state` R2 bucket.
2. Initializes encrypted, lock-protected OpenTofu state and imports the bootstrap-created bucket.
3. Creates or adopts the Fly app, volume, scheduled snapshots, public IPs, and certificate attachment.
4. Discovers Fly's actual IPv6 address and ownership TXT value.
5. Applies the Cloudflare DNS records and strict origin TLS.
6. Deploys one Machine and verifies both the Fly origin and canonical hostname.
7. Archives the encrypted infrastructure state under its immutable history prefix.

If the fixed state-bucket name is already owned by another Cloudflare account, change `state_bucket_name` consistently in `infra/deploy/state.tf`, `infra/deploy/backend.hcl.example`, and the two production preparation scripts before the first run.

## Routine operation

- Run `Deploy Application` for releases. It snapshots the volume by default, deploys exactly one Machine, and checks the Fly and canonical endpoints.
- Run `Deploy Infrastructure` for Cloudflare/OpenTofu changes. It discovers current Fly inputs, archives state, plans, and applies.
- The three production workflows share one concurrency group so bootstrap, application deploys, and infrastructure applies cannot overlap.

The state bucket has a 365-day immutable `history/` prefix and a matching expiration rule. Live state and `.tflock` objects remain writable. OpenTofu encrypts state and saved plans with AES-GCM before upload. R2 does not provide S3 object versioning, so each infrastructure apply makes a server-side encrypted-state archive first.

## Cloudflare scope

The initial stack manages the app DNS records and the zone's strict TLS setting. Verify every existing proxied `hyperchad.dev` origin supports strict TLS before the first bootstrap.

Shared zone-phase rulesets remain disabled initially because `hyperchad.dev` can host unrelated services and Cloudflare has one entry-point ruleset per phase. Inventory or import existing redirect, WAF, rate-limit, cache, and response-header rules before setting `manage_zone_rulesets = true`. Until then, `https://hyperchad.dev/games/wwmtf` may not redirect to the canonical hostname.

Turnstile remains disabled because registration verification is not integrated. `manage_turnstile = true` only provisions a widget and must not be enabled before application verification exists.

## Fly storage and snapshots

The database is always `/data/words-with-spouses.db`; never run more than one Machine against this volume. The Machine remains running because long-lived HyperChad SSE sessions have not been qualified for suspension.

Fly scheduled snapshots and pre-deploy snapshots are rollback protection, not an off-Fly backup service. The snapshot path creates `/data/backups/database.tar.gz` inside the volume so the database and sidecars are consistent when the volume snapshot is taken.

Restore a snapshot to a new, unattached volume with:

```sh
SNAPSHOT_ID=vs_... ./scripts/restore-volume-snapshot.sh
```

Follow the recovery obligations in `DEPLOYMENT.md` before attaching a restored volume to production.

## Runtime security

The container's internal nginx proxy exposes port 8080, validates host and same-origin requests, limits account POSTs, applies a 64 KiB body limit, strips permissive CORS headers, and adds CSP/privacy/security headers. It forwards streaming traffic to the renderer-neutral application on `127.0.0.1:8343`.

Production fails closed unless configured with:

- `WORDS_WITH_SPOUSES_PRODUCTION_MODE=true`
- `WORDS_WITH_SPOUSES_DATABASE_PATH=/data/words-with-spouses.db`
- `WORDS_WITH_SPOUSES_PUBLIC_BASE_URL=https://wwmtf.hyperchad.dev`
- `WORDS_WITH_SPOUSES_DEV_MODE` absent or false

## Launch checks

After bootstrap, manually verify registration, login, invitation redemption, two-player live updates, a persisted turn, and persistence across a redeploy. Confirm Cloudflare does not cache application responses and logs do not expose invitation, session, CSRF, rack, bag, or canonical-event secrets.

The direct `<app>.fly.dev` origin remains reachable. Cloudflare controls are defense in depth, not the application's only authentication or authorization boundary.
