# Quipcall — Implementation Plan

How to read this file:

- A finished task gets `[x]` and ~~strikethrough~~.
- A finished stage gets its heading struck through and a ✅.
- Each stage ends with **Done when**: the checks that have to pass before we cross it out.
- Decisions and the reasoning behind them live in [ARCHITECTURE.md](ARCHITECTURE.md).

**Current focus:** Stage 1 — Media spike

---

## Footprint budget

These are targets. We measure them at the end of Stage 1 and at every milestone, and record the results in `docs/FOOTPRINT.md`.

| Metric | Target |
|---|---|
| macOS app bundle (without speech-to-text models) | ≤ 40 MB |
| Idle in the menu bar, logged in | ≤ 60 MB RAM, ~0% CPU |
| 1:1 call, 720p camera both ways | ≤ 200 MB RAM, ≤ 15% CPU on an M-series Mac |
| 10-person grid call | ≤ 350 MB RAM |
| Screen share, 1080p at 30 fps (sending) | ≤ 20% CPU |

---

## ~~Stage 0 — Foundations~~ ✅

- [x] ~~Git repo, Cargo workspace, `.gitignore`, README~~
- [x] ~~`docs/PLAN.md` and `docs/ARCHITECTURE.md`~~
- [x] ~~Local LiveKit dev server config (`dev/livekit.yaml`) and Makefile tasks~~
- [x] ~~`apps/server` skeleton: Axum, `/health`, and a dev-only LiveKit token endpoint~~
- [x] ~~`crates/protocol` with the first shared types~~

**Done when:** `make dev` starts LiveKit and the server, `cargo build --workspace` passes, and `curl` returns a token.

## Stage 1 — Media spike (riskiest part first)

- [x] ~~`crates/core`: room connection, events, and subscribing to remote tracks, with no UI code (own tokio runtime)~~
- [x] ~~Desktop: GPUI window that connects to a room with a dev token~~
- [x] ~~Desktop: render remote video. I420 → full-range NV12 `CVPixelBuffer` → GPUI `surface`~~
- [ ] Desktop: microphone and speaker via `PlatformAudio`, with echo cancellation, noise suppression and auto gain from WebRTC. *Wired up; needs a test where a person can answer the permission prompt.*
- [ ] Desktop: screen share via ScreenCaptureKit (system picker), with frame-rate presets 5/15/30/60 and a text/motion mode. *Implemented with libwebrtc's `DesktopCapturer`; needs a manual test (picker and Screen Recording permission).*
- [ ] Desktop: ask for mic and camera permission up front (AVCaptureDevice `requestAccess`) before opening the audio device. Otherwise a pending prompt blocks WebRTC's audio thread and freezes the call (found in testing).
- [x] ~~Desktop: pick the simulcast layer per tile from its on-screen size (the Rust SDK's adaptive stream doesn't report sizes)~~
- [ ] Desktop: camera capture and publish (AVFoundation)
- [ ] Desktop: local self-view for camera and screen share
- [x] ~~Web: minimal Svelte page that joins the same room with camera, mic, and screen share (with frame-rate picker)~~
- [ ] Zero-copy receive path for hardware-decoded frames (VideoToolbox `CVPixelBuffer` → full range on the GPU)
- [ ] Direct ScreenCaptureKit capture (`CVPixelBuffer` → `NativeBuffer` → VideoToolbox), if the libwebrtc capturer uses too much CPU
- [ ] Measure the footprint and write `docs/FOOTPRINT.md`. *First numbers recorded: 23.9 MB binary, ~110 MB / 8–9% CPU receiving 720p. Sending, screen share, and a 10-person grid still to measure.*

**Done when:** a 1:1 call between desktop and web works locally with audio, video, and screen share at a chosen frame rate. Footprint numbers are recorded, and we've made a go/no-go call on GPUI.

## Stage 2 — Backend core

- [ ] SQLite via sqlx, with migrations
- [ ] Accounts: invite codes, username + password (argon2), session tokens
- [ ] `protocol`: REST request/response types and WebSocket messages, with TypeScript types generated for web
- [ ] WebSocket hub: authenticated connection, presence (online / away / in call)
- [ ] LiveKit token minting for calls; LiveKit webhooks (joined / left / room finished) → call records
- [ ] Remove the dev-only token endpoint from non-dev builds

**Done when:** integration tests for auth, token minting, and WebSocket presence pass.

## Stage 3 — Desktop app shell

- [ ] Login, with the session stored in the macOS Keychain
- [ ] Menu-bar mode: runs in the background, stays connected for presence, small idle footprint
- [ ] `core`: call state machine with no UI code
- [ ] Contacts: add by username, see who's online
- [ ] Settings: mic, speaker, camera, default screen-share preset

**Done when:** the app starts in the menu bar, logs in, shows contacts with live presence, and meets the idle budget.

## Stage 4 — 1:1 calls

- [ ] Ringing protocol: invite → ringing → accept / decline / cancel / timeout / busy. With multiple devices, the first accept wins.
- [ ] Incoming-call window and ringtone
- [ ] In-call UI: mute, camera, screen share (frame-rate picker), device switching, hang up
- [ ] Call history

**Done when:** two desktops can call each other through the backend, and every ringing state is covered by tests.

## Stage 5 — Web client

- [ ] Login, contacts, place and answer calls from the browser
- [ ] Call UI on par with desktop: grid, controls, screen share with frame-rate picker
- [ ] Mobile layout (active speaker + up to 4 tiles), tested in Chrome on Android

**Done when:** calls work web ↔ desktop, including from an Android phone.

## Stage 6 — Alpha deploy to the VPS (friends start testing here)

- [ ] LiveKit on a single node: UDP port, built-in TURN/TLS for strict networks
- [ ] Caddy (HTTPS), systemd units, server, and static web files
- [ ] SQLite backups
- [ ] macOS build: `.app` bundle, signing and notarization (or ad-hoc signing for friends), simple update check
- [ ] Invite codes for friends

**Done when:** friends install the app, sign up, and call each other over the internet, including through strict NAT.

## Stage 7 — Group calls (up to ~10 people)

- [ ] Groups: create, add/remove members, owner role
- [ ] Start a group call (rings online members); join a call already in progress; show which groups are in a call
- [ ] Guest links: signed, expiring link → join on web with a display name and limited permissions; the host can remove guests
- [ ] Layouts: grid / speaker / screen-share focus; active speaker; pagination
- [ ] Performance: adaptive stream, dynacast, pause off-screen tiles, tuned simulcast layers

**Done when:** a 10-participant call with a mix of desktop, web, and bots stays smooth, and CPU is measured.

## Stage 8 — Transcription (single transcriber)

- [ ] `crates/stt`: VAD (Silero), a `Transcriber` trait, model manager (download on demand, checksum)
- [ ] Model bake-off for our languages (accuracy and speed on Apple Silicon); record the decision
- [ ] Transcriber role in the backend: claim, lease, and heartbeat; capability shown via participant attributes; handoff prompt
- [ ] Pipeline: local mic (before encoding) + each remote track → 16 kHz → VAD → shared ASR queue; partial and final results
- [ ] Transcript flow: transcriber → backend (store and order) → WebSocket to all participants; late joiners get the history
- [ ] Transcript panel (chat style) on desktop and web; "Transcribed by X" indicator
- [ ] Raise Opus bitrate while a transcription is running

**Done when:** a 1-hour mixed-language test call produces a transcript with correct speakers, and handing off the transcriber role works.

## Stage 9 — LLM summaries

- [ ] `Summarizer` trait: Claude API (`claude-haiku-5-5` / `claude-sonnet-5-5`), plus a local backend (llama.cpp-compatible)
- [ ] Summaries on demand and at the end of a call, stored in call history
- [ ] Prompt covers decisions, action items, and highlights per speaker, written in the transcript's language

**Done when:** a summary of a test call is generated and visible on both desktop and web.

## Later

- [ ] Windows desktop (Windows.Graphics.Capture, GPUI on DirectX 11)
- [ ] Linux desktop (screen capture through PipeWire and the desktop portal)
- [ ] Server-side transcriber (headless bot on the VPS or a GPU box)
- [ ] End-to-end encryption
- [ ] Speaker separation for shared microphones
- [ ] Peer-to-peer for 1:1 calls
- [ ] Native mobile apps (reusing the Rust core through UniFFI)
