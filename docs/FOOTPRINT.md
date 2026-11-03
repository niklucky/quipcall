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

Ideas to try if we need headroom:
- Direct ScreenCaptureKit capture, letting SCK scale on the GPU, for screen share.
- Find out what the ~90 MB of GPU memory is (drawables at 2× scale, glyph/path atlases, MSAA textures for paths) and whether GPUI lets us shrink it.
- Check whether that GPU memory goes away when the window closes (matters for the menu-bar idle budget).
