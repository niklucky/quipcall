//! Run test sounds through a call's microphone processing, offline. Opens no
//! audio device. `dev/denoise-eval.py` builds the sounds, runs this and
//! scores the results:
//!
//!   cargo run --release -p quipcall-core --example denoise_eval -- <dir>
//!
//! Each `<dir>/scenes/<scene>/` holds `mic.pcm` (what the microphone hears)
//! and `far.pcm` (what the speaker plays, for echo cancellation), 48 kHz mono
//! s16le. Every chain below processes every scene with WebRTC's audio
//! processing set up as in a call (`quipcall_test_apm_create` in our
//! webrtc-sys patch), writing `<dir>/out/<scene>/<chain>.wav`, and prints one
//! JSON line per run.

use std::ffi::c_void;
use std::path::Path;
use std::time::{Duration, Instant};

use quipcall_core::denoise::{Denoiser, FRAME, SAMPLE_RATE, Strength};

/// One way to process the microphone.
struct Chain {
    name: &'static str,
    /// WebRTC's noise suppression (kHigh).
    webrtc_ns: bool,
    /// WebRTC's AGC1 (fixed digital), before the filter.
    agc1: bool,
    /// AGC2 (adaptive digital), after the filter.
    agc2: bool,
    filter: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    None,
    Deep(Strength),
    RnNoise,
}

const fn deep(limit_db: f32, pause_limit_db: Option<f32>, input_gain_db: f32) -> Kind {
    Kind::Deep(Strength {
        limit_db: Some(limit_db),
        pause_limit_db,
        noise_only_below_db: -10.0,
        input_gain_db,
    })
}

const CHAINS: &[Chain] = &[
    // QUIPCALL_DENOISE=webrtc: WebRTC's own suppressor.
    Chain {
        name: "webrtc",
        webrtc_ns: true,
        agc1: true,
        agc2: false,
        filter: Kind::None,
    },
    // As first shipped (053cdbd): AGC1 before the model, no limit.
    Chain {
        name: "dfn",
        webrtc_ns: false,
        agc1: true,
        agc2: false,
        filter: Kind::Deep(Strength::MODEL_DEFAULT),
    },
    // What calls use (Strength::CALLS), and alternatives.
    Chain {
        name: "dfn12p30g12-agc2",
        webrtc_ns: false,
        agc1: false,
        agc2: true,
        filter: deep(12.0, Some(30.0), 12.0),
    },
    Chain {
        name: "dfn15p30g12-agc2",
        webrtc_ns: false,
        agc1: false,
        agc2: true,
        filter: deep(15.0, Some(30.0), 12.0),
    },
    Chain {
        name: "dfn12g12-agc2",
        webrtc_ns: false,
        agc1: false,
        agc2: true,
        filter: deep(12.0, None, 12.0),
    },
    Chain {
        name: "dfn18g12-agc2",
        webrtc_ns: false,
        agc1: false,
        agc2: true,
        filter: deep(18.0, None, 12.0),
    },
    // Close to Telemost's desktop default: RNNoise, then AGC2.
    Chain {
        name: "rnnoise-agc2",
        webrtc_ns: false,
        agc1: false,
        agc2: true,
        filter: Kind::RnNoise,
    },
];

/// The filter the hook calls, with the time it took.
struct Filter {
    state: State,
    busy: Duration,
}

enum State {
    None,
    Deep(Box<Denoiser>),
    RnNoise(Box<nnnoiseless::DenoiseState<'static>>, Vec<f32>),
}

impl Filter {
    fn new(kind: Kind) -> Self {
        let state = match kind {
            Kind::None => State::None,
            Kind::Deep(strength) => {
                let mut d = Denoiser::new().expect("load DeepFilterNet");
                d.set_strength(strength);
                State::Deep(Box::new(d))
            }
            Kind::RnNoise => State::RnNoise(nnnoiseless::DenoiseState::new(), vec![0.0; FRAME]),
        };
        Filter {
            state,
            busy: Duration::ZERO,
        }
    }
}

type ProcessFn = unsafe extern "C" fn(*const c_void, *const *mut f32, usize, usize, i32);

unsafe extern "C" {
    fn quipcall_test_apm_create(
        sample_rate_hz: i32,
        noise_suppression: bool,
        gain_control1: bool,
        gain_control2: bool,
        process: Option<ProcessFn>,
        context: *const c_void,
    ) -> *mut c_void;
    fn quipcall_test_apm_process(apm: *mut c_void, render: *const i16, capture: *mut i16) -> i32;
    fn quipcall_test_apm_destroy(apm: *mut c_void);
}

/// The hook: WebRTC's capture thread hands each processed frame here.
unsafe extern "C" fn hook(
    context: *const c_void,
    channels: *const *mut f32,
    num_channels: usize,
    num_frames: usize,
    sample_rate_hz: i32,
) {
    assert!(num_channels >= 1 && num_frames == FRAME && sample_rate_hz == SAMPLE_RATE);
    // SAFETY: `context` is the `Filter` that `run` keeps alive and only the
    // APM's processing (on this thread) touches; WebRTC passes valid buffers.
    let filter = unsafe { &mut *(context as *mut Filter) };
    let frame = unsafe { std::slice::from_raw_parts_mut(*channels, num_frames) };
    let start = Instant::now();
    match &mut filter.state {
        State::None => {}
        State::Deep(d) => {
            d.process(frame).expect("DeepFilterNet");
        }
        State::RnNoise(state, out) => {
            state.process_frame(out, frame);
            frame.copy_from_slice(out);
        }
    }
    filter.busy += start.elapsed();
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: denoise_eval <dir>"));
    let only: Option<Vec<String>> = std::env::args()
        .nth(2)
        .map(|s| s.split(',').map(String::from).collect());
    let mut scenes: Vec<_> = std::fs::read_dir(dir.join("scenes"))
        .expect("scenes")
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("mic.pcm").exists())
        .collect();
    scenes.sort();

    for chain in CHAINS {
        if only
            .as_ref()
            .is_some_and(|o| !o.iter().any(|n| n == chain.name))
        {
            continue;
        }
        for scene in &scenes {
            let name = scene.file_name().unwrap().to_string_lossy();
            let mic = read_pcm(&scene.join("mic.pcm"));
            let far = read_pcm(&scene.join("far.pcm"));
            let (out, filter_time, total_time) = run(chain, &mic, &far);
            let out_dir = dir.join("out").join(&*name);
            std::fs::create_dir_all(&out_dir).unwrap();
            write_wav(&out_dir.join(format!("{}.wav", chain.name)), &out);
            let frames = (mic.len() / FRAME) as f64;
            println!(
                "{{\"scene\":\"{name}\",\"chain\":\"{}\",\"filter_us\":{:.0},\"apm_us\":{:.0}}}",
                chain.name,
                filter_time.as_secs_f64() * 1e6 / frames,
                total_time.as_secs_f64() * 1e6 / frames,
            );
        }
    }
}

/// One chain over one scene: the processed audio, the filter's time and the
/// whole APM's time.
fn run(chain: &Chain, mic: &[i16], far: &[i16]) -> (Vec<i16>, Duration, Duration) {
    let mut filter = Box::new(Filter::new(chain.filter));
    let context = &mut *filter as *mut Filter as *const c_void;
    let process: Option<ProcessFn> = match chain.filter {
        Kind::None => None,
        _ => Some(hook),
    };
    // SAFETY: the APM is destroyed below, before `filter` goes away.
    let apm = unsafe {
        quipcall_test_apm_create(
            SAMPLE_RATE,
            chain.webrtc_ns,
            chain.agc1,
            chain.agc2,
            process,
            context,
        )
    };
    assert!(!apm.is_null());
    let mut out = Vec::with_capacity(mic.len());
    let silence = [0i16; FRAME];
    let mut total = Duration::ZERO;
    for (i, frame) in mic.as_chunks::<FRAME>().0.iter().enumerate() {
        let render = far.get(i * FRAME..(i + 1) * FRAME).unwrap_or(&silence);
        let mut capture = *frame;
        let start = Instant::now();
        let err = unsafe { quipcall_test_apm_process(apm, render.as_ptr(), capture.as_mut_ptr()) };
        total += start.elapsed();
        assert_eq!(err, 0, "APM error");
        out.extend_from_slice(&capture);
    }
    unsafe { quipcall_test_apm_destroy(apm) };
    (out, filter.busy, total)
}

fn read_pcm(path: &Path) -> Vec<i16> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect()
}

fn write_wav(path: &Path, samples: &[i16]) {
    let data = samples.len() as u32 * 2;
    let rate = SAMPLE_RATE as u32;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + data).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    for s in samples {
        wav.extend(s.to_le_bytes());
    }
    std::fs::write(path, wav).unwrap();
}
