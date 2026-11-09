//! Speech model files: whether they're here, and fetching them.
//!
//! A download goes to `<file>.part`, resumes from there after a failure or
//! a restart, and is renamed into place only once its SHA-256 matches the
//! catalog's. A model in use is memory-mapped, so files are never rewritten
//! in place.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use sha2::{Digest as _, Sha256};

pub use quipcall_stt::{ModelInfo, catalog};

pub fn path(info: &ModelInfo, dir: &Path) -> PathBuf {
    dir.join(info.file)
}

/// The file is here, whole. (Its checksum was checked when it arrived.)
pub fn installed(info: &ModelInfo, dir: &Path) -> bool {
    std::fs::metadata(path(info, dir)).is_ok_and(|m| m.len() == info.bytes)
}

pub fn remove(info: &ModelInfo, dir: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(part(info, dir));
    match std::fs::remove_file(path(info, dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn part(info: &ModelInfo, dir: &Path) -> PathBuf {
    dir.join(format!("{}.part", info.file))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Bytes so far, of `info.bytes`.
    Bytes(u64),
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// A download in progress. Dropping it cancels; what came is kept for next
/// time.
pub struct Download {
    pub progress: async_channel::Receiver<Progress>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Fetches `info` into `dir` on a thread of its own. Progress comes about
/// four times a second; the last message is `Done`, `Failed` or `Cancelled`.
pub fn download(info: &'static ModelInfo, dir: PathBuf) -> Download {
    let (tx, progress) = async_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::Builder::new()
        .name("model-download".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("runtime")
                .and_then(|rt| rt.block_on(fetch(info, &dir, &stop, &tx)));
            let last = match result {
                Ok(path) => Progress::Done(path),
                Err(_) if stop.load(Ordering::Relaxed) => Progress::Cancelled,
                Err(e) => {
                    tracing::warn!("downloading {}: {e:#}", info.file);
                    Progress::Failed(format!("{e:#}"))
                }
            };
            let _ = tx.try_send(last);
        })
        .expect("download thread");
    Download { progress, cancel }
}

async fn fetch(
    info: &ModelInfo,
    dir: &Path,
    stop: &AtomicBool,
    tx: &async_channel::Sender<Progress>,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let done = path(info, dir);
    if installed(info, dir) {
        return Ok(done);
    }
    let part = part(info, dir);

    // Resume: hash what's already here.
    let mut hash = Sha256::new();
    let mut have = 0u64;
    if let Ok(mut file) = std::fs::File::open(&part) {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            have += n as u64;
        }
    }
    if have > info.bytes {
        hash = Sha256::new();
        have = 0;
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let mut request = client.get(info.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await?.error_for_status()?;
    let mut file = if have > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        std::fs::OpenOptions::new().append(true).open(&part)?
    } else {
        // The server sent it all again.
        hash = Sha256::new();
        have = 0;
        std::fs::File::create(&part)?
    };
    let _ = tx.try_send(Progress::Bytes(have));

    let mut told = Instant::now();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(!stop.load(Ordering::Relaxed), "cancelled");
        file.write_all(&chunk)?;
        hash.update(&chunk);
        have += chunk.len() as u64;
        anyhow::ensure!(have <= info.bytes, "the file is bigger than it should be");
        if told.elapsed() >= Duration::from_millis(250) {
            told = Instant::now();
            let _ = tx.try_send(Progress::Bytes(have));
        }
    }
    file.flush()?;
    drop(file);
    anyhow::ensure!(
        have == info.bytes,
        "the download stopped at {have} of {} bytes",
        info.bytes
    );
    let sha = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if sha != info.sha256 {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("the file is damaged (checksum {sha}); try again");
    }
    std::fs::rename(&part, &done)?;
    let _ = tx.try_send(Progress::Bytes(have));
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real VAD (1.2 MB), resumed from half of it. Needs the network.
    #[test]
    #[ignore = "downloads from Hugging Face"]
    fn resumes_and_checks() {
        let dir = std::env::temp_dir().join(format!("qc-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let info = &catalog::VAD;
        let wait = |d: Download| loop {
            match d.progress.recv_blocking().unwrap() {
                Progress::Bytes(_) => {}
                last => return last,
            }
        };
        let first = wait(download(info, dir.clone()));
        assert_eq!(first, Progress::Done(path(info, &dir)));
        // Cut it in half and make it a partial download.
        let bytes = std::fs::read(path(info, &dir)).unwrap();
        std::fs::remove_file(path(info, &dir)).unwrap();
        std::fs::write(part(info, &dir), &bytes[..bytes.len() / 2]).unwrap();
        assert_eq!(
            wait(download(info, dir.clone())),
            Progress::Done(path(info, &dir))
        );
        assert_eq!(std::fs::read(path(info, &dir)).unwrap(), bytes);
        // Garbage in the partial file fails the checksum.
        std::fs::remove_file(path(info, &dir)).unwrap();
        std::fs::write(part(info, &dir), vec![7u8; 1000]).unwrap();
        assert!(matches!(
            wait(download(info, dir.clone())),
            Progress::Failed(_)
        ));
        assert!(!part(info, &dir).exists(), "a damaged file is thrown away");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
