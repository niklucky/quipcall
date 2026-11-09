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

## 2026-11-07 — Stage 2, minimal mode as one capsule

Release build, sending the synthetic 720p camera, 9 `lk load-test` H.264 publishers: 10 faces at 44 px in one capsule window, each on the lowest layer and drawn as ~15 bands cut to a circle. No microphone (`QUIPCALL_NO_MIC`): a fresh unsigned build would have raised the permission prompt. Samples every 5 s.

| Scenario | CPU (one core) | Footprint |
|---|---|---|
| Capsule, repainting on every frame | 18–27%, ~24% avg | ~133 MB |
| Capsule, repaints capped at 30 fps | 15–21%, ~19% avg | ~135 MB |

Everyone's frames arrive out of step, so without the cap the one window redrew at the display's rate; the cap is in. Against the 10 circles below (~23%, ~207 MB, mic on), memory is clearly lower; CPU isn't directly comparable without the mic (the noise model costs ~4% of a core while unmuted), and the Nov 3 build can't join today's server for a side-by-side.

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

## 2026-11-06 — Noise suppression, round 2: volume drops

*Scored with the first version of the bench; round 4 re-scores it after review.*

A real call found our voice "slightly worse than Telemost, as if we try too hard; mostly volume drops". The test bench (`python3 dev/denoise-eval.py`) now uses real voices (LibriVox: English and Russian, male and female) and 12 noise recordings. It builds 22 scenes: noises, a quiet and a loud speaker, a bark in a pause, two people talking at once, and the other side's voice echoing from laptop speakers. It runs every scene through WebRTC's audio processing set up as in a call, with the filter in its hook (`quipcall_test_apm_create` in the webrtc-sys patch), so gain control and echo cancellation are in the measurement.

What caused the drops: **DeepFilterNet takes a quiet voice for noise.** Through the model alone (18 dB limit, no gain control), voice level change and PESQ by speaking level:

| Voice | -30 dBFS | -40 dBFS | -45 dBFS | -50 dBFS | -55 dBFS |
|---|---|---|---|---|---|
| English, male | -0.2 dB, 4.23 | -0.4 dB, 4.08 | -8.2 dB, 3.06 | -18.2 dB, 3.14 | -18.2 dB, 2.64 |
| Russian, female | -0.6 dB, 4.01 | -17.3 dB, 1.75 | -18.4 dB, 2.32 | -18.4 dB, 2.12 | -18.4 dB, 1.78 |
| English, male, +12 dB into the model | -0.2 dB, 4.24 | -0.2 dB, 4.17 | -0.2 dB, 4.07 | -0.3 dB, 3.95 | -0.4 dB, 3.72 |
| Russian, female, +12 dB into the model | -0.7 dB, 3.84 | -0.6 dB, 4.05 | -0.6 dB, 3.73 | -16.3 dB, 1.78 | -18.3 dB, 1.67 |

Without a limit the voice simply vanishes past the cliff. The female voice reaches it at -40 dBFS, where a laptop microphone at arm's length often sits. A gain into the model, undone after it, moves the cliff about 10 dB lower and costs up to 0.2 PESQ on a normal voice.

Calls now use a 12 dB attenuation limit and a 12 dB input gain, with AGC2 after the model in place of AGC1 before it (`Strength::CALLS`). Means over all 22 scenes:

| | WebRTC | DFN as first shipped | **DFN 12 dB + gain (calls)** | DFN 18 dB + gain | RNNoise + AGC2 (like Telemost) |
|---|---|---|---|---|---|
| Voice dips (quietest 10%, dB) | 2.9 | 8.5 | **4.2** | 5.2 | 3.6 |
| Voice > 6 dB below its level | 3.3% | 12.6% | **4.5%** | 8.8% | 5.1% |
| PESQ | 2.52 | 2.20 | **2.62** | 2.58 | 2.47 |
| STOI | 0.87 | 0.79 | **0.86** | 0.85 | 0.86 |
| Quiet speaker (-45 dBFS), PESQ | 4.25 | 1.19 | **4.12** | 3.88 | 3.44 |
| Bark, noise cut in pauses | 10.1 dB | 44.8 dB | **7.6 dB** | 12.1 dB | 13.1 dB |
| Chips bag, noise cut in pauses | 10.1 dB | 85.8 dB | **10.9 dB** | 17.1 dB | 1.6 dB |
| Own voice while the other side talks | -5.8 dB | -27.8 dB | **-13.8 dB** | -15.4 dB | -4.5 dB |

- WebRTC's noise cuts are mostly its old gain control (AGC1) lifting speech more than pauses; its suppressor itself takes ~1 dB off a bark.
- Moving gain control after the model did not by itself change much: AGC1 recovered within 1 dB after a loud bark (`after` in the script). The input level was the cause.
- RNNoise lets chips through and, in the chips scene, muted voice and noise together for seconds.
- Double talk with laptop speakers is still the weak spot: echo cancellation leaves the voice thinner while the other side talks, and the model then lowers it further. A lower limit helps, and no limit hurts most.
- Telemost (inspected on this Mac, 3.3.3) runs RNNoise on the raw microphone by default, turns off AGC1 for AGC2 at 12 dB/s (as we now do) and disables Opus DTX. DeepFilterNet is only its web client's "high" level.

Cost is unchanged: 0.40 ms per 10 ms frame for the model, plus ~0.05 ms for the rest of WebRTC's processing.

## 2026-11-06 — Noise suppression, round 3: deeper in pauses

*Scored with the first version of the bench; round 4 re-scores it after review.*

Second real call: "sound is better", but hard rustling stayed faintly audible. In a pause nothing masks the rustle and no voice can be lost, so the limit now deepens there. The model's own speech-to-noise estimate tells voice from pause: in noisy pauses 90% of frames sit below -11 dB. After 0.3 s with no frame above -5 dB, the limit glides from 12 to 30 dB at 0.5 dB per frame. It returns to 12 dB at the next voiced frame. Simulated over the scenes, about 4% of speech frames get the pause limit (mostly where loud rustling buries the voice) and about two thirds of noisy-pause time does.

| | 12 dB (round 2) | 18 dB | **12 dB, 30 dB in pauses** | 15 dB, 30 dB in pauses |
|---|---|---|---|---|
| Chips bag, cut in pauses | 10.9 dB | 17.1 dB | **20.6 dB** | 23.5 dB |
| Bark | 7.6 dB | 12.1 dB | **15.0 dB** | 16.3 dB |
| Typing | 11.4 dB | 17.2 dB | **26.8 dB** | 27.3 dB |
| Noise cut, mean | 14.0 dB | 18.7 dB | **22.9 dB** | 24.0 dB |
| Voice dips | 4.22 dB | 5.19 dB | **4.31 dB** | 4.88 dB |
| Voice > 6 dB below its level | 4.5% | 8.8% | **5.0%** | 7.2% |
| PESQ | 2.62 | 2.58 | **2.63** | 2.61 |

The voice is as before, and noise in pauses drops twice as far. The bench doesn't score the trade-off: with steady noise (a fan), the background now rises while you talk and fades in pauses. The fade is gradual, and the rise is masked by the voice.

## 2026-11-06 — Noise suppression, round 4: the bench, corrected

Two rounds of review found flaws in the bench that had flattered some chains:
- Gain control and the model were still settling when scoring began.
- The noise average counted scenes without noise.
- One voice's correlation peak set every scene's delay.
- Double talk was compared with a different passage, and then with a control that also dropped the far side's playback.

The warm-up is now 40 s: the same 10 s of speech four times, after which a quiet voice's level holds steady. Delays come from phase-only correlation and must agree across all four clean voices. Noise is only scored where there is noise. Double talk is compared with the same words, the far side playing but not echoing.

It also turned up two model behaviours:
- **Digital silence.** DeepFilterNet skips frames below -70 dBFS (in the model) without updating its state, and after stretches of such digital silence it held a quiet voice down for ~12 s. Those frames now get a faint hiss (-60 dBFS RMS in the model), which keeps it running. A real microphone's own noise is far above that, so calls are otherwise unchanged; hissing every frame instead made the model trim voices.
- **Debug builds.** tract's debug-only graph check rejects DeepFilterNet's model, so debug builds (`make desktop`) silently fell back to WebRTC's suppressor. Its debug assertions are now off (`Cargo.toml`).

The LibriVox voices have studio-silent pauses. Once the model hears any floor there, it trims some of a clean voice, so these clean-speech scores (lower than before) are the realistic ones. All 23 scenes:

| | WebRTC | DFN as first shipped | 12 dB | **12 dB, 30 in pauses (calls)** | 15/30 | 18 dB | RNNoise + AGC2 |
|---|---|---|---|---|---|---|---|
| Voice dips (dB) | 2.9 | 5.9 | 4.3 | **4.4** | 4.8 | 5.0 | 3.6 |
| Voice > 6 dB below its level | 3.0% | 10.0% | 4.6% | **5.3%** | 7.4% | 8.6% | 5.2% |
| Noise cut in pauses (mean) | 9.0 dB | 77.8 dB | 10.7 dB | **20.7 dB** | 21.8 dB | 16.0 dB | 23.2 dB |
| Chips bag | 10.5 dB | 85.7 dB | 11.9 dB | **26.1 dB** | 26.9 dB | 17.1 dB | 1.5 dB |
| Clicks | 12.9 dB | 79.1 dB | 15.2 dB | **26.6 dB** | 26.5 dB | 21.3 dB | 16.4 dB |
| Bark | 9.9 dB | 64.5 dB | 8.8 dB | **10.0 dB** | 12.6 dB | 13.1 dB | 12.7 dB |
| PESQ (mean) | 2.50 | 1.89 | 2.30 | **2.33** | 2.29 | 2.23 | 2.46 |
| PESQ, the four clean voices | 4.15 | 2.72 | 3.58 | **3.67** | 3.51 | 3.31 | 3.61 |
| PESQ, quiet female voice (-45 dBFS) | 4.27 | 1.59 | 2.59 | **2.60** | 2.53 | 2.39 | 4.27 |
| STOI (mean) | 0.87 | 0.82 | 0.87 | **0.87** | 0.86 | 0.85 | 0.86 |
| Own voice while the other side talks | -1.4 dB | -0.9 dB | -2.3 dB | **-2.3 dB** | -2.4 dB | -2.5 dB | -1.4 dB |

- **Against the first shipped version,** the calls setting more than halves the dropouts and leaves the voice dipping 1.5 dB less. It takes 21 dB out of pauses, where the first version removed nearly everything (78 dB).
- **Against WebRTC's suppressor,** it takes over twice as much out of pauses, and far more chips and clicks. The cost is 1.5 dB more dipping and lower clean-speech quality; WebRTC's keeps clean speech best.
- **RNNoise** is close to WebRTC's on the voice and best on steady noise (cafe, traffic, siren), but lets chips and music through, and in the chips scene it mutes the voice outright.
- **Double talk,** compared like for like, costs the voice about 2 dB: barely more than WebRTC alone.
- **Open question:** the bench scores DeepFilterNet's clean speech, and quiet voices, below what the second real call reported ("sound is better"). Recordings from the user's own microphone would settle it.
