# Quipcall — Implementation Plan

How to read this file:

- A finished task gets `[x]` and ~~strikethrough~~.
- A finished stage gets its heading struck through and a ✅.
- Each stage ends with **Done when**: the checks that have to pass before we cross it out.
- Decisions and the reasoning behind them live in [ARCHITECTURE.md](ARCHITECTURE.md).

**Current focus:** Stage 2 — Call UI (Stage 1 waits on a manual test of camera, speaker and screen share)

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
- [ ] Desktop: microphone and speaker via `PlatformAudio`, with echo cancellation, noise suppression and auto gain from WebRTC. *Mic → web verified (audio arrives with signal, no concealment). Speaker playback still needs a human ear.*
- [x] ~~Desktop: screen share via ScreenCaptureKit, with frame-rate presets 5/15/30/60 and a text/motion mode~~ *Verified by sharing one of Quipcall's own windows: the web client receives 2200×1440 at 15 fps (text) and 1920×1256 at 30 fps (motion), 0 dropped frames.*
- [x] ~~Desktop: ask for mic and camera permission up front (AVCaptureDevice `requestAccess`) before opening the audio device. Otherwise a pending prompt blocks WebRTC's audio thread and freezes the call (found in testing).~~ *The header shows "Waiting for … access" while the prompt is open; if the mic is denied, the call continues listen-only.*
- [x] ~~Desktop: pick the simulcast layer per tile from its on-screen size (the Rust SDK's adaptive stream doesn't report sizes)~~
- [ ] Desktop: camera capture and publish (AVFoundation → full-range NV12 `CVPixelBuffer` → VideoToolbox H.264, simulcast 720p/360p/180p). *Publish path verified with the synthetic camera (`QUIPCALL_FAKE_CAMERA=1`): the web client receives 720p30 H.264 with 0 dropped frames. The AVFoundation capture itself still needs a first run by a person (camera permission prompt).*
- [ ] Desktop: local self-view for the camera, drawn straight from the capture buffers. Screen share shows the active preset instead of a preview, so we don't spend CPU converting our own screen. *Implemented with the camera; not mirrored yet.*
- [x] ~~Web: minimal Svelte page that joins the same room with camera, mic, and screen share (with frame-rate picker)~~
- [x] ~~Zero-copy receive path for hardware-decoded frames~~ *VideoToolbox already outputs full-range NV12, so the decoded `CVPixelBuffer` goes to GPUI as is. Receiving 720p: ~7.5% CPU with H.264 vs ~12.5% with VP8. The web client now publishes H.264 by default.*
- [x] ~~Direct ScreenCaptureKit capture (`CVPixelBuffer` → `NativeBuffer` → VideoToolbox)~~ *Replaced libwebrtc's capturer on macOS; needed for our own picker anyway. Text mode moved from VP9 to H.264: 460 → 173 MB, 24 → 11% CPU.*
- [x] ~~Fix: every video frame leaked an IOSurface (libwebrtc's autoreleased Objective-C wrappers on threads without an autorelease pool)~~ *Found while measuring.*
- [ ] Measure the footprint and write `docs/FOOTPRINT.md`. *Recorded (as physical footprint): receive only ~170 MB / 6%, 1:1 both ways ~189 MB / 9%, 10-person grid ~199 MB / 15%, all within budget. Screen share (sending, plus receiving one 720p camera): 15 fps text at 2200 px ~173 MB / ~11%, 30 fps motion at 1920 px ~188 MB / ~13%.*
- [ ] Find out why the mic track sometimes shows as muted on the server right after joining. *Seen twice, not reproduced in 10 scripted runs. Mic toggles and mute changes are logged now.*
- [ ] GPUI go/no-go. *Recommendation: go (see the notes below the stage).*

**Done when:** a 1:1 call between desktop and web works locally with audio, video, and screen share at a chosen frame rate. Footprint numbers are recorded, and we've made a go/no-go call on GPUI.

**GPUI so far.** For: video frames are drawn straight from `CVPixelBuffer`s with no copy, the whole call stays within the CPU budget, the binary is 25 MB, and Zed proves it on Windows and Linux. Against: about 90 MB of the ~170 MB footprint is GPU memory held by the window, `gpui-pre` is a snapshot of an unstable API (pinned to an exact version), and some basics are missing (no transforms, so no mirrored self-view yet; no widget library, so we build our own controls). None of these is a blocker, and the alternatives (Tauri/web view, Slint, egui) would need their own zero-copy video path.

## Stage 2 — Call UI (desktop): home, minimal and regular modes

Design: [UI.md](UI.md).

- [x] ~~Design: home screen, minimal mode (floating circles + glass toolbar), regular mode~~
- [x] ~~App structure: one call model shared by several windows; each video view repaints only itself~~
- [ ] Floating circle: circle-clipped borderless panel above all apps, drag to move, resize (handle, scroll or pinch; 120–480 px), video or avatar, speaking ring, muted badge. *Built; content checked with snapshots. The circle clip, dragging and resizing need a look on a real screen.*
- [x] ~~Resize knob on hover: a dark knob with arrows on the circle's rim at 45° (the window mask grows to include it while you hover); screen shares get it in the corner~~
- [x] ~~Share picker: a window with live thumbnails of screens and windows, frame-rate presets, Share / Cancel (Esc, Enter, double-click)~~ *Explains the Screen Recording permission when it's off.*
- [x] ~~Fix: the expand button quit the app~~ *Switching modes closed every window before opening the new one, and the app quit when no window was left. Windows now close a tick later, and the app only quits when there's no call and no home window.*
- [ ] Glass toolbar: Lucide icons; mic, camera, screen share with fps presets, copy link, invite, expand, leave; draggable. *Built; the blur and clicks need a look on a real screen.*
- [ ] Other participants as circles, screen shares as floating rectangles, sensible placement for new circles. *Circles verified with 10 people; screen-share windows not tested yet.*
- [ ] Regular mode: one window with tiles and the toolbar; switch both ways. *Verified with a scripted run (circles → regular → circles → leave → home); needs a click test.*
- [ ] Home window: rooms (persistent and one-time) and call history, stored locally until Stage 3. *Built and rendered; rooms get generated names ("Room 1") until there's a text field to rename them.*
- [ ] Text field for naming rooms (adapt GPUI's `input.rs` example)
- [ ] Mirror your own circle
- [x] ~~Footprint with 10 circles open~~ *~207 MB, ~23% CPU with 10 circles + toolbar while sending the camera. Memory is the same as one grid window; CPU is higher because every circle draws itself.*

**Done when:** a 3+ person call runs in minimal mode with movable, resizable circles and the glass toolbar; switching to regular mode and back works; home lists rooms and history; footprint with 10 circles is recorded.

## Stage 3 — Backend core

- [ ] SQLite via sqlx, with migrations
- [ ] Accounts: invite codes, username + password (argon2), session tokens
- [ ] `protocol`: REST request/response types and WebSocket messages, with TypeScript types generated for web
- [ ] WebSocket hub: authenticated connection, presence (online / away / in call)
- [ ] LiveKit token minting for calls; LiveKit webhooks (joined / left / room finished) → call records
- [ ] Remove the dev-only token endpoint from non-dev builds

**Done when:** integration tests for auth, token minting, and WebSocket presence pass.

## Stage 4 — Desktop app shell

- [ ] Login, with the session stored in the macOS Keychain
- [ ] Menu-bar mode: runs in the background, stays connected for presence, small idle footprint. Check that GPUI's GPU memory (~90 MB with the call window open) is released when no window is open.
- [ ] `core`: call state machine with no UI code
- [ ] Contacts: add by username, see who's online
- [ ] Settings: mic, speaker, camera, default screen-share preset

**Done when:** the app starts in the menu bar, logs in, shows contacts with live presence, and meets the idle budget.

## Stage 5 — 1:1 calls

- [ ] Ringing protocol: invite → ringing → accept / decline / cancel / timeout / busy. With multiple devices, the first accept wins.
- [ ] Incoming-call window and ringtone
- [ ] Device switching (mic, speaker, camera) during a call
- [ ] Call history

**Done when:** two desktops can call each other through the backend, and every ringing state is covered by tests.

## Stage 6 — Web client

- [ ] Login, contacts, place and answer calls from the browser
- [ ] Call UI on par with desktop: grid, controls, screen share with frame-rate picker
- [ ] Mobile layout (active speaker + up to 4 tiles), tested in Chrome on Android

**Done when:** calls work web ↔ desktop, including from an Android phone.

## Stage 7 — Alpha deploy to the VPS (friends start testing here)

- [ ] LiveKit on a single node: UDP port, built-in TURN/TLS for strict networks
- [ ] Caddy (HTTPS), systemd units, server, and static web files
- [ ] SQLite backups
- [ ] macOS build: `.app` bundle, signing and notarization (or ad-hoc signing for friends), simple update check
- [ ] Invite codes for friends

**Done when:** friends install the app, sign up, and call each other over the internet, including through strict NAT.

## Stage 8 — Group calls (up to ~10 people)

- [ ] Groups: create, add/remove members, owner role
- [ ] Start a group call (rings online members); join a call already in progress; show which groups are in a call
- [ ] Guest links: signed, expiring link → join on web with a display name and limited permissions; the host can remove guests
- [ ] Layouts: grid / speaker / screen-share focus; active speaker; pagination
- [ ] Performance: adaptive stream, dynacast, pause off-screen tiles, tuned simulcast layers

**Done when:** a 10-participant call with a mix of desktop, web, and bots stays smooth, and CPU is measured.

## Stage 9 — Transcription (single transcriber)

- [ ] `crates/stt`: VAD (Silero), a `Transcriber` trait, model manager (download on demand, checksum)
- [ ] Model bake-off for our languages (accuracy and speed on Apple Silicon); record the decision
- [ ] Transcriber role in the backend: claim, lease, and heartbeat; capability shown via participant attributes; handoff prompt
- [ ] Pipeline: local mic (before encoding) + each remote track → 16 kHz → VAD → shared ASR queue; partial and final results
- [ ] Transcript flow: transcriber → backend (store and order) → WebSocket to all participants; late joiners get the history
- [ ] Transcript panel (chat style) on desktop and web; "Transcribed by X" indicator
- [ ] Raise Opus bitrate while a transcription is running

**Done when:** a 1-hour mixed-language test call produces a transcript with correct speakers, and handing off the transcriber role works.

## Stage 10 — LLM summaries

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
