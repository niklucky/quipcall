# Deploy

Alpha runs on one VPS (`root@testron.dev`, Debian 13, shared with other projects) at **https://quipcall.com**.

```sh
make deploy        # server + web client + VPS config
make release-mac   # build the Mac app and publish it at https://quipcall.com/download/
make mac-app       # build only → target/mac/
```

Set `QUIPCALL_HOST=user@host` to deploy somewhere else.

## What runs where

| | |
|---|---|
| `https://quipcall.com/` | web client (`/opt/quipcall/web`, static) |
| `/api/` | `quipcall-server` on `127.0.0.1:4600` |
| `/rtc` | LiveKit signalling (WebSocket) on `127.0.0.1:7880` |
| `/download/` | Mac app, install script, instructions page (`/opt/quipcall/download`) |
| UDP `7882` | LiveKit media, all participants on one port |
| TCP `7881` | LiveKit media over TCP, for networks that block UDP |

- nginx terminates TLS: site `/etc/nginx/sites-available/quipcall.com.conf` (source: `deploy/nginx/`). `www` redirects to the bare domain.
- Let's Encrypt certificate via certbot's webroot (`/var/www/quipcall-acme`), renewed by `certbot.timer`. A deploy hook already on the box reloads nginx after renewals.
- systemd units `quipcall-livekit` and `quipcall-server`, both running as the `quipcall` user.
- `/opt/quipcall/secrets.env` (root, 0600) holds the LiveKit API key and secret. `install.sh` generates it on the first deploy and it never leaves the box. It also holds the auth settings below. Rotate LiveKit keys in place; do not delete the email configuration.
- No host firewall. LiveKit binds only to `eth0` (public IPv4 + IPv6), not to the Docker bridges.

## How `make deploy` works

`deploy/deploy.sh` builds locally and runs `deploy/install.sh` on the box:

1. The server, cross-compiled to a static Linux binary with `cargo zigbuild` (`x86_64-unknown-linux-musl`; Zig compiles bundled SQLite and TLS C code), and the web client (`pnpm build`). Install `zig` and `cargo-zigbuild` on the build Mac first.
2. One tarball over SSH to `/opt/quipcall/incoming`, removed afterwards.
3. `install.sh` (idempotent): user and directories, LiveKit at the pinned version, keys, configs, units, web files, nginx site, then health checks (`/api/health` through nginx, LiveKit on 7880).

Guard rails, because the box is shared:
- LiveKit restarts only when its binary, config or unit changed. A restart drops every call in progress. The API server always restarts; that takes milliseconds and keeps calls alive.
- If `nginx -t` rejects the new site, the previous one is restored, so other sites keep working and future reloads don't fail.
- Only quipcall files are touched: the site file, two units, `/opt/quipcall`, `/var/lib/quipcall`, `/var/www/quipcall-acme`.

## Mac app

`deploy/mac/package.sh` builds one app per architecture (Apple Silicon and Intel), signs them and zips them. Two slices in one binary would be about 115 MB, well over the 60 MB bundle budget. Release builds have `https://quipcall.com` baked in (`QUIPCALL_SERVER` at build time), and `MACOSX_DEPLOYMENT_TARGET=12.3`, the release ScreenCaptureKit appeared in. Below 10.14.4 the linker would also point the Swift overlay `libswiftCoreMedia` at `@rpath`, and the Intel app wouldn't launch. The script refuses to package a binary that links anything outside macOS.

Friends install with `curl -fsSL https://quipcall.com/download/install.sh | sh`, which picks the right architecture. A file downloaded with curl isn't quarantined, so Gatekeeper doesn't block the unnotarized app. Downloading the zip in a browser works too, but needs System Settings → Privacy & Security → Open Anyway once.

macOS remembers the app's permissions (microphone, camera, Screen Recording) by its signature. Ad-hoc signatures tied them to one exact build, so every update lost them, while System Settings went on showing the old build as allowed. Releases are now signed with a self-signed certificate that stays the same across builds. `deploy/mac/signing.sh` makes it on the first `make mac-app`, in `~/Library/Keychains/quipcall-signing.keychain-db` (no password, not in the keychain search list). **Back that file up**: a new certificate means everyone grants permissions once more. The first release signed this way also needs that once, coming from ad-hoc. The certificate isn't a Developer ID, so a zip downloaded in a browser still needs one manual "Open"; notarization would fix that (Stage 7).

For a release from uncommitted work, set `QUIPCALL_BUILD` to a positive integer greater than the currently published build (check `/download/version.txt`). Otherwise the build number defaults to the Git commit count.

The app logs to `~/Library/Logs/Quipcall/quipcall.log` when started from Finder (the previous run is kept as `quipcall.previous.log`).

## Logs and status

```sh
ssh root@testron.dev journalctl -u quipcall-server -u quipcall-livekit -f
ssh root@testron.dev systemctl status quipcall-livekit quipcall-server
```

## Invite-only authentication

Keep local credentials in ignored `deploy/auth.env.local` (mode 0600), using [deploy/auth.env.example](../deploy/auth.env.example) as the template. Add those values to `/opt/quipcall/secrets.env` before deploying:

- `RESEND_API_KEY`: a Resend key with permission to send emails.
- `QUIPCALL_MAIL_FROM`: sender on a domain verified in Resend. Disable email click tracking for sign-in messages.
- `QUIPCALL_BOOTSTRAP_EMAIL`: your email. Verify it through the normal magic-link flow to create the first admin account; only admins issue account registration invitations.

The installer checks these settings before replacing the running application. It never writes real Resend credentials. No deployment is needed to run tests.

The systemd unit sets the public origin to `https://quipcall.com`, creates a private state directory, and stores SQLite at `/var/lib/quipcall/quipcall.db`. Back this database up using SQLite's online backup command (or stop the API before copying it). It contains accounts, sessions, invitations, rooms and memberships. Restarting the API preserves access. Migrations run at startup. The API checks readiness by opening and migrating the database before binding its port.

Sign in on the web, then use **Invite a new account** to make a single-use, email-bound registration link. Share the link yourself. It expires after seven days. The recipient enters the invited email and verifies it through Resend. Existing accounts only need an email sign-in link. Magic links expire after 15 minutes and are consumed only by an explicit confirmation POST, so email-link scanners don't consume them. Credential hashes, rather than bearer credentials, are stored in SQLite.

Browser sessions use HttpOnly, Secure, SameSite=Lax cookies on production. Native sessions use macOS Keychain, scoped to the server and (for tests) `QUIPCALL_DATA_DIR`. Desktop sign-in opens the browser and requires typing the eight-character code shown by that Mac. The code is absent from the URL, stored hashed on the server, and limited to five approval attempts. Pending sign-in can be cancelled immediately. HTTP 401 clears the native credential so the next call asks for sign-in. The app exchanges a separate private polling secret for a session; no session credential is passed through a custom URL scheme. User sessions last 30 days, guest browser/device sessions one year. A room membership persists independently of session expiry; a guest who loses their credential needs a fresh invitation. Apple and other OAuth providers are deferred; the `identities` table already separates provider identity from the internal account ID.

## Calls and access

- A signed-in user creates rooms. A **guest 1-to-1** reserves one identity for the creator and one for the first guest; the same guest can reconnect, and a third principal cannot join. Removing the guest frees the slot for a replacement while keeping the removed principal blocked.
- A **public room** admits anyone with its unguessable link, including guests.
- A **private room** admits its owner, invited registered members and explicitly invited guests. Account registration and room membership are separate permissions.
- A **single-use guest invitation** can be claimed by one identity; that identity can leave and return until manually removed. The unclaimed link expires after seven days, but claimed membership and the original return link have no time limit. Acceptance is atomic. A lost HTTP response can be retried by the same identity, but a consumed link never restores removed access.
- Only the room owner can issue/revoke room invitations, remove access, rename or close the room. Membership invitations are bound to an account email. Guests cannot create rooms or issue invitations.
- **Remove access** commits revocation before requesting LiveKit disconnection. If disconnection fails, the API reports failure and the UI offers a retry; access remains revoked. Closing a room works the same way.

LiveKit signaling **must** pass through nginx's `auth_request` gate on `/rtc` (including its subpaths). Each join has a distinct signed LiveKit identity (`principal:connection`) so one account can use multiple clients. Removal enumerates and disconnects every connection for that principal, including legacy identities. The gate validates the signed LiveKit token and checks current room access before every handshake, including refreshed tokens. The LiveKit HTTP/signaling port remains bound to loopback. This is required for manual removal: self-hosted LiveKit alone does not reliably revoke a previously issued token. Do not expose port 7880 directly, cache authorization responses, or remove this gate. nginx must include `http_auth_request_module`; the installer runs `nginx -t` before reload. Signaling access logs are disabled because URLs contain tokens.

The old unauthenticated GET `/api/join` is removed. Unknown room IDs no longer create rooms. Existing alpha room bookmarks are not automatically claimed by whoever presents them; create replacement rooms while signed in. Ship the updated desktop app with the backend/web update; old clients cannot authenticate. Current call history and recordings remain local and aren't exposed by a room invitation. Desktop **+** creates public/private rooms. **Access** on Home and **Invite** during a call manage guest/member links, pending invitations, member removal, renaming and closing. **Refresh rooms** syncs the native list. Registration invitations remain in the web admin controls.

## Local auth development

`make dev` uses `dev/dev.env`: SQLite under `dev/data`, bootstrap email `admin@example.test`, and a private-file outbox in `dev/data/mail`. Request a magic link from the local web UI, then open the URL from the outbox file. Nothing is sent to Resend in this mode. The outbox is explicitly restricted to localhost and never logged. Real deployment must use Resend.

For an isolated second stack, source `dev/isolated-env.sh` **once** from the repository root, then launch all processes from that shell (or pass its exported values to each terminal):

```sh
source dev/isolated-env.sh
livekit-server --config "$QUIPCALL_LIVEKIT_CONFIG" > "$QUIPCALL_TEST_DIR/livekit.log" 2>&1 &
cargo run -p quipcall-server > "$QUIPCALL_TEST_DIR/server.log" 2>&1 &
(cd apps/web && pnpm dev --port 5273 --strictPort) > "$QUIPCALL_TEST_DIR/web.log" 2>&1 &
# Native clients also need the web origin for browser approval:
MACOSX_DEPLOYMENT_TARGET=12.3 cargo run -p quipcall-desktop -- --server "$QUIPCALL_API" --web "$QUIPCALL_PUBLIC_URL"
```

The helper creates fresh SQLite/outbox/native state under `$QUIPCALL_TEST_DIR`, uses server 8180, Vite 5273 with a matching allowed Origin, and LiveKit 7980/7981/7982. Do not use ordinary `make dev` for this stack: its included dev.env points at the user's shared development state. Keep track of your own process IDs when stopping the stack. Authenticate with `admin@example.test` via the isolated outbox, create a public room, then use that room ID for a headless participant. `?room=<id>&autojoin=1&mic=0` works only in local Vite development mode and still obeys server authorization; production always shows the joining screen. Use Chrome's fake-media flags from CLAUDE.md.

To test token revocation end to end, put a local nginx auth-request gateway in front of LiveKit and use its URL as `LIVEKIT_PUBLIC_URL`. The minimal development stack connects directly to LiveKit and therefore does not enforce the signaling gate; it still enforces API admission.

Verification: `cargo test -p quipcall-server` covers signup gating, email confirmation/reuse/expiry, session revocation, role enforcement, simultaneous invite/direct-slot claims, private access persistence/removal, cached-token authorization, desktop proof and CSRF/rate limits. `make check` checks Rust and Svelte; `pnpm build` in `apps/web` checks the production bundle.

Native room checks: `cargo test -p quipcall-desktop rooms::tests` covers creation responses, retaining issued links when refresh fails, and revocation/disconnection partial failure. With the `snapshot` feature, `room-new`, `room-create-private`, `room-create-public`, `room-manage`, `room-guest`, `room-member`, `room-refresh`, `room-remove`, `room-retry-remove`, `room-revoke` and `room-close` exercise the native controls against an isolated API. `QUIPCALL_TEST_SESSION_FILE` supplies a fixture credential only on localhost; `QUIPCALL_ROOM_TEST_STATE` exports the result for assertions. These hooks are absent from release packages. Always use an isolated `QUIPCALL_DATA_DIR`, `QUIPCALL_NO_MIC=1`, `QUIPCALL_FAKE_CAMERA=1` and `QUIPCALL_FAKE_SCREEN=1`.

Join preflight: `POST /api/entry` accepts exactly one of `room` or `invite`. It returns only permitted room metadata and an access state, never consumes invitations or reserves a direct-call slot, and never mints a media token. Ship its server change with the web join page. Mac build 56 adds `quipcall://open/` for the explicit browser-identity handoff. `pnpm test` in `apps/web` (Node 22.18+ or 24+) checks media request cancellation, device changes, cleanup and permission denial. API tests check preflight privacy and unchanged admission rules.

Desktop sign-in polling uses a separate IP allowance from email/approval requests, plus a per-challenge quota after verifying the private proof. The app backs off on HTTP 429 within its original sign-in deadline. Cached room lists are bound to the principal and cleared before a new account refresh, including when that refresh fails. Browser join continuations are cancelled when navigating away, so late identity/redemption responses cannot trigger another join or approval.

Auth requests are limited per IP; unauthenticated requests cannot consume a shared global email, guest, verification or desktop-start budget. Email requests enter a durable SQLite queue before any account lookup, and delivery runs separately. Each IP can request at most three messages per eligible recipient per 15-minute window, so another IP cannot spend the recipient’s allowance; excess requests return the same success response and preserve every already-delivered link. Successful verification resets mail suppression, and failed delivery does not consume it. Delivery failures are logged server-side without changing the public response.

Migration 0002 adds queued mail and device-code approval. Deploy updated web, API and Mac clients together; existing pending challenges and older Mac builds cannot complete code approval. Existing signed-in sessions remain valid.
