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
  Desktop (Rust) ───┼─ HTTPS/WS ─▶ Caddy ─▶ quipcall-server (Axum, SQLite)       │
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
| macOS media | `PlatformAudio` (CoreAudio, WebRTC echo cancellation / noise suppression / auto gain), ScreenCaptureKit, AVFoundation, VideoToolbox |
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
docs/               plan, architecture, footprint measurements
```

## Media decisions

- **Rendering video (macOS).** GPUI's `surface()` draws full-range NV12 `CVPixelBuffer`s directly on the GPU. Hardware-decoded H.264 already arrives in that format and is drawn with no copy; software-decoded frames are converted into pooled IOSurface buffers.
- **Codec.** H.264 everywhere (camera and screen share) so every Mac encodes and decodes in hardware. VP9 for screen text cost ~460 MB and ~24% CPU at 2560 px, against ~175 MB and ~11% for H.264.
- **Screen share (macOS).** Our own picker (live thumbnails from ScreenCaptureKit), then ScreenCaptureKit captures straight into IOSurface buffers at the preset's frame rate and VideoToolbox encodes them, no CPU copy. Needs the Screen Recording permission once. Our own floating windows are left out of the share. Presets:
  - *Text*: 5 or 15 fps, up to 2560 px; when bandwidth drops, keep resolution and lower the frame rate.
  - *Motion*: 30 or 60 fps, up to 1920 px; when bandwidth drops, keep the frame rate and lower resolution.
- **Bandwidth.** Simulcast + adaptive stream (each video is received at the size it's shown) + dynacast (layers nobody watches aren't encoded) + pausing tiles that aren't visible.
- **Audio.** Opus with DTX and RED, using WebRTC's audio processing through the platform audio device module.

## Transcription design (single transcriber)

- One capable participant (a desktop with the model downloaded) turns on "Transcribe this call" and becomes the **transcriber** for everyone.
- Each participant's audio arrives as a separate track, so we always know **who said what**. No speaker separation needed.
- Pipeline: own mic (before encoding) + each remote track → resample to 16 kHz → VAD per track → one shared ASR queue.
- Segments `{speaker, start, end, text, final}` → backend (stores and orders them) → WebSocket to every participant. People who join late get the history.
- Role handling: the backend keeps a lease with a heartbeat. If the transcriber leaves, others are asked to take over. Everyone sees "Transcribed by X".
- Later, a server-side transcriber (a headless Rust bot) takes the same role, so nothing else changes.

## Alpha scope

Only me and friends, self-hosted on one VPS. About 10 people per call at most. Android users join through the mobile web client.
