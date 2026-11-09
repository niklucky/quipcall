//! Plays a recording's sound: Home's player under a call's transcript.
//! AVPlayer, part of macOS; it plays an .mp4's audio as well as an .m4a.
//! Main thread only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_av_foundation::AVPlayer;
use objc2_core_media::CMTime;
use objc2_foundation::{NSString, NSURL};

pub struct Player {
    player: Retained<AVPlayer>,
    path: PathBuf,
}

impl Player {
    /// `None` off the main thread.
    pub fn open(path: &Path) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        // SAFETY: a file URL; AVPlayer loads it in the background.
        let player = unsafe { AVPlayer::playerWithURL(&url, mtm) };
        // Test instances have no speaker either.
        if std::env::var_os("QUIPCALL_NO_MIC").is_some() {
            unsafe { player.setMuted(true) };
        }
        Some(Self {
            player,
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn play(&self) {
        // SAFETY: plain AVPlayer calls on the main thread (see `open`).
        unsafe { self.player.play() }
    }

    pub fn pause(&self) {
        unsafe { self.player.pause() }
    }

    pub fn is_playing(&self) -> bool {
        (unsafe { self.player.rate() }) > 0.0
    }

    /// To exactly `at`: a line plays from its first word.
    pub fn seek(&self, at: Duration) {
        // SAFETY: plain value constructors and an AVPlayer call.
        unsafe {
            let time = CMTime::new(at.as_millis() as i64, 1000);
            let exact = CMTime::new(0, 1000);
            self.player
                .seekToTime_toleranceBefore_toleranceAfter(time, exact, exact)
        }
    }

    pub fn position(&self) -> Duration {
        seconds(unsafe { self.player.currentTime() }).unwrap_or_default()
    }

    /// Once the file has loaded.
    pub fn duration(&self) -> Option<Duration> {
        let item = unsafe { self.player.currentItem() }?;
        seconds(unsafe { item.duration() })
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.pause();
    }
}

/// A valid, finite time.
fn seconds(t: CMTime) -> Option<Duration> {
    let valid = t.flags.0 & 1 != 0 && t.flags.0 & (1 << 2 | 1 << 3 | 1 << 4) == 0;
    (valid && t.timescale > 0 && t.value >= 0)
        .then(|| Duration::from_secs_f64(t.value as f64 / t.timescale as f64))
}
