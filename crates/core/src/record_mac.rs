//! The recording's file on macOS, written by AVAssetWriter: AAC in an .m4a,
//! or with your screen, H.264 and AAC in an .mp4. The encoders are part of the
//! OS (VideoToolbox for the screen), so there's nothing to ship. The file gets
//! a fragment every few seconds, so a crash or a full disk keeps everything up
//! to the last one.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::time::Duration;

use anyhow::Context as _;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVAssetWriterStatus,
    AVFileTypeAppleM4A, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo,
    AVVideoAverageBitRateKey, AVVideoCodecKey, AVVideoCodecTypeH264,
    AVVideoCompressionPropertiesKey, AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey,
    AVVideoMaxKeyFrameIntervalDurationKey, AVVideoProfileLevelH264HighAutoLevel,
    AVVideoProfileLevelKey, AVVideoWidthKey,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMAudioFormatDescriptionCreate, CMAudioSampleBufferCreateReadyWithPacketDescriptions,
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use crate::audio_tap::SAMPLE_RATE;

/// Plenty for speech in mono: about 29 MB an hour.
const BITRATE: u32 = 64_000;
/// Seconds between fragments: what a crash can lose.
const FRAGMENT: i64 = 5;
/// How long a chunk may wait for the encoder before it's dropped.
const BACKPRESSURE: Duration = Duration::from_millis(500);

/// The screen's frames in a recording.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Average bits per second.
    pub bitrate: u32,
}

pub(crate) struct MediaWriter {
    path: PathBuf,
    writer: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    /// The audio handed over: mono float32 at [`SAMPLE_RATE`].
    format: CFRetained<CMFormatDescription>,
    /// Samples written so far; the time of the next one.
    written: i64,
    video: Option<Video>,
}

struct Video {
    input: Retained<AVAssetWriterInput>,
    adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    /// The last frame's time, in microseconds: frames must keep moving on.
    last: Option<i64>,
    skipped: u64,
}

impl MediaWriter {
    /// An .m4a of audio, or with `video`, an .mp4 of audio and the screen.
    pub fn create(path: &Path, video: Option<VideoFormat>) -> anyhow::Result<Self> {
        crate::autorelease_pool(|| {
            // SAFETY: AVFoundation and CoreMedia calls with valid arguments;
            // out-pointers are checked before use.
            unsafe { Self::open(path, video) }
        })
    }

    unsafe fn open(path: &Path, video: Option<VideoFormat>) -> anyhow::Result<Self> {
        anyhow::ensure!(!path.exists(), "{} already exists", path.display());
        let url = NSURL::fileURLWithPath(&NSString::from_str(
            path.to_str().context("the path isn't UTF-8")?,
        ));
        let file_type = if video.is_some() {
            unsafe { AVFileTypeMPEG4 }.context("no MPEG-4 file type")?
        } else {
            unsafe { AVFileTypeAppleM4A }.context("no M4A file type")?
        };
        let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type) }
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        unsafe { writer.setMovieFragmentInterval(CMTime::new(FRAGMENT, 1)) };

        // AVAudioSettings' keys (AVFAudio), spelled out.
        let keys = [
            NSString::from_str("AVFormatIDKey"),
            NSString::from_str("AVSampleRateKey"),
            NSString::from_str("AVNumberOfChannelsKey"),
            NSString::from_str("AVEncoderBitRateKey"),
        ];
        let values = [
            NSNumber::new_u32(kAudioFormatMPEG4AAC),
            NSNumber::new_f64(SAMPLE_RATE as f64),
            NSNumber::new_u32(1),
            NSNumber::new_u32(BITRATE),
        ];
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&AnyObject> = values.iter().map(|v| v.as_ref()).collect();
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        let media = unsafe { AVMediaTypeAudio }.context("no audio media type")?;
        let input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(media, Some(&settings))
        };
        unsafe { input.setExpectsMediaDataInRealTime(true) };
        anyhow::ensure!(
            unsafe { writer.canAddInput(&input) },
            "the recording can't take audio"
        );
        unsafe { writer.addInput(&input) };
        let video = match video {
            Some(format) => Some(unsafe { add_video(&writer, format) }?),
            None => None,
        };
        anyhow::ensure!(unsafe { writer.startWriting() }, failure(&writer));
        unsafe { writer.startSessionAtSourceTime(CMTime::new(0, SAMPLE_RATE as i32)) };

        let asbd = AudioStreamBasicDescription {
            mSampleRate: SAMPLE_RATE as f64,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4,
            mChannelsPerFrame: 1,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let mut format: *const CMFormatDescription = ptr::null();
        let status = unsafe {
            CMAudioFormatDescriptionCreate(
                None,
                NonNull::from(&asbd),
                0,
                ptr::null(),
                0,
                ptr::null(),
                None,
                NonNull::from(&mut format),
            )
        };
        let format = NonNull::new(format.cast_mut())
            .filter(|_| status == 0)
            .with_context(|| format!("audio format: {status}"))?;
        Ok(Self {
            path: path.to_owned(),
            writer,
            input,
            // SAFETY: a +1 reference from a Create function.
            format: unsafe { CFRetained::from_raw(format) },
            written: 0,
            video,
        })
    }

    /// One frame of the screen, `at` into the recording. A frame the encoder
    /// can't take right now is skipped: the previous one stays on screen.
    pub fn write_frame(
        &mut self,
        pixel_buffer: &CVPixelBuffer,
        at: Duration,
    ) -> anyhow::Result<()> {
        let Some(video) = &mut self.video else {
            return Ok(());
        };
        let micros = at.as_micros() as i64;
        if video.last.is_some_and(|last| micros <= last) {
            return Ok(());
        }
        crate::autorelease_pool(|| {
            // SAFETY: a valid pixel buffer, appended to our own input.
            unsafe {
                if !video.input.isReadyForMoreMediaData() {
                    video.skipped += 1;
                    return Ok(());
                }
                let time = CMTime::new(micros, 1_000_000);
                anyhow::ensure!(
                    video
                        .adaptor
                        .appendPixelBuffer_withPresentationTime(pixel_buffer, time),
                    failure(&self.writer)
                );
            }
            video.last = Some(micros);
            Ok(())
        })
    }

    /// Append `samples` (mono, -1..1).
    pub fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        crate::autorelease_pool(|| {
            // SAFETY: as in `create`; `samples` outlives the copy into the
            // block buffer.
            unsafe { self.append(samples) }
        })
    }

    unsafe fn append(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        let bytes = std::mem::size_of_val(samples);
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                bytes,
                None,
                ptr::null(),
                0,
                bytes,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            )
        };
        let block = NonNull::new(block)
            .filter(|_| status == 0)
            .with_context(|| format!("audio buffer: {status}"))?;
        // SAFETY: a +1 reference from a Create function.
        let block = unsafe { CFRetained::from_raw(block) };
        let source =
            NonNull::new(samples.as_ptr().cast_mut().cast::<c_void>()).context("no audio")?;
        let status = unsafe { CMBlockBuffer::replace_data_bytes(source, &block, 0, bytes) };
        anyhow::ensure!(status == 0, "copying audio: {status}");

        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        let status = unsafe {
            CMAudioSampleBufferCreateReadyWithPacketDescriptions(
                None,
                &block,
                &self.format,
                samples.len() as isize,
                CMTime::new(self.written, SAMPLE_RATE as i32),
                ptr::null(),
                NonNull::from(&mut sample),
            )
        };
        let sample = NonNull::new(sample)
            .filter(|_| status == 0)
            .with_context(|| format!("audio sample: {status}"))?;
        // SAFETY: a +1 reference from a Create function.
        let sample = unsafe { CFRetained::from_raw(sample) };

        // A real-time input is nearly always ready; give it a moment if not.
        let waited = std::time::Instant::now();
        while !unsafe { self.input.isReadyForMoreMediaData() } {
            if waited.elapsed() > BACKPRESSURE {
                tracing::warn!("recording: the encoder fell behind, skipping a chunk");
                self.written += samples.len() as i64;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        anyhow::ensure!(
            unsafe { self.input.appendSampleBuffer(&sample) },
            failure(&self.writer)
        );
        self.written += samples.len() as i64;
        Ok(())
    }

    /// Close the file. Returns its size in bytes.
    pub fn finish(self) -> anyhow::Result<u64> {
        if let Some(video) = &self.video
            && video.skipped > 0
        {
            tracing::info!(
                skipped = video.skipped,
                "recording: screen frames the encoder couldn't take"
            );
        }
        crate::autorelease_pool(|| {
            // SAFETY: as in `create`.
            unsafe { self.close() }
        })
    }

    unsafe fn close(self) -> anyhow::Result<u64> {
        if self.written == 0 {
            unsafe { self.writer.cancelWriting() };
            let _ = std::fs::remove_file(&self.path);
            anyhow::bail!("nothing was recorded");
        }
        // The session ends with whichever lasts longer, sound or picture.
        let audio_end = self.written * 1_000_000 / SAMPLE_RATE as i64;
        let video_end = self.video.as_ref().and_then(|v| v.last).unwrap_or(0);
        unsafe {
            self.input.markAsFinished();
            if let Some(video) = &self.video {
                video.input.markAsFinished();
            }
            self.writer
                .endSessionAtSourceTime(CMTime::new(audio_end.max(video_end), 1_000_000));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let done = RcBlock::new(move || {
            let _ = tx.send(());
        });
        unsafe { self.writer.finishWritingWithCompletionHandler(&done) };
        rx.recv_timeout(Duration::from_secs(10))
            .context("finishing the file took too long")?;
        anyhow::ensure!(
            unsafe { self.writer.status() } == AVAssetWriterStatus::Completed,
            failure(&self.writer)
        );
        Ok(std::fs::metadata(&self.path)?.len())
    }
}

/// An H.264 input for the screen (VideoToolbox does the encoding).
unsafe fn add_video(writer: &AVAssetWriter, format: VideoFormat) -> anyhow::Result<Video> {
    let key = |k: Option<&'static NSString>| k.context("missing AVFoundation key");
    let (bitrate, fps, keyframes) = (
        NSNumber::new_u32(format.bitrate),
        NSNumber::new_u32(format.fps.max(1)),
        // A keyframe at least every 2 s, so every fragment can start playing.
        NSNumber::new_f64(2.0),
    );
    let compression = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            key(unsafe { AVVideoAverageBitRateKey })?,
            key(unsafe { AVVideoExpectedSourceFrameRateKey })?,
            key(unsafe { AVVideoMaxKeyFrameIntervalDurationKey })?,
            key(unsafe { AVVideoProfileLevelKey })?,
        ],
        &[
            bitrate.as_ref(),
            fps.as_ref(),
            keyframes.as_ref(),
            key(unsafe { AVVideoProfileLevelH264HighAutoLevel })?.as_ref(),
        ],
    );
    let (width, height) = (
        NSNumber::new_u32(format.width),
        NSNumber::new_u32(format.height),
    );
    let settings = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            key(unsafe { AVVideoCodecKey })?,
            key(unsafe { AVVideoWidthKey })?,
            key(unsafe { AVVideoHeightKey })?,
            key(unsafe { AVVideoCompressionPropertiesKey })?,
        ],
        &[
            key(unsafe { AVVideoCodecTypeH264 })?.as_ref(),
            width.as_ref(),
            height.as_ref(),
            compression.as_ref(),
        ],
    );
    let media = unsafe { AVMediaTypeVideo }.context("no video media type")?;
    let input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(media, Some(&settings))
    };
    unsafe { input.setExpectsMediaDataInRealTime(true) };
    anyhow::ensure!(
        unsafe { writer.canAddInput(&input) },
        "the recording can't take the screen at {}x{}",
        format.width,
        format.height
    );
    unsafe { writer.addInput(&input) };
    let adaptor = unsafe {
        AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None)
    };
    Ok(Video {
        input,
        adaptor,
        last: None,
        skipped: 0,
    })
}

/// What AVAssetWriter says went wrong.
fn failure(writer: &AVAssetWriter) -> String {
    match unsafe { writer.error() } {
        Some(e) => format!("recording: {}", e.localizedDescription()),
        None => "recording failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.3)
            .collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("quipcall-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    /// The length afinfo (part of macOS) reads from the file.
    fn seconds(path: &Path) -> Option<f32> {
        let info = Command::new("afinfo").arg(path).output().ok()?;
        String::from_utf8_lossy(&info.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("estimated duration: "))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    #[test]
    fn writes_a_playable_file() {
        let path = scratch("tone.m4a");
        let mut writer = MediaWriter::create(&path, None).unwrap();
        for chunk in tone(1.0).chunks(4800) {
            writer.write(chunk).unwrap();
        }
        let bytes = writer.finish().unwrap();
        assert!(bytes > 2_000, "{bytes} bytes");
        let secs = seconds(&path).expect("afinfo reads it");
        assert!((secs - 1.0).abs() < 0.1, "{secs} s");
    }

    /// Two seconds of a test pattern at 15 fps with a tone: an .mp4 with both.
    #[test]
    fn writes_the_screen_too() {
        use crate::pixel_buffer::{PixelBufferPool, paint_test_pattern};

        let path = scratch("screen.mp4");
        let format = VideoFormat {
            width: 640,
            height: 400,
            fps: 15,
            bitrate: 1_000_000,
        };
        let mut writer = MediaWriter::create(&path, Some(format)).unwrap();
        let pool = PixelBufferPool::new(640, 400).unwrap();
        let audio = tone(2.0);
        for n in 0..30usize {
            let frame = pool.take().unwrap();
            paint_test_pattern(&frame, n, [160, 100]);
            writer
                .write_frame(&frame, Duration::from_millis(n as u64 * 1000 / 15))
                .unwrap();
            let chunk = SAMPLE_RATE as usize / 15;
            let from = n * chunk;
            writer.write(&audio[from..from + chunk]).unwrap();
        }
        writer.finish().unwrap();
        // ffprobe reads it if it's there (Homebrew); afinfo checks the sound.
        if let Ok(out) = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames"])
            .args([
                "-show_entries",
                "stream=codec_name,width,height,nb_read_frames",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.starts_with("h264,640,400"), "ffprobe: {text}");
        }
        let secs = seconds(&path).expect("afinfo reads the sound");
        assert!((secs - 2.0).abs() < 0.15, "{secs} s");
    }

    /// The other half of `a_crash_keeps_what_came_before`: writes 12 s, then
    /// dies without closing the file. Does nothing in a normal test run.
    #[test]
    fn crash_while_writing() {
        let Some(path) = std::env::var_os("QUIPCALL_CRASH_FILE") else {
            return;
        };
        let mut writer = MediaWriter::create(Path::new(&path), None).unwrap();
        for chunk in tone(12.0).chunks(4800) {
            writer.write(chunk).unwrap();
        }
        // AVAssetWriter writes on a queue of its own; give it a moment.
        std::thread::sleep(Duration::from_secs(1));
        std::process::abort();
    }

    #[test]
    fn a_crash_keeps_what_came_before() {
        let path = scratch("crash.m4a");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "record_mac::tests::crash_while_writing",
                "--nocapture",
            ])
            .env("QUIPCALL_CRASH_FILE", &path)
            .status()
            .unwrap();
        assert!(!status.success(), "the writer was meant to crash");
        let secs = seconds(&path).expect("the crashed file still opens");
        // Fragments every 5 s: at least the first 10 s survive.
        assert!(secs >= 9.5, "{secs} s kept");
    }
}
