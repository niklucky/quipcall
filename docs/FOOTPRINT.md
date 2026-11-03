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
