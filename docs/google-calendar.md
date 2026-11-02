# Sign in with Google (calendar)

Stenly can read Google Calendar directly, with no Calendar.app, Kody or ICS link. It uses
the OAuth 2.0 installed-app flow: PKCE (S256), a loopback redirect
(`http://127.0.0.1:<random port>/callback`), and the single scope
`https://www.googleapis.com/auth/calendar.readonly` (read-only; it can list calendars and read
events, guests and conference data, and can change nothing).

- Tokens are kept in the login Keychain (item `ai.stenly.google-calendar`, one per profile; a
  `0600` file under `secrets/` on Linux). They never appear in `config.toml` or logs.
- Access tokens are refreshed automatically; a revoked or expired grant shows "Sign-in expired.
  Sign in again" on the account's row instead of failing silently.
- **Multiple accounts are supported.** Each account's calendars are listed with on/off
  switches. Selections are stored as `calendar.google_calendar_ids = ["account|calendar id"]`
  (empty = each account's primary calendar).
- Events flow into the same scheduler and filters as EventKit and ICS, and `stenly-cli
  upcoming` and the MCP `upcoming_meetings` tool see them too:
  `stenly-cli google-login | google-logout [email] | google-calendars`.
- Meeting links come from `conferenceData` (Meet, and the Zoom and Teams add-ons), then
  `hangoutLink`, `location` and `description`.
- Free/busy-only calendars are hidden (their events can't be read).
- Code: `crates/stenly-core/src/google.rs`. A Microsoft Graph sibling module can follow the
  same shape (`Credentials`, `Endpoints`, `TokenStore`, `Client`, an event mapper).

## Setting up the Google OAuth client (maintainer, once)

A build only shows the "Sign in with Google" buttons if it was built with a client ID. Without
one nothing else changes.

1. Open <https://console.cloud.google.com/> and create a project (for example "Stenly").
2. **APIs & Services → Library**, search for **Google Calendar API**, click **Enable**.
3. **APIs & Services → OAuth consent screen** (called **Google Auth Platform → Branding** in
   the newer console):
   - User type **External**, then Create.
   - App name `Stenly`, a support email, the app logo (optional but needed for verification),
     the app home page (the Stenly site), privacy policy link and terms link on the same
     domain, and a developer contact email.
   - **Authorized domains:** the domain of the home page and privacy policy.
   - **Data Access / Scopes → Add or remove scopes:** add
     `https://www.googleapis.com/auth/calendar.readonly` (listed as "See and download any
     calendar you can access using your Google Calendar"). That's the only scope.
   - **Audience / Test users:** while the app is in **Testing**, add the Google accounts
     that may sign in (up to 100). Refresh tokens issued to a Testing app **expire after 7
     days**, so testers must sign in again weekly until the app is published.
4. **APIs & Services → Credentials → Create credentials → OAuth client ID**:
   - Application type **Desktop app**, name `Stenly desktop`.
   - Copy the **Client ID** (`…apps.googleusercontent.com`) and the **Client secret**.
     Loopback redirects (`127.0.0.1` with any port) are allowed for Desktop clients; there is
     no redirect URI to register.
5. Give the values to the release build (they are baked in at build time and must not be
   committed):
   - GitHub → wbunting/stenly → Settings → Secrets and variables → Actions → **Variables**:
     `STENLY_GOOGLE_CLIENT_ID` = the client ID.
   - **Secrets**: `STENLY_GOOGLE_CLIENT_SECRET` = the client secret.
   - The release workflow passes both to `scripts/bundle-macos.sh`.
   - For a local build: `STENLY_GOOGLE_CLIENT_ID=… STENLY_GOOGLE_CLIENT_SECRET=… cargo build
     --release -p stenly -p stenly-cli`. At run time the same two variables override the
     built-in values (development only).
   - A desktop client's "secret" isn't confidential (Google documents this: it can be
     extracted from any installed app), which is why PKCE does the real protection. It is
     kept out of the repository anyway.
6. Try it with your own Google account (listed as a test user): Settings → Calendar → Sign in
   with Google. Google shows an "unverified app" warning while the app is in Testing:
   **Advanced → Go to Stenly (unsafe)**.

### Verification (needed before general release)

`calendar.readonly` is a **sensitive scope**. While the app is unverified:

- Users see a "Google hasn't verified this app" warning,
- the app is capped at **100 users** (counted over its lifetime, not reset), and
- Testing-mode refresh tokens expire after 7 days.

To go beyond that, set the consent screen to **In production** and submit it for
**verification** (Google Auth Platform → Verification Center). Expect to provide: the
verified domain, a privacy policy that states what calendar data is read and that it never
leaves the device except to the user's chosen summary provider, a **demo video** of the OAuth
flow showing the consent screen with the client ID and how the data is used, and a written
justification for why `calendar.readonly` (not `calendar.events.readonly`) is needed: Stenly
must list the user's calendars so they can choose which ones to watch, and
`calendar.events.readonly` can't list calendars. Review of a sensitive scope commonly takes
**days to several weeks**, with back-and-forth, so **start it early**. It isn't a security
assessment (that is for *restricted* scopes); calendar read access is not restricted.

## Testing without Google

`cargo test -p stenly-core --test google_mock` runs the whole flow against a local mock OAuth
and Calendar server (`crates/stenly-core/tests/support/mock_google.rs`): PKCE verification,
token refresh, revoked grants, two accounts, paging and unreadable calendars.

To drive the real app against it (synthetic data, isolated profile):

```sh
cargo run -p stenly-core --example mock-google      # prints the three variables below
export STENLY_GOOGLE_MOCK=http://127.0.0.1:<port>   # loopback only; anything else is ignored
export STENLY_GOOGLE_CLIENT_ID=mock-client-id.apps.example.com
export STENLY_GOOGLE_CLIENT_SECRET=mock-client-secret
export STENLY_CONFIG=/tmp/stenly-test/config.toml STENLY_DATA_DIR=/tmp/stenly-test/data
cargo run -p stenly
# add STENLY_GOOGLE_MOCK_AUTO=1 STENLY_TEST_GOOGLE=2 to sign in twice without a browser
```
