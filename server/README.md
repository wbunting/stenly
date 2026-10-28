# Stenly share server

`server/stenly-share` is the server behind Stenly's private share links. The same Docker image runs the free hosted default (`share.stenly.ai`) and company self-hosting.

Notes are **end-to-end encrypted**. The Stenly app encrypts each note on your computer with a fresh 256-bit key and uploads only the ciphertext. The key travels in the link after the `#` (`https://share.example.com/s/<id>#<key>`). Browsers never send that part to a server. The viewer page decrypts the note in the reader's browser with WebCrypto.

- [Run it](#run-it)
- [Configuration](#configuration)
- [Company mode (OIDC sign-in, admin)](#company-mode-oidc-sign-in-admin)
- [Point the desktop app at it](#point-the-desktop-app-at-it)
- [Security model](#security-model)
- [Backups and upgrades](#backups-and-upgrades)
- [API (contract v1)](#api-contract-v1)
- [Development](#development)

## Run it

### Build the image

The server is written in Rust on [hearth](https://usehearth.dev) ([source](https://github.com/wbunting/hearth)). The `hearth` crate and the `hearth-cli` asset bundler are published on crates.io at the versions pinned in `Cargo.toml` and the Dockerfile, so no credentials are needed:

```sh
cd server/stenly-share
docker build -t stenly-share .
```

The final image is Debian slim plus one static-ish binary and its CSS/JS/font bundle (about 100 MB, most of it the Debian base).

### Single container

```sh
docker run -d --name stenly-share -p 8080:8080 \
  -e STENLY_SHARE_BASE_URL=https://share.example.com \
  -v stenly-share-data:/data \
  stenly-share
curl http://localhost:8080/api/v1/info
```

Or use [`deploy/docker-compose.yml`](stenly-share/deploy/docker-compose.yml).

**Use HTTPS in production.** Browsers only expose WebCrypto on secure pages (`https://`, or `http://localhost`). Over plain HTTP the viewer shows "This page needs HTTPS".

### Behind Caddy with automatic TLS

[`deploy/docker-compose.caddy.yml`](stenly-share/deploy/docker-compose.caddy.yml) runs the server behind Caddy, which gets a Let's Encrypt certificate:

```sh
cd server/stenly-share/deploy
SHARE_DOMAIN=share.example.com docker compose -f docker-compose.caddy.yml up -d
```

Point the domain's DNS at the host and open ports 80 and 443 first.

### Without Docker

```sh
cd server/stenly-share                 # rust-toolchain.toml pins rustc 1.99.1
cargo install --locked hearth-cli --version =0.1.0
cargo build --release && hearth asset bundle --release --bin stenly-share
STENLY_SHARE_DATA_DIR=/var/lib/stenly-share PORT=8080 HOST=0.0.0.0 target/release/stenly-share
```

The binary looks for its asset bundle in `assets/` next to itself, or in `STENLY_SHARE_ASSETS_DIR`. Deploy the two together, because a bundle only matches the binary it was built with.

## Configuration

Everything is set with environment variables.

| Variable | Default | Meaning |
|---|---|---|
| `STENLY_SHARE_BASE_URL` | request `Host` | Public origin used to build share URLs, e.g. `https://share.example.com`. **Set this in production.** |
| `STENLY_SHARE_DATA_DIR` | `./data` (`/data` in Docker) | Where the SQLite database lives. Mount a volume here. |
| `STENLY_SHARE_MAX_BYTES` | `2097152` (2 MiB) | Largest encrypted note accepted. Bigger uploads get `413`. |
| `STENLY_SHARE_DEFAULT_TTL_DAYS` | `90` | Expiry when the client doesn't ask for one. |
| `STENLY_SHARE_MAX_TTL_DAYS` | `365` | Longer requested expiries are clamped to this. |
| `STENLY_SHARE_UPLOAD_TOKEN` | unset | If set, creating or replacing a share needs `Authorization: Bearer <token>` (otherwise `401`). Reading shares never needs it. |
| `STENLY_SHARE_WRITE_RATE_PER_MIN` | `30` | Creates, replaces and revokes per client IP per minute (`0` turns the limit off). Excess requests get `429` with `Retry-After`. |
| `STENLY_SHARE_READ_RATE_PER_MIN` | `300` | Viewer and ciphertext fetches per client IP per minute. |
| `STENLY_SHARE_AUTH_RATE_PER_MIN` | `30` | Company mode: sign-ins started per client IP per minute (`0` turns the limit off). An office behind one NAT IP shares this limit, so raise it there, or set `STENLY_SHARE_TRUSTED_PROXIES` when behind a proxy. |
| `STENLY_SHARE_TRUSTED_PROXIES` | unset | Comma-separated CIDRs of your reverse proxies. Their `X-Forwarded-For` is trusted for the client IP used in rate limiting. Without it, every request behind a proxy counts as the proxy's IP. |
| `STENLY_SHARE_TOMBSTONE_DAYS` | `30` | How long an expired or revoked share keeps answering `410` before the server forgets it (then `404`). |
| `STENLY_SHARE_SWEEP_SECS` | `600` | How often expired ciphertext is deleted. |
| `STENLY_SHARE_ASSETS_DIR` | `assets/` next to the binary | The CSS/JS/font bundle. |
| `HOST`, `PORT` | `127.0.0.1`, `3000` (`0.0.0.0`, `8080` in Docker) | Listen address. |

| `STENLY_SHARE_OIDC_ISSUER`, `…_CLIENT_ID`, `…_CLIENT_SECRET` | unset | Turns on [company mode](#company-mode-oidc-sign-in-admin). Set all three, or none. |
| `STENLY_SHARE_ALLOWED_EMAIL_DOMAINS` | unset (any) | Company mode: comma-separated domains allowed to sign in. Admins can change this in `/admin`. |
| `STENLY_SHARE_ADMINS` | unset | Company mode: comma-separated emails that are always admins. The first user to sign in also becomes an admin. |
| `STENLY_SHARE_UPLOAD_AUTH` | `token` | Company mode: `token` accepts the shared upload token or a personal one. `user` accepts personal tokens only, so every share has an owner. |

`GET /healthz` returns `200 ok` when the database is reachable, and the Docker image's `HEALTHCHECK` uses it.

## Company mode (OIDC sign-in, admin)

Set the three `STENLY_SHARE_OIDC_*` variables (plus `STENLY_SHARE_BASE_URL`) to keep shared notes inside a company:

- **Viewing needs sign-in.** Opening `/s/{id}` or fetching ciphertext from `GET /api/v1/shares/{id}` needs a signed-in user whose email domain is allowed. Anyone else gets a "Sign in to view this note" page, or `401 {"error":"sign_in_required"}` from the API.
  - The key still never reaches the server. Identity-provider redirects drop the URL fragment, so the viewer keeps the key in the tab's `sessionStorage` during sign-in and puts it back afterwards.
- **Uploads** need the shared `STENLY_SHARE_UPLOAD_TOKEN` or a **personal upload token**. Signed-in users create personal tokens at `/account`. With `STENLY_SHARE_UPLOAD_AUTH=user`, only personal tokens are accepted, and each share records who uploaded it.
- **`/admin`** is for admins: the first user to sign in, plus anyone in `STENLY_SHARE_ADMINS`. It shows:
  - stats: active and total shares, views, stored bytes, users, and shares and views per day for the last 14 days;
  - a **live list of shares** with id, the opt-in plaintext title, owner, created, expires, size, views and status, plus **Revoke** and **+30 days**;
  - **server settings**: default expiry, max size and allowed domains. Saved settings override the environment and apply immediately. Removing a domain signs its users out.

  The admin never sees note contents, only the metadata listed under the [security model](#security-model). The share list is a hearth synced table: the browser keeps a local copy, filters it instantly, and applies revoke and extend optimistically, with every write validated on the server.
- `/api/v1/info` reports `"auth":"oidc"`.

**Setting up the identity provider.** Create an OIDC "web application" client with the redirect URI `https://<your server>/auth/callback` and the scopes `openid email profile`. Any standards-compliant provider works (Google Workspace, Microsoft Entra ID, Okta, Keycloak, Authentik, …). ID tokens are verified against the provider's JWKS (RS256 or ES256) and checked for issuer, audience, expiry and nonce. The flow uses PKCE.

```sh
STENLY_SHARE_BASE_URL=https://share.example.com
STENLY_SHARE_OIDC_ISSUER=https://accounts.google.com
STENLY_SHARE_OIDC_CLIENT_ID=…apps.googleusercontent.com
STENLY_SHARE_OIDC_CLIENT_SECRET=…
STENLY_SHARE_ALLOWED_EMAIL_DOMAINS=example.com
STENLY_SHARE_ADMINS=it@example.com
```

**Trying it locally** with the bundled test provider (**not for production**):

```sh
cargo run --bin dev-idp &           # OIDC provider on http://127.0.0.1:8790; type any email
STENLY_SHARE_BASE_URL=http://localhost:8788 PORT=8788 STENLY_SHARE_DATA_DIR=/tmp/ss \
STENLY_SHARE_OIDC_ISSUER=http://127.0.0.1:8790 STENLY_SHARE_OIDC_CLIENT_ID=stenly-share \
STENLY_SHARE_OIDC_CLIENT_SECRET=dev-secret STENLY_SHARE_ALLOWED_EMAIL_DOMAINS=acme.test \
  cargo run --bin stenly-share
```

Use `http://localhost`, not `127.0.0.1`. Session cookies are `Secure` and `__Host-` prefixed, and browsers only send those over HTTPS or to `localhost`. In production, serve over HTTPS.

## Point the desktop app at it

In Stenly, open **Settings → Destinations → Add → Stenly share link** and set **Server URL** to your origin (e.g. `https://share.example.com`). Click **Test**. It calls `GET /api/v1/info` and should say "Connected to stenly-share". If you set `STENLY_SHARE_UPLOAD_TOKEN`, paste the same token as the destination's credential. The app stores it in the Keychain.

From the CLI, with a destination configured in `config.toml`:

```toml
[[destinations]]
id = "company-share"
name = "Company share links"
kind = "share"
[destinations.share]
server_url = "https://share.example.com"
include_transcript = true
plaintext_title = false     # opt in to a plaintext title for link previews
expiry_days = 90
```

```sh
stenly-cli destinations --test company-share
stenly-cli share latest --to company-share          # prints https://…/s/<id>#<key>
stenly-cli share latest --to company-share --revoke
```

## Security model

**What the server sees and stores, for each share:**
- the encrypted note (AES-256-GCM ciphertext, nonce and tag);
- its size, creation, update and expiry times, revocation time, and a view count;
- the SHA-256 hash of the owner token (never the token itself);
- **only if the sharer opted in**, a plaintext title, used for link previews.

It also sees ordinary request metadata: IP addresses, kept in memory for rate limiting only and never written to disk, plus whatever your proxy logs.

**What it never sees:** the key, and therefore the note's title, date, attendees, contents and transcript. The key is in the URL fragment, which browsers don't send in requests or `Referer` headers. The viewer also sends `Referrer-Policy: no-referrer`.

**Company mode** adds what the server needs for sign-in: user records (email and name), hashed session and personal-token records, and which user uploaded each share. It still never has a key.

**Viewer hardening.** The viewer page uses a strict Content-Security-Policy: same-origin scripts, styles, fonts and requests only. It has no inline scripts, no `eval` and no third-party code, and it can't be framed. The note's Markdown is rendered into DOM nodes as text, never as HTML. Links are limited to `http(s)` and `mailto`. The company-mode admin pages run the hearth runtime, which needs `'unsafe-eval'`. They get that looser policy only on `/admin` and `/account`, and those pages never see keys or decrypted notes.

**What this protects against.** A compromised or curious server operator, a database leak or a backup leak reveals no note contents. A stolen backup reveals only the metadata listed above.

**What it doesn't protect against.**
- Anyone holding the full link can read the note. Treat links like passwords.
- An operator who serves malicious JavaScript in the viewer could steal keys from the people who open links on that server. This is inherent to any web viewer that decrypts in the browser, and it's why companies may prefer to self-host. To audit the code, check `web/viewer.js` in this repo against what your server serves at `/_hearth/assets/viewer-*.js`.

**Revocation and expiry.** Revoking deletes the ciphertext immediately. Expired ciphertext is deleted by the sweeper within `STENLY_SHARE_SWEEP_SECS`. Both then answer `410` until the tombstone period ends. Each re-share of a meeting replaces the ciphertext in place and keeps the link and key. A new share gets a new key.

## Backups and upgrades

All state is one SQLite database, `stenly-share.db` (WAL mode), in `STENLY_SHARE_DATA_DIR`. Take a consistent online backup with:

```sh
docker exec stenly-share sh -c 'cd /data && tar -czf - stenly-share.db*' > stenly-share-$(date +%F).tgz
# or, with sqlite3 on the host and the volume mounted:
sqlite3 /path/to/data/stenly-share.db ".backup 'stenly-share-backup.db'"
```

Backups contain only ciphertext plus the metadata listed above.

**Upgrades:** pull or build the new image and restart the container on the same volume. Schema migrations are embedded in the binary (`migrations/`). They run automatically at startup, and migrations that were already applied are skipped. Back up before upgrading. Downgrading after a migration is not supported.

## API (contract v1)

JSON in and out, with errors as `{"error":"<code>","message":"…"}`. Every timestamp is RFC 3339 UTC.

| Request | Response |
|---|---|
| `GET /api/v1/info` | `200 {"name":"stenly-share","version","max_bytes","default_ttl_days","upload_token_required","auth":"none"}` |
| `POST /api/v1/shares` `{"ciphertext","expires_in_days"?,"title"?}` | `201 {"id","url","owner_token","expires_at"}`. `url` has no fragment. Needs `Authorization: Bearer` if an upload token is configured. |
| `PUT /api/v1/shares/{id}` + `X-Stenly-Owner-Token`, same body | `200`, same shape, same `owner_token`. Without `expires_in_days` the current expiry is kept. |
| `DELETE /api/v1/shares/{id}` + `X-Stenly-Owner-Token` | `204` (repeat revokes are idempotent) |
| `GET /api/v1/shares/{id}` | `200 {"ciphertext","title"?,"created_at","expires_at"}`, `404` unknown, `410` revoked or expired (`error` is `revoked` or `expired`) |
| `GET /s/{id}` | the viewer (`404`/`410` pages for unknown, revoked and expired shares) |
| `GET /healthz` | `200 ok` |

Status codes: `400` malformed (bad JSON or base64, an envelope shorter than 29 bytes, version ≠ 1, `expires_in_days` < 1), `401` missing or wrong upload token or missing owner token, `403` wrong owner token, `404`, `410`, `413` over `max_bytes`, `429` rate limited.

The envelope is `[0x01][12-byte nonce][AES-256-GCM ciphertext || tag]`, with AAD `stenly-share-v1` and base64url without padding. `id` is 16 random bytes and `owner_token` is 32 random bytes, both base64url.

## Development

```sh
cd server/stenly-share
cargo build && hearth asset bundle --bin stenly-share   # hearth-cli 0.1.0
STENLY_SHARE_DATA_DIR=/tmp/ss PORT=8788 target/debug/stenly-share
node scripts/share-note.mjs http://127.0.0.1:8788      # encrypts a sample note, prints url#key
```

Tests:

```sh
cargo test                     # contract test vector, API + company-mode integration tests (needs the asset bundle: `hearth asset bundle --bin stenly-share`)
node --test web/viewer.test.mjs  # browser-side decrypt of the test vector (WebCrypto), Markdown renderer
```

To work against a local hearth checkout, add a patch to `Cargo.toml` and don't commit it:

```toml
[patch.crates-io]
hearth = { path = "../../../hearth/crates/hearth" }
```

### Layout

| Path | |
|---|---|
| `src/app.rs` | router, security headers, viewer page (`/s/{id}`) |
| `src/app/v1.rs` | the `/api/v1` routes |
| `src/api.rs` | contract errors, upload parsing, auth headers, per-IP rate limiter |
| `src/store.rs`, `src/model.rs` | Toasty models, SQLite, embedded migrations, expiry sweeper |
| `src/share.rs` | ids, tokens, envelope checks (and the contract test vector) |
| `src/auth.rs`, `src/oidc.rs`, `src/app/signin.rs` | company mode: sessions, guards, OIDC (PKCE, JWKS verification) |
| `src/tables.rs`, `src/app/admin.rs` | the `/admin` hearth synced tables, live share list, settings, stats, upload tokens |
| `src/policy.rs` | admin-editable settings (default TTL, max size, allowed domains) |
| `src/dev_idp.rs`, `src/bin/dev-idp.rs` | local test OIDC provider (tests and development only) |
| `web/viewer.js` | the viewer: WebCrypto decrypt and a safe Markdown renderer (also a test module) |
| `src/styles.css` | Tailwind entry point with the site's paper-and-ink styles |
