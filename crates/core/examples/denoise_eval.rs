//! Compare noise suppressors offline on speech mixed with real noise. Opens no
//! audio device. `dev/denoise-eval.py` prepares the inputs and runs this:
//!
//!   cargo run --release -p quipcall-core --example denoise_eval -- <dir>
//!
//! Reads `<dir>/speech.pcm` and `<dir>/noise/*.pcm` (48 kHz mono s16le),
//! writes listening samples to `<dir>/out/` and prints one JSON line per
//! scenario and filter.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df::tract::{DfParams, RuntimeParams};
use livekit::webrtc::native::apm::AudioProcessingModule;
use quipcall_core::denoise::{Denoiser, FRAME, SAMPLE_RATE};

const RATE: usize = SAMPLE_RATE as usize;
/// Noise level against speech, both measured over their active parts.
const SNR_DB: f32 = 5.0;
/// Noise alone before and after the speech.
const PAD: usize = RATE * 3 / 2;

enum Filter {
    None,
    WebRtc(AudioProcessingModule, Vec<i16>),
    RnNoise(Box<nnnoiseless::DenoiseState<'static>>, Vec<f32>),
    Deep(Box<Denoiser>),
}

impl Filter {
    fn new(name: &str) -> Self {
        let deep = |params: RuntimeParams| {
            Filter::Deep(Box::new(
                Denoiser::with(DfParams::default(), &params).expect("load DeepFilterNet"),
            ))
        };
        let defaults = || RuntimeParams::default_with_ch(1);
        match name {
            "none" => Filter::None,
            "webrtc" => Filter::WebRtc(
                AudioProcessingModule::new(false, false, false, true),
                vec![0; FRAME],
            ),
            "rnnoise" => Filter::RnNoise(nnnoiseless::DenoiseState::new(), vec![0.0; FRAME]),
            "deepfilternet" => deep(defaults()),
            "deepfilternet-pf" => deep(defaults().with_post_filter(0.02)),
            "deepfilternet-30db" => deep(defaults().with_atten_lim(30.0)),
            _ => panic!("unknown filter {name}"),
        }
    }

    fn process(&mut self, frame: &mut [f32]) {
        match self {
            Filter::None => {}
            Filter::WebRtc(apm, pcm) => {
                for (d, s) in pcm.iter_mut().zip(frame.iter()) {
                    *d = s.clamp(-32768.0, 32767.0) as i16;
                }
                apm.process_stream(pcm, SAMPLE_RATE, 1).expect("apm");
                for (d, s) in frame.iter_mut().zip(pcm.iter()) {
                    *d = *s as f32;
                }
            }
            Filter::RnNoise(state, out) => {
                state.process_frame(out, frame);
                frame.copy_from_slice(out);
            }
            Filter::Deep(d) => d.process(frame).expect("process"),
        }
    }
}

const FILTERS: [&str; 6] = [
    "none",
    "webrtc",
    "rnnoise",
    "deepfilternet",
    "deepfilternet-pf",
    "deepfilternet-30db",
];

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: denoise_eval <dir>"));
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // Memory and load time of one model, before anything else is allocated.
    let before = footprint_mb();
    let start = Instant::now();
    let model = Denoiser::new().expect("load DeepFilterNet");
    let load = start.elapsed();
    let after = footprint_mb();
    println!(
        "{{\"model\":\"deepfilternet\",\"load_ms\":{:.0},\"footprint_mb\":{:.1},\"declared_delay_ms\":{:.1}}}",
        load.as_secs_f64() * 1e3,
        after - before,
        model.delay() as f64 * 1e3 / RATE as f64
    );
    drop(model);

    let mut speech = vec![0.0; PAD];
    speech.extend(read_pcm(&dir.join("speech.pcm")));
    speech.extend(vec![0.0; PAD]);
    let speaking = activity(&speech, 40.0);

    let mut noises: Vec<(String, Vec<f32>)> = std::fs::read_dir(dir.join("noise"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pcm"))
        .map(|p| {
            (
                p.file_stem().unwrap().to_string_lossy().into(),
                read_pcm(&p),
            )
        })
        .collect();
    noises.sort_by(|a, b| a.0.cmp(&b.0));

    let mut scenarios = vec![("clean".to_string(), speech.clone())];
    for (name, noise) in &noises {
        scenarios.push((name.clone(), mix(&speech, &speaking, noise)));
    }

    // Each filter's delay, from clean speech, to line outputs up with it.
    let mut delays = Vec::new();
    for name in FILTERS {
        let (output, _) = run(name, &speech);
        delays.push(delay(&speech, &output));
    }

    for (scenario, input) in &scenarios {
        let pauses: Vec<bool> = speaking.iter().map(|s| !s).collect();
        for (name, &lag) in FILTERS.iter().zip(&delays) {
            let (output, times) = run(name, input);
            write_wav(&out.join(format!("{scenario}-{name}.wav")), &output);
            let aligned = &output[lag..];
            let n = aligned.len();
            let si_sdr = si_sdr(&speech[..n], aligned);
            // Noise left where nobody speaks, against the input there.
            let left = rms_where(&aligned[..n], &pauses[..n / FRAME]);
            let was = rms_where(&input[..n], &pauses[..n / FRAME]);
            let mut sorted = times.clone();
            sorted.sort();
            let mean = times.iter().sum::<Duration>() / times.len() as u32;
            let p99 = sorted[sorted.len() * 99 / 100];
            let max = *sorted.last().unwrap();
            println!(
                "{{\"scenario\":\"{scenario}\",\"filter\":\"{name}\",\"si_sdr_db\":{si_sdr:.2},\
                 \"pause_reduction_db\":{:.1},\"delay_ms\":{:.1},\"frame_mean_us\":{},\
                 \"frame_p99_us\":{},\"frame_max_us\":{},\"cpu_pct\":{:.2}}}",
                db(was / left.max(1e-3)),
                lag as f64 * 1e3 / RATE as f64,
                mean.as_micros(),
                p99.as_micros(),
                max.as_micros(),
                mean.as_secs_f64() / 0.010 * 100.0,
            );
        }
    }
}

/// Run one filter over `input`, a fresh instance per run.
fn run(name: &str, input: &[f32]) -> (Vec<f32>, Vec<Duration>) {
    let mut filter = Filter::new(name);
    let mut output = input.to_vec();
    let mut times = Vec::with_capacity(input.len() / FRAME);
    for frame in output.as_chunks_mut::<FRAME>().0 {
        let start = Instant::now();
        filter.process(frame);
        times.push(start.elapsed());
    }
    let whole = output.len() / FRAME * FRAME;
    output.truncate(whole);
    (output, times)
}

/// Speech plus noise at [`SNR_DB`], the noise looped over the whole clip.
fn mix(speech: &[f32], speaking: &[bool], noise: &[f32]) -> Vec<f32> {
    let looped: Vec<f32> = noise.iter().copied().cycle().take(speech.len()).collect();
    let speech_rms = rms_where(speech, speaking);
    let noise_rms = rms_where(&looped, &activity(&looped, 30.0));
    let gain = speech_rms / noise_rms / 10f32.powf(SNR_DB / 20.0);
    speech
        .iter()
        .zip(&looped)
        .map(|(s, n)| (s + n * gain).clamp(-32768.0, 32767.0))
        .collect()
}

/// Frames within `range_db` of the loudest one.
fn activity(samples: &[f32], range_db: f32) -> Vec<bool> {
    let energy: Vec<f32> = samples
        .chunks(FRAME)
        .map(|f| f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32)
        .collect();
    let peak = energy.iter().cloned().fold(0.0, f32::max);
    let floor = peak / 10f32.powf(range_db / 10.0);
    energy.iter().map(|&e| e > floor).collect()
}

fn rms_where(samples: &[f32], frames: &[bool]) -> f32 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for (frame, &on) in samples.chunks(FRAME).zip(frames) {
        if on {
            sum += frame.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
            n += frame.len();
        }
    }
    (sum / n.max(1) as f64).sqrt() as f32
}

/// Scale-invariant signal-to-distortion ratio of `estimate` against `target`.
fn si_sdr(target: &[f32], estimate: &[f32]) -> f32 {
    let dot: f64 = target
        .iter()
        .zip(estimate)
        .map(|(&t, &e)| t as f64 * e as f64)
        .sum();
    let energy: f64 = target.iter().map(|&t| (t as f64).powi(2)).sum();
    let scale = dot / energy;
    let (mut signal, mut error) = (0.0f64, 0.0f64);
    for (&t, &e) in target.iter().zip(estimate) {
        let s = scale * t as f64;
        signal += s * s;
        error += (e as f64 - s).powi(2);
    }
    (10.0 * (signal / error.max(1e-9)).log10()) as f32
}

/// The lag (≤ 100 ms) at which `output` best matches `reference`.
fn delay(reference: &[f32], output: &[f32]) -> usize {
    // Two busy seconds of speech are enough.
    let start = PAD + RATE;
    let window = &reference[start..start + 2 * RATE];
    (0..RATE / 10)
        .max_by(|&a, &b| {
            let corr = |lag: usize| -> f64 {
                window
                    .iter()
                    .zip(&output[start + lag..])
                    .map(|(&r, &o)| r as f64 * o as f64)
                    .sum()
            };
            corr(a).total_cmp(&corr(b))
        })
        .unwrap()
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.log10()
}

fn read_pcm(path: &Path) -> Vec<f32> {
    std::fs::read(path)
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b) as f32)
        .collect()
}

fn write_wav(path: &Path, samples: &[f32]) {
    let data = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + data).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend((RATE as u32).to_le_bytes());
    wav.extend((RATE as u32 * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    for s in samples {
        wav.extend((s.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, wav).unwrap();
}

/// This process's physical footprint, as Activity Monitor shows it.
fn footprint_mb() -> f64 {
    let out = std::process::Command::new("vmmap")
        .args(["--summary", &std::process::id().to_string()])
        .output()
        .expect("vmmap");
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text
        .lines()
        .find_map(|l| l.strip_prefix("Physical footprint:"))
        .expect("footprint")
        .trim();
    let (num, unit) = value.split_at(value.len() - 1);
    let num: f64 = num.parse().unwrap();
    match unit {
        "K" => num / 1024.0,
        "M" => num,
        "G" => num * 1024.0,
        _ => num / 1048576.0,
    }
}
