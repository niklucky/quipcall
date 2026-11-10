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
| Speech-to-text | `crates/stt`: Silero VAD and Parakeet TDT v3 or Qwen3-ASR 0.6B, both run by transcribe.cpp (ggml, Metal) |
| Summaries | `Summarizer` trait: Claude API or a local LLM |

## Repo layout

```
crates/protocol     shared request/response and WebSocket message types
crates/core         call engine: LiveKit room, tracks, devices, call state (no UI)
crates/capture-mac  ScreenCaptureKit and AVFoundation capture (added when needed)
crates/stt          VAD + ASR pipeline and the model catalog (transcribe.cpp)
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
- **Noise suppression (desktop).** DeepFilterNet 3, a neural filter, runs on each 10 ms microphone frame right after WebRTC's echo cancellation, through a hook in our `webrtc-sys` patch (`vendor/README.md`); WebRTC's adaptive gain control (AGC2) runs after it, so gain follows the cleaned voice. It is held back on purpose: while you talk it may lower noise by 12 dB at most (30 dB once the model has heard no voice for 0.3 s), and the voice goes in 12 dB louder (undone after), because the model takes voices below about -45 dBFS for noise. Removing everything it could made voices dip and drop out in a real call. Measured on real voices and recordings (`python3 dev/denoise-eval.py`, [FOOTPRINT.md](FOOTPRINT.md)), it takes 10–27 dB of barks, rustling, clicks and typing out of pauses, where WebRTC's takes 3–13 dB. The cost: the voice dips a little more than with WebRTC's suppressor, and clean speech scores lower on quality, since the model trims some of a voice once it hears a noise floor. Costs on an M5 Pro: 0.4 ms per frame (about 4% of one core while unmuted), 30 ms of delay, about 12–29 MB of memory once a call loads it, and 17.6 MB of binary (8 MB of it the model). WebRTC's suppressor and older gain control (AGC1) are off while the model runs; they take over if the model fails to load or a Mac needs more than 3 ms per frame. The web client keeps the browser's suppressor.

## Transcription design (single transcriber)

- One capable participant (a desktop with the model downloaded) turns on "Transcribe this call" and becomes the **transcriber** for everyone.
- Each participant's audio arrives as a separate track, so we always know **who said what**. No speaker separation needed.
- Pipeline: own mic (before encoding) + each remote track → resample to 16 kHz → VAD per track → one shared ASR queue.
- Segments `{speaker, start, end, text, final}` → backend (stores and orders them) → WebSocket to every participant. People who join late get the history.
- Choosing the transcriber: whoever starts it picks a Mac, their own or someone else's who has a model. That person accepts or declines (Settings can make a Mac accept by itself). The transcriber can hand over from the transcript window. Anyone in the call can stop the transcript.
- Role handling: the backend keeps a lease with a heartbeat. If the transcriber leaves, others are asked to take over. Everyone sees "Transcribed by X".
- Models: one per call, chosen in Settings (a language and a model) and downloaded on demand. We start with Parakeet TDT v3 (25 European languages, Russian included), then Qwen3-ASR 0.6B for Chinese. The bake-off compares them with Whisper Large v3 Turbo, GigaAM v3 and Nemotron 3.5 ASR.
- Later, a server-side transcriber (a headless Rust bot) takes the same role, so nothing else changes.

### As built (alpha)

- **Engine: transcribe.cpp** (crate `transcribe-cpp`, pinned to 0.4.1; ggml with Metal on Apple Silicon, the CPU elsewhere). One engine runs every model we offer and Silero VAD, from GGUF files. It and recording together added 8.9 MB: the Apple Silicon app went from 44.7 MB (main, 2026-11-10) to 53.6 MB, and the Intel one is 60.8 MB. It needs cmake to build. Measured on an M5 Pro: Parakeet v3 runs at about 200× real time on the GPU, 30× on the CPU. Picked over sherpa-onnx (18 MB, CoreML 5× slower), tract (can't load these models) and FluidAudio (macOS 14+). Risks: a young project with one maintainer (hence the pin); the first run of each new build compiles Metal shaders for about 3 s, done while starting, off the UI thread; static linking needs clang's runtime (`crates/stt/build.rs`).
- **Models** (`crates/stt/src/catalog.rs`): Parakeet TDT 0.6B v3 (NVIDIA, CC-BY-4.0, so the app credits it in Settings), 8-bit, 740 MB; Qwen3-ASR 0.6B (Alibaba Qwen, Apache-2.0), 8-bit, 850 MB; Silero VAD v6.2 (MIT), 1.2 MB. Downloaded from Hugging Face only when chosen, pinned to a commit, checked against their SHA-256, resumed after a failure, a dropped connection retried twice by itself (`crates/core/src/models.rs`), into Application Support › Quipcall › Models. 8-bit because the user prefers quality over a smaller download: Parakeet's 6-bit is 610 MB.
- **Pipeline** (`crates/stt/src/lib.rs`): everyone's voice from `audio_tap.rs`, each speaker on their own. A 64-tap FIR takes 48 kHz to 16 kHz; Silero scores each 32 ms on the CPU (near-silent frames skip it); `segment.rs` cuts utterances (speech at 0.5, 320 ms kept before it, 600 ms of silence ends it, under 250 ms of speech is dropped, monologues are cut at 25 s). One queue feeds the model: while someone talks, the utterance so far is transcribed about every second as a draft, then the finished utterance replaces it. Finished utterances go first, and a newer draft replaces one still waiting, so a slow Mac skips drafts instead of falling behind.
- **Who transcribes**, without a backend for now; LiveKit's presence is the lease. `quipcall.can_transcribe` (a model id) marks Macs with a model ready. `quipcall.transcribing` ("<model>@<when>": unix ms it started, but later than every claim it saw, so a handover wins even when clocks disagree) tells everyone who transcribes, before any voice is heard. Asking another Mac is an `ask` data message; it answers Accept or Not now (or accepts by itself, a Setting). A handover is an ask with `handover`: the other Mac starts while yours runs, and yours stops once it sees theirs. The ask carries your claim, and the Mac taking over checks once its model has loaded that it still stands and nobody started since: if you stopped, or stopped and started again, it doesn't start. Stopping takes the claim away first, before the last words are transcribed, so nothing takes over a transcript that was stopped. A start on its way (the model loading, its claim going out) can be stopped too: it then never listens, and takes its claim back. A Mac that asked another to take over passes a Stop on to it, while the handover is pending and for 15 s after it. An ask from an older app, which doesn't carry the claim, is tied to the claim the asker has when the ask arrives. A plain start checks again too that nobody started while it loaded. If two Macs ever transcribe at once (a handover, or two people pressing Transcribe together), the one that started last carries on and the other stops, so both agree without talking; a Mac checks again once its own transcriber is in place, in case the other's claim came while it was starting. When the transcriber leaves, everyone whose Mac can carry on is asked; the first to start wins.
- **Transcript flow:** LiveKit data messages on the topic `quipcall.transcript` (`TranscriptMessage` in `crates/protocol`), not the backend yet. The transcriber sends each line to everyone as it comes, drafts included, and the final lines so far to whoever joins. Every Mac keeps the call's final lines, whoever transcribed them, so after a handover a late joiner still gets the whole transcript. A line says which Mac transcribed it (`by`), and is known by that and its id wherever it's relayed, so history from two Macs mid-handover is one line, not two. Anyone in a call can send on the topic, so apps take lines only from a Mac that transcribes the call, or stopped less than 15 s ago (its last words; `LAST_WORDS_MS`, the same on the web), and the web app checks each line's shape. Lines relayed for another Mac only add what an app doesn't have, or finish a draft it has: they never change or take back a final line. The claim is a participant attribute, and tokens let everyone set their own (the recording notice needs it), so someone could still claim the transcript and send lines; everyone would see them as the transcriber. The backend lease makes the claim exclusive. History goes in messages of at most 14,000 bytes as sent (LiveKit's packets take 15 KiB), so Cyrillic or Chinese lines fit too. Leaving while your Mac transcribes closes the windows at once, but the call goes into history only after its last words are in. Anyone can send `stop`. Lines carry wall-clock times, so each app places them on its own call's timeline. Everyone keeps the transcript in their call history.
- **Bake-off** (`crates/stt/examples/bakeoff.rs`, one paragraph in six languages, macOS voices): Parakeet v3 and Qwen3-ASR are both perfect or one word off in English, Russian, German and French; only Qwen3-ASR knows Chinese (0% CER) and Japanese (3%). Parakeet runs at 160× real time on the GPU and 42× on the CPU, Qwen3-ASR at 39× and 18×. So Parakeet by default, Qwen3-ASR for Chinese, Japanese and Korean. Raising Opus's bitrate for transcription turned out unnecessary: lines through a call came out word for word as from the clean audio.
- **Try it** (Settings): the microphone through AVCaptureSession (48 kHz float, outside any call) into the same pipeline.
- **Cost** (M5 Pro, release build, one voice): memory goes from about 120 MB to 300–385 MB (the model's working buffers on the GPU; its 705 MB of weights are memory-mapped), CPU from about 4% to 4–9%. The transcriber alone held at 277 MB over 21 minutes of audio. Untested: Intel Macs and GPU contention with video.

## Recording (client-side)

- Record is a separate button from Transcribe. Whoever presses it records on their own Mac. There is one recording per call: while someone records, nobody else can start one, and only the person who started it can stop it. If the recorder leaves, the recording stops and stays on their Mac.
- Everyone sees it: the recorder sets the participant attribute `quipcall.recording`, and every app (desktop and web) shows who records. The attribute goes out before the first frame is kept; if the server refuses it, nothing is recorded. Tokens grant `can_update_own_metadata` for this. Two people pressing Record at the same moment can both end up recording; every app keeps showing the notice while either does, and a backend lease fixes the race later.
- Everyone's voice, 10 ms at a time (`crates/core/src/audio_tap.rs`), shared by recording and, later, transcription. Your own mic comes through a hook in our webrtc-sys patch, after echo cancellation, noise suppression and gain: what the call sends, before encoding. It's gated on the mic being unmuted. Remote mics come from LiveKit's audio sinks, decoded at 48 kHz mono. Nothing is copied while nobody listens. Each call has its own hub: leaving finishes the recording on its own time, and if you join another call meanwhile, nothing of it reaches the old file. The recording stops taking anything in (screen included) the moment the call ends, and is finished while the transcript's last words are still being transcribed. Late events from a room still closing are ignored, so an engine reused for the next call keeps its state to itself. Each connect gets its number when it's asked for, so a connection overtaken by a later one (or by leaving) closes instead of taking over. LiveKit has one audio device for the whole app, so your mic is one stream, sent to the call it's live in.
- Audio (`record.rs`, `record_mac.rs`): a thread mixes everyone on its own clock. Each voice gets 60 ms of slack for jitter, a voice that runs ahead is trimmed, and a limiter keeps overlapping voices from clipping. AVAssetWriter encodes AAC at 64 kbps mono into an .m4a with a fragment every 5 s, so a crash keeps all but the last few seconds (a test aborts a writer to check this). AVAssetWriter is part of macOS, so this adds no dependency. About 30 MB an hour. Optionally one track per voice (not built yet).
- Audio and screen: the recorder's own screen or a window, picked in the share picker with the share presets. A ScreenCaptureKit capture of its own delivers IOSurfaces to the recorder, and AVAssetWriter has VideoToolbox encode H.264 into one .mp4 with the audio mix. The average bitrate is 1–6 Mbps by preset, so up to about 2.7 GB an hour at 60 fps; text at 15 fps is about 0.9 GB. Frames are shifted by the mixer's 60 ms of slack, so picture and sound line up. Quipcall's windows are left out, as when sharing. Sharing and recording the same screen runs two captures for now. Reusing the share's would tie the recording to the share: stopping the share would end the picture.
- Each voice in its own file too (a Setting): the mixer hands out every voice's 10 ms along with the mix, and each voice gets its own AAC writer, made when it's first heard and filled with silence up to then, so every file lines up with the mix (`<recording> voices/<name>.m4a`). The voices' files are written on a thread of their own: an hour of silence for someone who joins late mustn't hold up the mix. Silence is encoded a second at a time, about 3 s per hour of it in a release build. Frames wait for that thread in a queue of 10 s; past that the voices' files miss them, and get silence in their place, so they still line up. On Stop the mix is finished first. Names are unique however the volume compares them: "Anna" and "anna" get a file each.
- Home plays the recording under the call's transcript with AVPlayer (part of macOS): a line plays from where it was said, the line playing is outlined.
- Files go to Movies › Quipcall (or a folder chosen in Settings), and only the recorder has them. Uploading them so the whole room gets them comes later, with the backend.

## Alpha scope

Only me and friends, self-hosted on one VPS. About 10 people per call at most. Android users join through the mobile web client.

Deployed at https://quipcall.com ([DEPLOY.md](DEPLOY.md)). The VPS is shared with other projects and already runs nginx with certbot, so we use those rather than Caddy. LiveKit takes all media on one UDP port (7882) with a TCP fallback (7881), so the firewall story is two ports. The Mac app ships as one build per architecture, signed with a self-signed certificate (so updates keep their permissions) until it's worth paying for notarization.
