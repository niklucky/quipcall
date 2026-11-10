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
- `/opt/quipcall/secrets.env` (root, 0600) holds the LiveKit API key and secret. `install.sh` generates it on the first deploy and it never leaves the box. To rotate: delete it and deploy again.
- No host firewall. LiveKit binds only to `eth0` (public IPv4 + IPv6), not to the Docker bridges.

## How `make deploy` works

`deploy/deploy.sh` builds locally and runs `deploy/install.sh` on the box:

1. The server, cross-compiled to a static Linux binary (`x86_64-unknown-linux-musl`, linked by Rust's bundled `rust-lld`, so no Docker or C toolchain), and the web client (`pnpm build`).
2. One tarball over SSH to `/opt/quipcall/incoming`, removed afterwards.
3. `install.sh` (idempotent): user and directories, LiveKit at the pinned version, keys, configs, units, web files, nginx site, then health checks (`/api/health` through nginx, LiveKit on 7880).

Guard rails, because the box is shared:
- LiveKit restarts only when its binary, config or unit changed. A restart drops every call in progress. The API server always restarts; that takes milliseconds and keeps calls alive.
- If `nginx -t` rejects the new site, the previous one is restored, so other sites keep working and future reloads don't fail.
- Only quipcall files are touched: the site file, two units, `/opt/quipcall`, `/var/www/quipcall-acme`.

## Mac app

`deploy/mac/package.sh` builds one app per architecture (Apple Silicon and Intel), signs them and zips them. Two slices in one binary would be about 115 MB, well over the 60 MB bundle budget. Release builds have `https://quipcall.com` baked in (`QUIPCALL_SERVER` at build time), and `MACOSX_DEPLOYMENT_TARGET=12.3`, the release ScreenCaptureKit appeared in. Below 10.14.4 the linker would also point the Swift overlay `libswiftCoreMedia` at `@rpath`, and the Intel app wouldn't launch. The script refuses to package a binary that links anything outside macOS.

Friends install with `curl -fsSL https://quipcall.com/download/install.sh | sh`, which picks the right architecture. A file downloaded with curl isn't quarantined, so Gatekeeper doesn't block the unnotarized app. Downloading the zip in a browser works too, but needs System Settings → Privacy & Security → Open Anyway once.

macOS remembers the app's permissions (microphone, camera, Screen Recording) by its signature. Ad-hoc signatures tied them to one exact build, so every update lost them, while System Settings went on showing the old build as allowed. Releases are now signed with a self-signed certificate that stays the same across builds. `deploy/mac/signing.sh` makes it on the first `make mac-app`, in `~/Library/Keychains/quipcall-signing.keychain-db` (no password, not in the keychain search list). **Back that file up**: a new certificate means everyone grants permissions once more. The first release signed this way also needs that once, coming from ad-hoc. The certificate isn't a Developer ID, so a zip downloaded in a browser still needs one manual "Open"; notarization would fix that (Stage 7).

The app logs to `~/Library/Logs/Quipcall/quipcall.log` when started from Finder (the previous run is kept as `quipcall.previous.log`).

## Logs and status

```sh
ssh root@testron.dev journalctl -u quipcall-server -u quipcall-livekit -f
ssh root@testron.dev systemctl status quipcall-livekit quipcall-server
```

## Rooms are open

There are no accounts yet (Stage 3). `/api/join` gives a token for any room to anyone, like a meeting link: knowing the room ID is the only key. Room IDs from the desktop app have a random suffix (`quick-1a2b3c4d`). Fine for an alpha among friends. Invite codes and accounts close it.
