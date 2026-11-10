# Quipcall — Implementation Plan

How to read this file:

- A finished task gets `[x]` and ~~strikethrough~~.
- A finished stage gets its heading struck through and a ✅.
- Each stage ends with **Done when**: the checks that have to pass before we cross it out.
- Decisions and the reasoning behind them live in [ARCHITECTURE.md](ARCHITECTURE.md).

**Current focus:** Stage 2 — Call UI (Stage 1 waits on a manual test of camera, speaker and screen share). An early alpha (Stage 7, partly) runs at https://quipcall.com so friends can test calls.

---

## Footprint budget

These are targets. We measure them at the end of Stage 1 and at every milestone, and record the results in `docs/FOOTPRINT.md`.

| Metric | Target |
|---|---|
| macOS app bundle (without speech-to-text models) | ≤ 60 MB per architecture *(40 MB until 2026-11-10: quality over a few megabytes; the Apple Silicon app is 53.6 MB with transcription)* |
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
- [ ] Desktop: neural noise suppression (DeepFilterNet 3) after WebRTC's echo cancellation, then AGC2, through a `webrtc-sys` hook (`vendor/README.md`). *First real call: voice dipped; the model was muting quiet voices (fixed with a 12 dB input gain and a 12 dB limit). Second call: better, hard rustling still faintly audible, so pauses now allow 30 dB ([FOOTPRINT.md](FOOTPRINT.md)). Needs a third listening check, and a look at double talk on laptop speakers.*
- [x] ~~Desktop: screen share via ScreenCaptureKit, with frame-rate presets 5/15/30/60 and a text/motion mode~~ *Verified by sharing one of Quipcall's own windows: the web client receives 2200×1440 at 15 fps (text) and 1920×1256 at 30 fps (motion), 0 dropped frames.*
- [x] ~~Desktop: ask for mic and camera permission up front (AVCaptureDevice `requestAccess`) before opening the audio device. Otherwise a pending prompt blocks WebRTC's audio thread and freezes the call (found in testing).~~ *The header shows "Waiting for … access" while the prompt is open; if the mic is denied, the call continues listen-only.*
- [x] ~~Desktop: pick the simulcast layer per tile from its on-screen size (the Rust SDK's adaptive stream doesn't report sizes)~~
- [ ] Desktop: camera capture and publish (AVFoundation → full-range NV12 `CVPixelBuffer` → VideoToolbox H.264, simulcast 720p/360p/180p). *Publish path verified with the synthetic camera (`QUIPCALL_FAKE_CAMERA=1`): the web client receives 720p30 H.264 with 0 dropped frames. The AVFoundation capture itself still needs a first run by a person (camera permission prompt).*
- [ ] Desktop: local self-view for the camera and for your screen share, drawn straight from the capture buffers. *Implemented; the camera isn't mirrored yet. The share preview needed no conversion after all: ScreenCaptureKit already delivers full-range NV12 IOSurfaces.*
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
- [x] ~~Design round 2: clickable prototype of every screen ([prototype/ui.html](prototype/ui.html)): onboarding, two-pane home, a finished call, regular and minimal modes~~ *Approved, with one change: while you share, your face in the capsule turns into the preview.*
- [x] ~~App structure: one call model shared by several windows; each video view repaints only itself~~
- [ ] Minimal mode as one glass capsule (replaces the window per circle and the toolbar): faces with camera or avatar, speaking ring, muted badge, mic button on your face; mic and Leave always shown, time, camera, share, link and expand on hover; your face becomes your share preview; "+N" past 6 people with the speaker always shown; a column at the screen's left or right edge; scroll or pinch to resize faces (32–96 px). *Built; checked with snapshots (faces with video, share preview, "+N", column). Hover, dragging, docking and resizing need a look on a real screen.*
- [ ] Floating circle per person. *Replaced by the capsule above (the code is gone).*
- [x] ~~Design round 4 for the minimal widget ([prototype/call-widget.html](prototype/call-widget.html)), after live testing found the capsule unusable: the row shows the 3 people active last plus you, the muted and the quiet fold into a stack, cameras become tiles above the row, hover rows open underneath instead of controls sliding out, your share preview grows on top, sharers get a badge~~
- [ ] Minimal widget, round 4. *Built; checked with snapshots (tiles, stack and its row, controls row, your share preview, the flip near the screen's bottom). First live test: hover rows never closed (GPUI reports no hover change once the cursor leaves a window; the cursor is now polled while a row is open), muting shifted the row (you now stay in it with a badge, and the stack keeps one width), and the widget jumped as rows changed (content now stays where the window is until a resize lands, and the window grows from its left end). Second live test: still jumped when sharing (two resizes in a row confused that bookkeeping; the main row's place is now set right before each frame change, and the preview waits for its first frame), the stack showed grey placeholders (now real faces, dimmed), and screen recordings showed nothing (floating windows were hidden from all capture; now only our own shares leave them out). Third live test (a recording, alone in a call): empty space left of your face (the widget had a 240 px floor; it now fits its row, and the hover rows fit in that, so hovering never changes the width), the widget skewed on every hover (the frame AppKit asks for during a resize was throttled, since GPUI caps windows that aren't key at 30 fps: on a 1× display all 24 of 24 resize frames were skipped in a scripted run, and the old frame showed stretched; patched in GPUI, now 25 of 25 draw), once for seconds (not reproduced; frames drawn into a buffer of the old size are now skipped too), a black ring round the mute badge, and pale glass over white pages (the tint went from 38% to 78%). Code review: alone with your camera on the widget was narrower than the hover row (it now never gets narrower than that row, which sits right-aligned under the buttons so a change of width moves nothing in it), and near the top of the screen a tile or your preview pushed the whole widget down (they now open below the main row there; checked with a scripted run at the top of the screen: the window's top and right edge stayed put through a share and a hover).*
- [x] ~~Resize knob on hover: a dark knob with arrows on the circle's rim at 45° (the window mask grows to include it while you hover); screen shares get it in the corner~~
- [x] ~~Share picker: a window with live thumbnails of screens and windows, frame-rate presets, Share / Cancel (Esc, Enter, double-click)~~ *Explains the Screen Recording permission when it's off.*
- [x] ~~Fix: the expand button quit the app~~ *Switching modes closed every window before opening the new one, and the app quit when no window was left. Windows now close a tick later, and the app only quits when there's no call and no home window.*
- [x] ~~Glass toolbar: Lucide icons; mic, camera, screen share with fps presets, copy link, invite, expand, leave; draggable~~ *Folded into the capsule.*
- [ ] Other participants in the capsule, screen shares as floating rectangles. *Circles verified with 10 people; screen-share windows checked with snapshots (synthetic screens), dragging and resizing them not yet.*
- [ ] Your own share as a preview: your face in the capsule turns into a rounded rectangle of what you share (same IOSurfaces, ≤ 15 fps repaints); the big tile in regular mode when nobody else shares. *Built; checked with snapshots using the synthetic screen and camera.*
- [ ] Fullscreen for someone's share: double-click it or press ⤢ (floating window), double-click its tile (regular mode); Esc, a double-click or ⤡ goes back. Web: double-click or ⤢ on a share tile. *Built; a scripted run entered native fullscreen and came back to the floating window. Clicks and Esc need a test by hand. Web fullscreen verified in headless Chrome. Live test (build 35): a share that ended while its fullscreen Space wasn't current left a dead black Space (Dock: "trying to take pid … out of fullscreen but it is not the current space"); now the window stays, says the share ended, and closes once its Space is showing. Not yet retested live.*
- [x] ~~Several people sharing at once~~ *Already worked (one share per person). Verified: two desktops sharing at the same time each receive the other's share, and a web participant sees both.*
- [x] ~~Synthetic screen for tests: `QUIPCALL_FAKE_SCREEN=1` (or `=WIDTHxHEIGHT`) shares a test pattern through the real publish path, so tests never capture the screen~~
- [ ] Regular mode: title bar with the time and Invite; layouts that pick themselves (alone with the link, grid up to 4, speaker with a strip that fits, share with faces in a column); a floating control bar; a side panel with the call's timeline and its people; switch both ways. *Built; each layout and both panel tabs checked with snapshots (synthetic camera and screen, `lk` bots). Needs a click test.*
- [ ] Home window: two panes. Rooms (avatar and "+N", last call) and history by day on the left; the selected call on the right with rename, copy link, remove, Call, ‹ › through the room's calls, and Transcript / Recordings / Files tabs (the timeline for now, empty states for the rest); quick calls get Keep as room and Call again. Opens on the call you just left. Stored locally until Stage 3. *Built; checked with snapshots on seeded data and after a scripted call. Needs a click test, renaming by hand included.*
- [ ] A call's timeline: who joined, left and started or stopped sharing, with times, saved with the call. *Built; checked after a scripted call.*
- [ ] First launch asks for your name (also behind the pencil on home); `--name` skips it. *Built; checked with a snapshot. Needs typing by hand.*
- [x] ~~Text field for naming rooms (adapt GPUI's `input.rs` example)~~ *`text_input.rs`: IME, selection, copy and paste; Enter and Esc as events.*
- [ ] Mirror your own circle
- [ ] Mic button on your own circle (also on your screen preview). *Built; checked with snapshots, needs a click.*
- [x] ~~Fix: after leaving, you stayed in the room as a ghost for 15–40 s, so rejoining showed you twice~~ *The engine's runtime was dropped with the call, cancelling the goodbye to LiveKit mid-way. It now finishes on a thread of its own, and quitting mid-call leaves first. Both checked in LiveKit's log (`CLIENT_REQUEST_LEAVE`).*
- [x] ~~Footprint with 10 circles open~~ *~207 MB, ~23% CPU with 10 circles + toolbar while sending the camera. Memory is the same as one grid window; CPU is higher because every circle draws itself.*
- [x] ~~Footprint of the capsule with 10 people~~ *~19% CPU, ~135 MB (no mic) once repaints were capped at 30 fps; ~24% before ([FOOTPRINT.md](FOOTPRINT.md)).*

**Done when:** a 3+ person call runs in minimal mode in the capsule (moved, docked, faces resized); switching to regular mode and back works; home lists rooms and history and shows a finished call; footprint with the capsule is recorded.

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
- [ ] Noise suppression switch (off for music), desktop and web; the web client gets the same model in WASM. Then shrink the model's footprint: a pre-converted model to cut load-time memory, or our own inference on Accelerate instead of tract.

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

Started early (2026-11-03), ahead of Stages 3–6: there are no accounts yet, so rooms are open links (`/api/join`). Runbook: [DEPLOY.md](DEPLOY.md).

- [x] ~~LiveKit on a single node: one UDP port (7882), TCP fallback (7881)~~ — calls from outside connect over UDP
- [ ] Built-in TURN/TLS for strict networks (only 443 open)
- [x] ~~HTTPS, systemd units, server, and static web files~~ — nginx + Let's Encrypt (the box already runs them for other projects, so not Caddy); `make deploy`
- [ ] SQLite backups
- [x] ~~macOS build: `.app` bundle, ad-hoc signing for friends~~ — one app per architecture, `curl … | sh` installer, call links (`quipcall://join/<room>`); `make release-mac`
- [ ] Updates keep their permissions: releases are signed with one self-signed certificate (`deploy/mac/signing.sh`), so macOS's requirement for the app stays the same across builds. *Built; needs two releases in a row to confirm.*
- [ ] Notarization (Apple Developer ID), so a downloaded zip opens without the manual "Open"; simple update check
- [ ] Invite codes for friends
- [x] ~~Fix: crash on join on Macs with a Touch Bar~~ — floating windows are created borderless (patched GPUI) instead of switching style afterwards; the accessibility content view also keeps its class when home closes so Touch Bar observers survive the transition
- [ ] Verify on the affected Intel Mac (macOS 15.6, two displays, Touch Bar): call windows stay visible when sharing and switching apps. *Fixed NSPanel's default hide-on-deactivate behavior for floating windows and popups; audio continued while the UI disappeared. Needs a manual check with the updated app.*
- [ ] Verify share picker responsiveness with many windows. *Permission checks and enumeration run off the UI thread; choices arrive before previews. Native submissions run on isolated workers with deadlines and a shared limit of four requests across picker sessions. In-flight requests keep their slot through timeouts and cancellation until the native callback completes; listing failures offer Retry.*

**Done when:** friends install the app, sign up, and call each other over the internet, including through strict NAT.

## Stage 8 — Group calls (up to ~10 people)

- [ ] Groups: create, add/remove members, owner role
- [ ] Start a group call (rings online members); join a call already in progress; show which groups are in a call
- [ ] Guest links: signed, expiring link → join on web with a display name and limited permissions; the host can remove guests
- [ ] Layouts: grid / speaker / screen-share focus; active speaker; pagination
- [ ] Performance: adaptive stream, dynacast, pause off-screen tiles, tuned simulcast layers

**Done when:** a 10-participant call with a mix of desktop, web, and bots stays smooth, and CPU is measured.

## Stage 9 — Transcription (single transcriber) and recording

- [x] ~~Design round 1: clickable prototype ([prototype/transcription.html](prototype/transcription.html)): Transcribe in the widget's hover row, choosing whose Mac transcribes (ask, accept, hand over, take over when the transcriber leaves), the transcript as a chat in its own floating window, the call window's side panel, and Settings › Transcription (language and model, downloads, try it with your voice)~~ *Approved as drawn: anyone in the call can stop the transcript, the window opens for everyone (a switch turns that off), one model per call, and the window's footprint gets measured.*
- [x] ~~Design round 2, same prototype: a separate Record button that saves the call's audio, or audio and video, on the recorder's Mac. Its panel, the indicator and notices for everyone, toolbar buttons in the call window, Settings › Recording, and Home after the call (the recording plays under the transcript)~~ *Decided: video means your own screen, picked as when sharing; one recording per call; only whoever started it can stop it; sharing the file with the room later.*
- [x] ~~Design round 3, same prototype: "Audio and screen" through the share picker in record mode, one recording per call (Record leaves everyone else's hover row), and the recorder leaving.~~ *Approved.*
- [x] ~~`crates/stt`: VAD (Silero), model manager (download on demand, checksum)~~ *transcribe.cpp runs Silero VAD, Parakeet v3 and Qwen3-ASR (no trait needed: one engine for every model). Downloads resume and check SHA-256 (`crates/core/src/models.rs`).*
- [x] ~~Model bake-off for our languages (accuracy and speed on Apple Silicon); record the decision~~ *Parakeet v3 and Qwen3-ASR on one paragraph in six languages (macOS voices, `crates/stt/examples/bakeoff.rs`): both perfect or one word off in English, Russian, German and French; Qwen3-ASR also gets Chinese and Japanese, Parakeet doesn't know them. Parakeet is 4× faster (160× real time on the GPU, 42× on the CPU, against 39× and 18×). Decided: Parakeet by default, Qwen3-ASR for Chinese, Japanese and Korean (Settings suggests it). Whisper, GigaAM and Nemotron would need transcribe.cpp support or another engine; not now.*
- [x] ~~Transcriber role: claim, lease, and heartbeat; capability shown via participant attributes; handoff prompt~~ *Built without the backend: LiveKit's presence is the lease (a transcriber's attribute goes when they leave). `quipcall.can_transcribe` marks Macs with a model ready; Transcribe asks "whose Mac?", the other Mac gets Accept / Not now (or accepts by itself, a Setting); hand over from the transcript window or the People tab; when the transcriber leaves, everyone who can is asked to take over. Two Macs transcribing at once: the newest carries on, the other stops. Verified with two app instances: handover, and the take-over prompt.*
- [x] ~~Pipeline: local mic (before encoding) + each remote track → 16 kHz → VAD → shared ASR queue; partial and final results~~ *Verified in a call: a web participant speaking English and Russian (macOS `say` as its microphone) came out right, with drafts about once a second.*
- [x] ~~Transcript flow to all participants; late joiners get the history~~ *LiveKit data messages from the transcriber (`quipcall.transcript`) instead of a backend WebSocket: lines to everyone, the history to late joiners, and anyone can ask it to stop. Verified with two app instances and a web participant. Everyone keeps the transcript on their own device; storing it on the server waits for accounts (Stage 3).*
- [x] ~~Transcript panel (chat style) on desktop and web; "Transcribed by X" indicator~~ *Desktop: the transcript window, the call window's side panel, the widget's captions button, Home's Transcript tab. Web: a side panel (below the video on phones) that opens when a transcript starts, with Stop for everyone.*
- [x] ~~Raise Opus bitrate while a transcription is running~~ *Not needed: lines transcribed through a call (a web participant's Opus) came out word for word as from the clean audio.*
- [x] ~~Settings window (⌘,) with Recording and Transcription tabs: language, model downloads (on demand, checksum, remove), try it with your voice~~ *Transcription: language, model, downloads (resumed, retried, checked), remove, the window switch, accept asks by itself, Try it (your mic through the model, live), credits. Recording: what Record offers first, each voice in its own file too, the folder.*
- [x] ~~Everyone's voice, 10 ms at a time, for recording and transcription (`audio_tap.rs`): your mic through a new hook in the webrtc-sys patch (after processing, only while unmuted), remote mics through LiveKit's audio sinks; nothing is copied unless something listens~~ *Remote audio verified end to end; your own voice in a real call recording (2026-11-09).*
- [x] ~~Recording, audio: everyone mixed into one AAC .m4a with AVAssetWriter, a fragment every 5 s; optionally one track per voice~~ *Verified with a headless web participant: its 2-per-second beeps land in the file exactly 0.5 s apart, a 14 s recording is 14.1 s, quitting mid-recording finishes the file, and a test that aborts the writer keeps 10 of 12 s. Each voice in its own file too (a Setting): one .m4a per person in a folder next to the mix, the same length as the mix.*
- [ ] Recording, screen: the share picker in record mode (same presets), ScreenCaptureKit and VideoToolbox H.264 plus the audio mix in one .mp4. Measure CPU. *Built. Checked with the synthetic screen and a web participant: 1280×800 H.264 at 15 fps, 150 frames in 10 s (none dropped), its beeps in the sound, and "… is recording this call and their screen" on the web. The choice of Audio or Audio and screen looked right in the widget (2026-11-10). CPU: about +1.5% while recording a 1920×1200 screen at 15 fps (release, M5 Pro). Sharing and recording the same screen runs two captures; sharing one capture is for later. Known issue: at 1920×1200, AVAssetWriter can fail (-11800, underneath -16341) after the process has been suspended a few times (reproduced only with `vmmap` attached; 1280×800 survives it). A failure now shows at once, clears the indicator for everyone, and the file keeps what came before its last 5 s fragment.*
- [x] ~~One recording per call: a participant attribute marks the recorder, Record is hidden for everyone else, and only the recorder can stop it. An indicator for everyone, a notice when it starts, a "saved" notice, and a stop when the recorder leaves~~ *In the widget, the call window and the web client; checked with two app instances and a web participant. Out since 2026-11-10 (build 40).*
- [x] ~~Home: the Recordings tab (opens the file in QuickTime Player) and a player under the transcript that plays the recording's sound~~ *AVPlayer under the transcript: any line plays from where it was said, the line playing is outlined, a bar jumps along.*
- [x] ~~Footprint of a long transcribed call~~ *30 minutes of one voice transcribed with the window open (release, M5 Pro): about 240 MB between utterances and 320–330 MB while the model runs, the same at the end as at the start; the baseline rose by about 0.5 MB a minute (244 lines). CPU about 9% with the speaker's video.*

**Done when:** a 1-hour mixed-language test call produces a transcript with correct speakers, handing off the transcriber role works, and a recording of the same call plays under its transcript in Home.

*Status 2026-11-10: everything above is built and out (web deployed, Mac build 46). Checked on quipcall.com: a Mac transcribing a web participant in English and Russian, a late joiner getting the history, the web panel. Still to do with people: the 1-hour call with several real voices. A code review (Codex) then found and we fixed: a dropped connection leaving the recording running; two Macs starting at once both carrying on, or a handover losing to a clock that runs ahead; the history after a handover missing what came before it; the transcriber's own history missing the line cut off by leaving (verified: it's kept now); a stopped voice trial still opening the microphone; a whole `.part` download never finishing, and two models fetching the VAD at once; a failed encoder going on "recording"; an unmuted voice's first line dated before the mute. Review of the pull request then found and we fixed: anyone in the call could post transcript lines in anyone's name, and a malformed one broke the web panel (lines now come only from the transcriber, checked on both apps); a recording still being finished after you left could take in the next call's voices (each call has its own audio now); a dropped connection could still let a recording or the transcript start; a handover going ahead after the transcript was stopped; the recording notice vanishing while a second recorder carried on; relayed history showing a line twice; two voices' files colliding on a name that differs only in case; and a slow voices' file holding frames without limit. Checked with test instances: forged lines dropped, a late joiner after a two-Mac handover gets the whole transcript once. A second review then found and we fixed: a relay could rewrite or take back another Mac's line; a handover could still go ahead after Stop, while the last words were being transcribed, or after the asker stopped and started again (checked with two test instances); the screen recording ran on after leaving until the last words were in; a room still closing could reach the next call's state on a reused engine; long late-join history in Cyrillic could exceed LiveKit's packet limit. A third review then found and we fixed: Stop could still lose to a handover while its claim went out (a start can now be stopped at any point before it listens, and the asker passes a Stop on; checked with two test instances: a Stop sent to the old transcriber after a handover stops the new one); an ask from an older app could take over a restarted transcript; an overtaken connect could replace a newer one; a relayed final couldn't finish a draft on the web; the web took last words for 10 s, the Mac 15. A fourth review then found and we fixed: Stop waited on sending before it stopped listening; with two handover asks out, a Stop went on to the wrong Mac; a start for a call you left blocked the next call's; a late draft could replace a final. Web deployed with the third round's fixes and checked on quipcall.com (forged and malformed lines dropped, late joiner's history intact). Still open: the transcriber's claim is an attribute anyone can set, so someone could add made-up lines, until the backend lease.*

## Stage 10 — LLM summaries

- [ ] `Summarizer` trait: Claude API (`claude-haiku-5-5` / `claude-sonnet-5-5`), plus a local backend (llama.cpp-compatible)
- [ ] Summaries on demand and at the end of a call, stored in call history
- [ ] Prompt covers decisions, action items, and highlights per speaker, written in the transcript's language

**Done when:** a summary of a test call is generated and visible on both desktop and web.

## Later

- [ ] Windows desktop (Windows.Graphics.Capture, GPUI on DirectX 11)
- [ ] Linux desktop (screen capture through PipeWire and the desktop portal)
- [ ] Server-side transcriber (headless bot on the VPS or a GPU box)
- [ ] Upload recordings to the server so everyone in the room gets them (needs Stage 3)
- [ ] End-to-end encryption
- [ ] Speaker separation for shared microphones
- [ ] Peer-to-peer for 1:1 calls
- [ ] Native mobile apps (reusing the Rust core through UniFFI)
