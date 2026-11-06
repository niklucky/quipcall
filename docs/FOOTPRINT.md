# Footprint measurements

Budget: see [PLAN.md](PLAN.md#footprint-budget). Always measure **release** builds (`cargo build --release -p quipcall-desktop`).

Measure memory as the **physical footprint** (`dev/measure.sh <pid>`, or `footprint <pid>`), not `ps` RSS. The footprint is what Activity Monitor shows and includes GPU and IOSurface memory; RSS mostly misses it. The first two entries below used RSS, so their memory numbers are too low.

## 2026-11-03 — Stage 1, first data point (preliminary)

Setup: Apple M5 Pro, macOS 27.0, release build. 1100×720 window. Receiving one 720p30 VP8 camera stream (headless Chrome, fake device). `QUIPCALL_NO_MIC=1`, so no audio device. Plain GPUI UI (no gpui-component yet). Remote video goes I420 → full-range NV12 on the CPU.

| Metric | Measured | Budget |
|---|---|---|
| Binary size (not yet an `.app` bundle) | 23.9 MB (10.9 MB gzipped) | ≤ 40 MB bundle |
| RSS in a call, receiving 1 × 720p | ~110 MB | ≤ 200 MB (1:1, both directions) |
| CPU in a call, receiving 1 × 720p | 8–9% of one core | ≤ 15% (1:1, both directions) |

Not measured yet: idle in the menu bar (no shell yet), sending camera, screen share, 10-person grid, audio.

## 2026-11-03 — Stage 1, receive path + mic

Same machine, release build, same window. Microphone on (WebRTC AEC/NS/AGC, Opus + RED), speaker open, receiving one 720p30 camera stream from headless Chrome. `ps` samples every 5 s for 30 s, after the 720p layer was selected.

| Remote codec | Render path | CPU (one core) | RSS |
|---|---|---|---|
| H.264 (VideoToolbox decode) | `CVPixelBuffer` handed to GPUI as is (zero-copy) | 6–8.5%, ~7.5% avg | 104–110 MB |
| VP8 (software decode) | I420 → NV12 conversion on the CPU | 8–14%, ~12.5% avg | 113–116 MB |

Binary: 25.2 MB (was 23.9 MB; +1.3 MB for camera capture and the AVFoundation bindings).

Takeaways:
- Hardware decode + zero-copy saves ~40% CPU, so everyone publishes camera video as H.264 (desktop always, web by default).
- Memory barely moves with the codec; most of the ~105 MB is the baseline (libwebrtc, GPUI, fonts). Still to break down.

## 2026-11-03 — Stage 1, full pipeline (physical footprint)

Same machine, release build, 1100×720 window, mic on in every row. The desktop camera is the synthetic one (`QUIPCALL_FAKE_CAMERA=1`), which goes through the real publish path (IOSurface `CVPixelBuffer` → VideoToolbox H.264, 3 simulcast layers). Samples every 5 s for 30 s with `dev/measure.sh`.

| Scenario | CPU (one core) | Footprint | Budget |
|---|---|---|---|
| Receiving 1 × 720p30 H.264 | 5–7.6%, ~6.4% avg | ~170 MB | — |
| 1:1 both ways: sending 720p30 H.264 simulcast, receiving 720p30 H.264 | 7.3–12%, ~8.7% avg | ~189 MB | ≤ 200 MB, ≤ 15% |
| 10-person grid: 9 × H.264 simulcast (`lk load-test`), 3×3 tiles on the medium layer | 12–17%, ~15% avg | ~199 MB | ≤ 350 MB |

The web receiver got the desktop's 720p30 with 0 dropped frames. Binary: 25.2 MB.

Where the ~170 MB baseline goes (`footprint`): ~90 MB GPU memory owned by the window (GPUI's Metal renderer), ~38 MB IOSurfaces (decoded frames, our pools, drawables), ~19 MB heap, the rest is libraries. Our own code is a small part of it; the GPU memory is the thing to look at.

**Bug found while measuring:** every video frame leaked an IOSurface (3.3 GB after a minute of sending). libwebrtc's Objective-C frame wrappers are autoreleased, and our threads (tokio workers, the synthetic camera) never drained an autorelease pool. Frame callbacks now run inside one. RSS barely showed it, which is how we found out RSS is the wrong metric.

## 2026-11-03 — Stage 2, minimal mode with 10 circles

Release build, mic on, sending the synthetic 720p camera. 9 `lk load-test` H.264 publishers, so 10 circles (you at 200 px, others at 160 px, each on the 360p layer) plus the glass toolbar: 11 windows.

| Scenario | CPU (one core) | Footprint |
|---|---|---|
| Minimal mode, 10 circles + toolbar | 17–26%, ~23% avg | ~207 MB, flat |

Small windows are cheap in memory (the 10-tile grid window was ~199 MB). CPU is higher than the grid (~15%) because each circle is its own window with its own Metal drawable and frame. If it matters: draw small circles at 15 fps, or share one renderer between windows.

## 2026-11-03 — Stage 2, screen share (ScreenCaptureKit)

Release build, sharing one of Quipcall's own windows (1100×721 pt) to a headless Chrome receiver, while also receiving its 720p camera. ScreenCaptureKit delivers IOSurface NV12 frames that go straight to the encoder.

| Preset | Codec | Sent | CPU (one core) | Footprint |
|---|---|---|---|---|
| 15 fps · text | VP9 (software) | 2200×1440 @ 15 | 22–28%, ~24% | ~460 MB |
| 15 fps · text | H.264 (VideoToolbox) | 2200×1440 @ 15 | 9–12.5%, ~10.5% | ~173 MB |
| 30 fps · motion | H.264 (VideoToolbox) | 1920×1256 @ 30 | 9–16%, ~12.7% | ~188 MB |

VP9 is out: software VP9 at this size more than doubles memory and CPU. Everything is H.264 now, including the web client's screen share. The window was mostly still; a busy screen costs more encoder time but the same memory.

Ideas to try if we need headroom:
- Direct ScreenCaptureKit capture, letting SCK scale on the GPU, for screen share.
- Find out what the ~90 MB of GPU memory is (drawables at 2× scale, glyph/path atlases, MSAA textures for paths) and whether GPUI lets us shrink it.
- Check whether that GPU memory goes away when the window closes (matters for the menu-bar idle budget).

## 2026-11-04 — Stage 2, your own share preview

Release build, alone in a room, `QUIPCALL_NO_MIC=1`, sharing the synthetic 1920×1200 screen (`QUIPCALL_FAKE_SCREEN`) at 30 fps · motion in minimal mode. The preview draws the capture's own IOSurfaces and repaints at most 15 times a second. "No preview" is the same build with the preview sink left out. Samples every 5 s for 30 s.

| Scenario | CPU (one core) | Footprint |
|---|---|---|
| Sharing, no camera, no preview | ~1.3% | ~49 MB |
| Sharing, no camera, preview | ~3.4% | ~136 MB |
| Sharing + synthetic camera, no preview | ~5.5% | ~148 MB |
| Sharing + synthetic camera, preview | ~5.5% | ~150 MB |

The preview itself is cheap: with any video already on screen (your camera, or anyone's) it adds about 2 MB and no measurable CPU. The ~88 MB jump without a camera is GPUI's GPU memory ("owned unmapped (graphics)" in `vmmap`): macOS releases it a few seconds after the last window stops drawing, and any repainting video keeps it. Your own camera circle alone holds the same 88.5 MB. So only an audio-only call where you share pays for it, and that is the GPU-memory item below, not the preview.

Two desktops can share at the same time (synthetic screens), each receiving the other's share, and a web participant sees both.

## 2026-11-05 — Stage 2, your camera inside the preview

Same setup, sharing at 30 fps with the synthetic camera on: the previous build (camera circle beside the preview, rounded by the window mask) against this one (camera inside the preview's corner, drawn as ~25 bands clipped to a circle under an opaque ring). Samples every 5 s; two runs of the old build, one of the new (the display went to sleep during the second).

| Build | CPU (one core) | Footprint |
|---|---|---|
| Camera beside the preview | ~5.5–6.0% | ~147–152 MB |
| Camera inside the preview | ~5.8–6.5% | ~152–155 MB |

The difference is within the run-to-run spread: about half a percent of CPU and 3 MB at most. All circles now start at 96 px instead of 200 / 160, so they decode the lowest simulcast layer more often.

The bands themselves, isolated: the same build drawing your camera square (one draw) against round (~25 draws), three alternating runs of each, 18 samples: 5.0% against 4.9% CPU, footprint 143–147 MB in both. Nothing measurable. Your circle on the preview stays 96 px whatever size your own circle is, so the band count doesn't grow with it.

## 2026-11-06 — Noise suppression: DeepFilterNet 3 vs WebRTC vs RNNoise

Apple M5 Pro, macOS 27.0, release build. `python3 dev/denoise-eval.py` mixes TTS speech with real recordings (a dog, a food wrapper, paper, typing, traffic) at 5 dB below the speech, runs each filter over it offline, and saves the outputs to listen to. "Pauses" is how much quieter the noise gets where nobody speaks. PESQ (speech quality, 1–4.6) and STOI (intelligibility, 0–1) compare the output with the clean speech. WebRTC runs at its default level here; calls use its "high" level, which takes a few dB more off steady noise.

| Noise | Pauses: WebRTC / RNNoise / DeepFilterNet | PESQ: none / WebRTC / RNNoise / DFN | STOI: none / WebRTC / RNNoise / DFN |
|---|---|---|---|
| Dog barking | 0.6 / 0.9 / **22.1 dB** | 1.73 / 1.82 / 1.78 / **1.97** | 0.955 / 0.940 / 0.956 / 0.929 |
| Food wrapper | 3.2 / 7.6 / **22.8 dB** | 1.13 / 1.30 / 1.40 / **1.83** | 0.931 / 0.924 / 0.944 / 0.867 |
| Paper | 1.2 / 8.0 / **29.5 dB** | 1.23 / 1.27 / 1.59 / **1.66** | 0.926 / 0.913 / 0.960 / 0.867 |
| Typing | 4.7 / 21.0 / **47.0 dB** | 1.05 / 1.08 / 1.20 / **1.53** | 0.774 / 0.781 / 0.879 / **0.914** |
| Traffic | 2.5 / 15.2 / **25.0 dB** | 1.12 / 1.29 / 1.56 / **1.78** | 0.903 / 0.902 / 0.923 / 0.873 |
| Clean speech | — | 4.64 / 4.35 / 4.55 / 3.94 | 1.000 / 0.980 / 1.000 / 0.997 |

- WebRTC's suppressor barely touches sudden noise, as expected: it tracks steady noise.
- RNNoise is cheap and keeps speech intact, but lets barking and most rustling through.
- DeepFilterNet removes almost all of it between words and scores best on quality. Where loud noise overlaps speech, it shaves a little intelligibility (STOI 0.87 against 0.93 unfiltered); it sounds cleaner but slightly processed. Its attenuation limit and post-filter (`-30db`, `-pf` in the script) changed little.

Cost per 10 ms frame (one core):

| Filter | Mean | p99 | CPU | Delay |
|---|---|---|---|---|
| WebRTC | 11 µs | 18 µs | 0.1% | ~11 ms |
| RNNoise (`nnnoiseless`) | 27 µs | 37 µs | 0.3% | 10 ms |
| DeepFilterNet 3 (tract) | 216–552 µs | 760–920 µs | 2.2–5.5% | 30 ms |

The model is cheaper on clean speech: below a speech-to-noise threshold it skips stages.

In the app: the model loads in ~180 ms while the call connects (plus a 30-frame speed check). It holds ~10.5 MB live, but parsing the ONNX file fragments the heap: in a mic-less call (model forced on), the footprint was 13–29 MB higher across runs, and ~12 MB of it stays after leaving, since macOS doesn't take back the freed pages. The model is therefore kept loaded for the rest of the run. Binary: 26.6 → 44.2 MB (+8 MB model, +9.6 MB tract and DeepFilterNet). Building tract's graph crates for size (`opt-level = "s"`) saved 2.2 MB of that for ~5% more time per frame; tract-linalg's kernels are left fast.

The hook itself still needs a real call: while it runs, the log shows `noise suppression, time per 10 ms frame` once per minute of unmuted audio.
