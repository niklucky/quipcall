use anyhow::{Context, bail};

pub struct Config {
    pub bind: String,
    /// LiveKit URL handed to clients (must be reachable from their network).
    pub livekit_public_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let cfg = Self {
            bind: env("QUIPCALL_BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            livekit_public_url: env("LIVEKIT_PUBLIC_URL")
                .unwrap_or_else(|| "ws://127.0.0.1:7880".into()),
            livekit_api_key: env("LIVEKIT_API_KEY").context("LIVEKIT_API_KEY is not set")?,
            livekit_api_secret: env("LIVEKIT_API_SECRET")
                .context("LIVEKIT_API_SECRET is not set")?,
        };
        if cfg.livekit_api_secret.len() < 32 {
            bail!("LIVEKIT_API_SECRET must be at least 32 characters");
        }
        Ok(cfg)
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
