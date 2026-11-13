use anyhow::{Context, bail};

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub livekit_public_url: String,
    pub livekit_api_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub database_url: String,
    pub public_url: String,
    pub resend_api_key: Option<String>,
    pub mail_from: String,
    pub bootstrap_email: Option<String>,
    /// Explicit local-only email sink. Never enabled by a release deployment.
    pub mail_dir: Option<std::path::PathBuf>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let cfg = Self {
            bind: env("QUIPCALL_BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            livekit_public_url: env("LIVEKIT_PUBLIC_URL")
                .unwrap_or_else(|| "ws://127.0.0.1:7880".into()),
            livekit_api_url: env("LIVEKIT_API_URL")
                .unwrap_or_else(|| "http://127.0.0.1:7880".into()),
            livekit_api_key: env("LIVEKIT_API_KEY").context("LIVEKIT_API_KEY is not set")?,
            livekit_api_secret: env("LIVEKIT_API_SECRET")
                .context("LIVEKIT_API_SECRET is not set")?,
            database_url: env("QUIPCALL_DATABASE_URL")
                .unwrap_or_else(|| "sqlite://dev/data/quipcall.db".into()),
            public_url: env("QUIPCALL_PUBLIC_URL")
                .unwrap_or_else(|| "http://127.0.0.1:5173".into())
                .trim_end_matches('/')
                .into(),
            resend_api_key: env("RESEND_API_KEY"),
            mail_from: env("QUIPCALL_MAIL_FROM")
                .unwrap_or_else(|| "Quipcall <login@quipcall.com>".into()),
            bootstrap_email: env("QUIPCALL_BOOTSTRAP_EMAIL").map(|s| s.trim().to_lowercase()),
            mail_dir: env("QUIPCALL_MAIL_DIR").map(Into::into),
        };
        if cfg.livekit_api_secret.len() < 32 {
            bail!("LIVEKIT_API_SECRET must be at least 32 characters");
        }
        let url = url::Url::parse(&cfg.public_url)?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("QUIPCALL_PUBLIC_URL must use HTTPS (except localhost)");
        }
        if url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("QUIPCALL_PUBLIC_URL must be an origin without a path or credentials");
        }
        if cfg.mail_dir.is_some() && !local {
            bail!("QUIPCALL_MAIL_DIR is only allowed on localhost");
        }
        if cfg.mail_dir.is_none() && cfg.resend_api_key.is_none() {
            bail!("set RESEND_API_KEY, or QUIPCALL_MAIL_DIR for local development");
        }
        Ok(cfg)
    }
    pub fn secure_cookie(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}
fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
