//! The models we offer, downloaded on demand from Hugging Face. Files are
//! pinned to a commit and checked against their SHA-256.

/// A model file.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// Stable id, kept in settings.
    pub id: &'static str,
    pub name: &'static str,
    /// Who made it; their licences ask us to say so.
    pub by: &'static str,
    pub license: &'static str,
    /// One line for Settings.
    pub about: &'static str,
    /// ISO 639-1 codes it transcribes; it detects which one is spoken.
    pub languages: &'static [&'static str],
    /// File name on disk.
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// Silero VAD v6.2: where there's speech. Needed with every model.
pub const VAD: ModelInfo = ModelInfo {
    id: "silero-vad-6.2",
    name: "Silero VAD",
    by: "Silero",
    license: "MIT",
    about: "Finds speech in each voice.",
    languages: &[],
    file: "silero-vad-v6.2-F32.gguf",
    url: "https://huggingface.co/handy-computer/silero-vad-v6.2-gguf/resolve/a9b2d61c20ae4ddf3644edb34969e8185a485448/silero-vad-v6.2-F32.gguf",
    bytes: 1_241_056,
    sha256: "0d871a800c420fdd5d1b17a3aea4296d1c39608f0ff4a9295eb76457eb1c88e6",
};

/// Parakeet TDT 0.6B v3, 8-bit: 25 European languages, very fast on the GPU.
pub const PARAKEET_V3: ModelInfo = ModelInfo {
    id: "parakeet-tdt-0.6b-v3",
    name: "Parakeet v3",
    by: "NVIDIA",
    license: "CC-BY-4.0",
    about: "25 European languages, Russian and Ukrainian included. Fast.",
    languages: &[
        "bg", "cs", "da", "de", "el", "en", "es", "et", "fi", "fr", "hr", "hu", "it", "lt", "lv",
        "mt", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "uk",
    ],
    file: "parakeet-tdt-0.6b-v3-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-tdt-0.6b-v3-gguf/resolve/d8d8c32cb2017c57e5307767343e97870f8a7f53/parakeet-tdt-0.6b-v3-Q8_0.gguf",
    bytes: 739_508_576,
    sha256: "5859f77944efcd8eafa23a6350731960b2b55b2203df51f319665c807d802cc7",
};

/// Qwen3-ASR 0.6B, 8-bit: Chinese and its dialects, Japanese, Korean and
/// more. Slower than Parakeet.
pub const QWEN3_ASR: ModelInfo = ModelInfo {
    id: "qwen3-asr-0.6b",
    name: "Qwen3-ASR",
    by: "Alibaba Qwen",
    license: "Apache-2.0",
    about: "30 languages with Chinese, Japanese and Korean. Slower.",
    languages: &[
        "ar", "cs", "da", "de", "el", "en", "es", "fa", "fi", "fil", "fr", "hi", "hu", "id", "it",
        "ja", "ko", "mk", "ms", "nl", "pl", "pt", "ro", "ru", "sv", "th", "tr", "vi", "yue", "zh",
    ],
    file: "Qwen3-ASR-0.6B-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/Qwen3-ASR-0.6B-gguf/resolve/7e7c69a577fa2725dfedbe34b5e999336e30a19b/Qwen3-ASR-0.6B-Q8_0.gguf",
    bytes: 850_423_456,
    sha256: "f081b2d5e23bd669d92cc331d722a8a0681943b8e6f34b48996fd5c319b5acd8",
};

/// Speech models, the default first.
pub const MODELS: &[&ModelInfo] = &[&PARAKEET_V3, &QWEN3_ASR];

/// Languages to choose from in Settings, by their own names.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("pl", "Polski"),
    ("nl", "Nederlands"),
    ("zh", "中文"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

pub fn model(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().copied().find(|m| m.id == id)
}

/// The model we'd pick for a language: the first that speaks it.
pub fn for_language(language: Option<&str>) -> &'static ModelInfo {
    language
        .and_then(|l| MODELS.iter().copied().find(|m| m.languages.contains(&l)))
        .unwrap_or(MODELS[0])
}

pub fn language_name(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_a_model() {
        for (code, _) in LANGUAGES {
            let m = for_language(Some(code));
            assert!(m.languages.contains(code), "{code}");
        }
        assert_eq!(for_language(Some("zh")).id, QWEN3_ASR.id);
        assert_eq!(for_language(Some("ru")).id, PARAKEET_V3.id);
        assert_eq!(for_language(None).id, PARAKEET_V3.id);
    }

    #[test]
    fn files_are_pinned() {
        for m in MODELS.iter().copied().chain([&VAD]) {
            assert!(m.url.ends_with(m.file), "{}", m.id);
            assert!(
                m.url.contains("/resolve/") && !m.url.contains("/main/"),
                "{}",
                m.id
            );
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
        }
    }
}
