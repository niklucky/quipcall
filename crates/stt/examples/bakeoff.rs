//! A small bake-off: each model on the same paragraph in six languages,
//! spoken by macOS's `say` voices. Word error rate (character error rate for
//! Chinese and Japanese) and speed, on the GPU and on the CPU.
//!
//!   QUIPCALL_STT_MODELS=<dir> cargo run --release -p quipcall-stt --example bakeoff
//!
//! Synthetic voices are cleaner than people on a call: the numbers compare
//! models, they don't predict a real call's accuracy.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use quipcall_stt::catalog::{self, ModelInfo};
use transcribe_cpp as tc;

const SAMPLES: &[(&str, &str, &str)] = &[
    (
        "en",
        "Samantha",
        "Good morning everyone. Let's start with the release plan for next week. The download page needs a new screenshot before Friday, and the installer should be signed again.",
    ),
    (
        "ru",
        "Milena",
        "Доброе утро всем. Давайте начнём с плана релиза на следующую неделю. Странице загрузки нужен новый скриншот до пятницы, а установщик нужно подписать заново.",
    ),
    (
        "de",
        "Anna",
        "Guten Morgen zusammen. Lass uns mit dem Plan für die nächste Woche anfangen. Die Download Seite braucht bis Freitag einen neuen Screenshot.",
    ),
    (
        "fr",
        "Eddy (French (France))",
        "Bonjour à tous. Commençons par le plan de la semaine prochaine. La page de téléchargement a besoin d'une nouvelle capture d'écran avant vendredi.",
    ),
    (
        "zh",
        "Eddy (Chinese (China mainland))",
        "大家早上好。我们先从下周的发布计划开始。下载页面在周五之前需要一张新的截图。",
    ),
    (
        "ja",
        "Eddy (Japanese (Japan))",
        "皆さん、おはようございます。来週のリリース計画から始めましょう。ダウンロードページには金曜日までに新しいスクリーンショットが必要です。",
    ),
];

fn main() {
    let dir = PathBuf::from(std::env::var("QUIPCALL_STT_MODELS").expect("QUIPCALL_STT_MODELS"));
    let tmp = std::env::temp_dir().join(format!("qc-bakeoff-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let clips: Vec<(&str, &str, Vec<f32>)> = SAMPLES
        .iter()
        .map(|(lang, voice, text)| (*lang, *text, say(voice, text, &tmp)))
        .collect();
    let _ = std::fs::remove_dir_all(&tmp);
    tc::disable_logging();

    for model in catalog::MODELS.iter().copied() {
        let path = dir.join(model.file);
        if !path.exists() {
            println!("{}: not downloaded", model.name);
            continue;
        }
        for backend in [tc::Backend::Auto, tc::Backend::Cpu] {
            run(model, &path, backend, &clips);
        }
    }
}

fn run(model: &ModelInfo, path: &Path, backend: tc::Backend, clips: &[(&str, &str, Vec<f32>)]) {
    let loaded = tc::Model::load_with(
        path,
        &tc::ModelOptions {
            backend,
            ..Default::default()
        },
    )
    .expect("load");
    let mut session = loaded.session().expect("session");
    // Warm up (Metal compiles its shaders on the first run).
    let _ = session.run(&vec![0.0; 16_000], &tc::RunOptions::default());
    let where_ = if backend == tc::Backend::Cpu {
        "CPU"
    } else {
        "GPU"
    };
    println!("\n{} on the {where_} ({})", model.name, loaded.backend());
    let (mut audio, mut compute) = (0.0, 0.0);
    for (lang, reference, pcm) in clips {
        let begun = Instant::now();
        let text = session
            .run(pcm, &tc::RunOptions::default())
            .map(|t| t.text)
            .unwrap_or_else(|e| format!("<{e}>"));
        let took = begun.elapsed().as_secs_f64();
        let secs = pcm.len() as f64 / 16_000.0;
        audio += secs;
        compute += took;
        let by_chars = matches!(*lang, "zh" | "ja");
        let error = error_rate(reference, &text, by_chars);
        let speaks = model.languages.contains(lang);
        println!(
            "  {lang}  {}{:5.1}%  {:6.0}× real time  {}",
            if by_chars { "CER " } else { "WER " },
            error * 100.0,
            secs / took,
            if speaks {
                text.trim().to_owned()
            } else {
                format!("(not one of its languages) {}", text.trim())
            }
        );
    }
    println!("  overall {:.0}× real time", audio / compute);
}

/// `say` at 16 kHz, as floats.
fn say(voice: &str, text: &str, dir: &Path) -> Vec<f32> {
    let path = dir.join(format!("{}.wav", voice.replace(' ', "_")));
    let ok = Command::new("say")
        .args(["-v", voice, "-o"])
        .arg(&path)
        .args(["--file-format=WAVE", "--data-format=LEF32@16000", text])
        .status()
        .expect("say")
        .success();
    assert!(ok, "say -v {voice}");
    let wav = std::fs::read(&path).unwrap();
    let at = wav.windows(4).position(|w| w == b"data").unwrap() + 8;
    let (samples, _) = wav[at..].as_chunks::<4>();
    samples.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Lowercase, no punctuation, ё as е: what a person would count as the same.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect()
}

fn error_rate(reference: &str, hypothesis: &str, by_chars: bool) -> f64 {
    let (r, h) = (normalize(reference), normalize(hypothesis));
    let units = |s: &str| -> Vec<String> {
        if by_chars {
            s.chars()
                .filter(|c| !c.is_whitespace())
                .map(String::from)
                .collect()
        } else {
            s.split_whitespace().map(String::from).collect()
        }
    };
    let (r, h) = (units(&r), units(&h));
    // Levenshtein distance over words (or characters).
    let mut row: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, hw) in h.iter().enumerate() {
            let next = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(rw != hw));
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[h.len()] as f64 / r.len().max(1) as f64
}
