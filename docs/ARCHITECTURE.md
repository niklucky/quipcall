# Quipcall — Architecture

A small, fast app for 1:1 and group video/audio calls, with screen sharing, local speech-to-text and LLM summaries.
Desktop (macOS first, then Windows and Linux) plus a web client.

## Principles

1. **Footprint first.** No bundled browser engine. The app ships and controls its own media engine. Speech-to-text models are downloaded only when needed.
2. **The media engine is the product.** We own capture, encoding and rendering, so codecs, echo cancellation and screen-share frame rate behave the same on every OS.
3. **The UI is replaceable.** All call logic lives in `crates/core`, with no UI code. If GPUI doesn't work out, we swap the UI without touching media.

## Components

```
                    ┌─────────────────────────── VPS ───────────────────────────┐
                    │                                                           │
  Desktop (Rust) ───┼─ HTTPS/WS ─▶ nginx ─▶ quipcall-server (Axum, SQLite)       │
  Web (Svelte)  ────┤                        accounts, contacts, groups, invites│
                    │                        presence + ringing (WebSocket)     │
                    │                        LiveKit tokens, webhooks, transcripts, summaries
                    │                                                           │
                    └─ WebRTC (UDP / TURN) ─▶ LiveKit SFU (single node)          │
                    └───────────────────────────────────────────────────────────┘
```

| Part | Tech |
|---|---|
| Media server | LiveKit (self-hosted). 1:1 calls also go through it, so there's one code path |
| Desktop | Rust, GPUI (`gpui-pre`, custom controls, no widget library), LiveKit Rust SDK (libwebrtc). UI design: [UI.md](UI.md) |
| macOS media | `PlatformAudio` (CoreAudio, WebRTC echo cancellation / auto gain), DeepFilterNet 3 noise suppression (tract), ScreenCaptureKit, AVFoundation, VideoToolbox |
| Web | Svelte 5, Vite, `livekit-client` |
| Backend | Rust: Axum, tokio, sqlx + SQLite |
| Shared types | `crates/protocol` (serde), with TypeScript types generated for web |
| Speech-to-text | `crates/stt`: Silero VAD + a pluggable ASR model (chosen in Stage 9) |
| Summaries | `Summarizer` trait: Claude API or a local LLM |

## Repo layout

```
crates/protocol     shared request/response and WebSocket message types
crates/core         call engine: LiveKit room, tracks, devices, call state (no UI)
crates/capture-mac  ScreenCaptureKit and AVFoundation capture (added when needed)
crates/stt          VAD + ASR pipeline, model manager (Stage 9)
apps/desktop        GPUI app
apps/server         Axum backend
apps/web            Svelte web client
dev/                local LiveKit config
deploy/             VPS deploy (nginx, systemd, LiveKit config) and Mac app packaging
docs/               plan, architecture, footprint measurements, deploy runbook
```

## Media decisions

- **Rendering video (macOS).** GPUI's `surface()` draws full-range NV12 `CVPixelBuffer`s directly on the GPU. Hardware-decoded H.264 already arrives in that format and is drawn with no copy; software-decoded frames are converted into pooled IOSurface buffers.
- **Codec.** H.264 everywhere (camera and screen share) so every Mac encodes and decodes in hardware. VP9 for screen text cost ~460 MB and ~24% CPU at 2560 px, against ~175 MB and ~11% for H.264.
- **Screen share (macOS).** Our own picker (live thumbnails from ScreenCaptureKit), then ScreenCaptureKit captures straight into IOSurface buffers at the preset's frame rate and VideoToolbox encodes them, no CPU copy. Needs the Screen Recording permission once. Our own floating windows are left out of the share. Your own preview of the share draws the same IOSurfaces (no copy) and repaints at most 15 times a second. Everyone in a call can share at the same time: each share is its own track. Presets:
  - *Text*: 5 or 15 fps, up to 2560 px; when bandwidth drops, keep resolution and lower the frame rate.
  - *Motion*: 30 or 60 fps, up to 1920 px; when bandwidth drops, keep the frame rate and lower resolution.
- **Bandwidth.** Simulcast + adaptive stream (each video is received at the size it's shown) + dynacast (layers nobody watches aren't encoded) + pausing tiles that aren't visible.
- **Audio.** Opus with DTX and RED, using WebRTC's audio processing through the platform audio device module.
- **Noise suppression (desktop).** DeepFilterNet 3, a neural filter, runs on each 10 ms microphone frame right after WebRTC's echo cancellation and gain control, through a hook in our `webrtc-sys` patch (`vendor/README.md`). WebRTC's own suppressor only removes steady noise (fans, hum); measured on real recordings, the model takes 21–47 dB of barking, wrappers, paper, typing and traffic out of speech pauses, where WebRTC's takes 1–5 dB and RNNoise 1–21 dB. Costs on an M5 Pro: 0.2–0.5 ms per frame (2–5% of one core while unmuted), 30 ms of delay, about 12–29 MB of memory once a call loads it, and 17.6 MB of binary (8 MB of it the model). WebRTC's suppressor is off while the model runs; it takes over if the model fails to load or a Mac needs more than 3 ms per frame. The web client keeps the browser's suppressor.

## Transcription design (single transcriber)

- One capable participant (a desktop with the model downloaded) turns on "Transcribe this call" and becomes the **transcriber** for everyone.
- Each participant's audio arrives as a separate track, so we always know **who said what**. No speaker separation needed.
- Pipeline: own mic (before encoding) + each remote track → resample to 16 kHz → VAD per track → one shared ASR queue.
- Segments `{speaker, start, end, text, final}` → backend (stores and orders them) → WebSocket to every participant. People who join late get the history.
- Role handling: the backend keeps a lease with a heartbeat. If the transcriber leaves, others are asked to take over. Everyone sees "Transcribed by X".
- Later, a server-side transcriber (a headless Rust bot) takes the same role, so nothing else changes.

## Alpha scope

Only me and friends, self-hosted on one VPS. About 10 people per call at most. Android users join through the mobile web client.

Deployed at https://quipcall.com ([DEPLOY.md](DEPLOY.md)). The VPS is shared with other projects and already runs nginx with certbot, so we use those rather than Caddy. LiveKit takes all media on one UDP port (7882) with a TCP fallback (7881), so the firewall story is two ports. The Mac app ships as one build per architecture, signed with a self-signed certificate (so updates keep their permissions) until it's worth paying for notarization.
