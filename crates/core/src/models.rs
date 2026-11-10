//! Speech model files: whether they're here, and fetching them.
//!
//! A download goes to `<file>.part`, resumes from there after a failure or
//! a restart, and is renamed into place only once its SHA-256 matches the
//! catalog's. A dropped connection is retried twice, from where it stopped,
//! before the download counts as failed. A model in use is memory-mapped, so files are never rewritten
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
            let result = match claim(info.file, &stop) {
                Some(_claim) => tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("runtime")
                    .and_then(|rt| rt.block_on(fetch_retrying(info, &dir, &stop, &tx))),
                None => Err(anyhow::anyhow!("cancelled")),
            };
            let last = match result {
                Ok(path) => Progress::Done(path),
                Err(_) if stop.load(Ordering::Relaxed) => Progress::Cancelled,
                Err(e) => {
                    tracing::warn!("downloading {}: {e:#}", info.file);
                    Progress::Failed(if transient(&e) {
                        // The details are in the log.
                        "the network dropped. What came is kept; try again to go on.".to_owned()
                    } else {
                        format!("{e:#}")
                    })
                }
            };
            let _ = tx.try_send(last);
        })
        .expect("download thread");
    Download { progress, cancel }
}

/// Files being fetched. One writer per `.part`: a second download of the
/// same file (the VAD, which every model needs, or a restart right after a
/// cancel, while the old thread winds down) waits for the first, then finds
/// the file here or resumes it.
static BUSY: parking_lot::Mutex<Vec<&'static str>> = parking_lot::Mutex::new(Vec::new());
static FREED: parking_lot::Condvar = parking_lot::Condvar::new();

struct Claim(&'static str);

impl Drop for Claim {
    fn drop(&mut self) {
        BUSY.lock().retain(|f| *f != self.0);
        FREED.notify_all();
    }
}

/// Wait until nobody else fetches `file`; `None` if cancelled meanwhile.
fn claim(file: &'static str, stop: &AtomicBool) -> Option<Claim> {
    let mut busy = BUSY.lock();
    while busy.contains(&file) {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        FREED.wait_for(&mut busy, Duration::from_millis(100));
    }
    busy.push(file);
    Some(Claim(file))
}

/// Attempts at a download that keeps dropping.
const ATTEMPTS: u32 = 3;

async fn fetch_retrying(
    info: &ModelInfo,
    dir: &Path,
    stop: &AtomicBool,
    tx: &async_channel::Sender<Progress>,
) -> anyhow::Result<PathBuf> {
    let mut attempt = 1;
    loop {
        match fetch(info, dir, stop, tx).await {
            Err(e) if attempt < ATTEMPTS && !stop.load(Ordering::Relaxed) && transient(&e) => {
                tracing::warn!("downloading {} (attempt {attempt}): {e:#}", info.file);
                tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The network failed us, or the server did: worth another go. A damaged
/// file or a missing one isn't.
fn transient(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        e.is_connect()
            || e.is_timeout()
            || e.is_body()
            || e.is_request()
            || e.status().is_some_and(|s| s.is_server_error())
    })
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
    // All of it came before (the app quit before checking it): asking for
    // the bytes after the end would only get a 416.
    if have == info.bytes {
        return install(hash, info, &part, &done);
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let mut request = client.get(info.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    // Errors without the URL: it's long, and Settings shows the error.
    let mut response = request
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(reqwest::Error::without_url)?;
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
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
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
    let _ = tx.try_send(Progress::Bytes(have));
    install(hash, info, &part, &done)
}

/// A whole `.part`, renamed into place if its checksum is the catalog's.
fn install(hash: Sha256, info: &ModelInfo, part: &Path, done: &Path) -> anyhow::Result<PathBuf> {
    let sha = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if sha != info.sha256 {
        let _ = std::fs::remove_file(part);
        anyhow::bail!("the file is damaged (checksum {sha}); try again");
    }
    std::fs::rename(part, done)?;
    Ok(done.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test file "hello", from a server that isn't there: a whole `.part`
    /// must never need it.
    static HELLO: ModelInfo = ModelInfo {
        id: "hello",
        name: "Hello",
        by: "",
        license: "",
        about: "",
        languages: &[],
        file: "hello.bin",
        url: "http://127.0.0.1:9/hello.bin",
        bytes: 5,
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    };

    fn last(d: Download) -> Progress {
        loop {
            match d.progress.recv_blocking().unwrap() {
                Progress::Bytes(_) => {}
                last => return last,
            }
        }
    }

    #[test]
    fn a_whole_part_is_installed_without_the_network() {
        let dir = std::env::temp_dir().join(format!("qc-models-whole-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(part(&HELLO, &dir), b"hello").unwrap();
        // Two at once (two models wanting the VAD): one writer, both done.
        let (a, b) = (download(&HELLO, dir.clone()), download(&HELLO, dir.clone()));
        assert_eq!(last(a), Progress::Done(path(&HELLO, &dir)));
        assert_eq!(last(b), Progress::Done(path(&HELLO, &dir)));
        assert!(installed(&HELLO, &dir));
        // A whole but damaged one is thrown away.
        std::fs::remove_file(path(&HELLO, &dir)).unwrap();
        std::fs::write(part(&HELLO, &dir), b"jello").unwrap();
        assert!(matches!(
            last(download(&HELLO, dir.clone())),
            Progress::Failed(_)
        ));
        assert!(!part(&HELLO, &dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
