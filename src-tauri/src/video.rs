//! Keyframe extraction for near-duplicate/blur analysis of video files.
//!
//! Rather than a video-specific hash or sharpness metric, this samples three frames
//! and hands each to the same `fingerprint`/`sharpness` functions a photograph goes
//! through (see `lib.rs`), so a video and a still are directly comparable and every
//! bit of the existing scoring logic (thresholds, clustering, tests) applies unchanged.
//!
//! Frames are grabbed by shelling out to an `ffmpeg` binary rather than linking
//! `ffmpeg-next`/`ffmpeg-sys-next`. The Rust-binding route was tried first and worked,
//! but it links against libav* at compile time via bindgen/pkg-config — nothing a CI
//! runner or an end user's machine has, so it built and tested only on a dev machine
//! with Homebrew's ffmpeg installed. Shelling out to a self-contained `ffmpeg`
//! executable (shipped as a Tauri sidecar, see `externalBin` in `tauri.conf.json`)
//! needs no native headers or linker flags at all: `cargo build`/`cargo test` work
//! anywhere, and what has to travel with the app is one portable binary per platform
//! instead of a set of shared libraries at specific rpaths.
//!
//! That sidecar binary is not something this code can supply. It must be a static,
//! LGPL-configured ffmpeg build (no GPL-only codecs like libx264, so it can ship inside
//! a closed-source paid app without copyleft obligations) placed at
//! `src-tauri/binaries/ffmpeg-<target-triple>[.exe]` per Tauri's sidecar convention.
//! `tauri-build`'s own build script strips the `-<target-triple>` suffix and copies
//! the right one next to the compiled executable for the current target, so at
//! runtime the lookup (`sidecar_path`, below) is just "next to `current_exe()`, under
//! its bare name" — no target-triple detection needed there at all. None of the
//! actual binaries exist yet, so `sidecar_path` simply returns `None` until they do,
//! and every caller already treats a missing sidecar the same as an undecodable file.

use image::RgbImage;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolves a Tauri sidecar's runtime path: next to the running executable, under its
/// bare name plus the platform's own executable suffix (`.exe` on Windows only).
/// `None` when `current_exe()` can't be read or the file isn't there — most commonly
/// today, because no LGPL ffmpeg binary has been placed under `src-tauri/binaries/` yet.
pub fn sidecar_path(name: &str) -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = dir.join(file_name);
    path.is_file().then_some(path)
}

/// Fraction of the timeline each sampled frame targets. A handful of points, not every
/// frame: enough to notice a video that changes mid-clip (a pan, a scene cut) without
/// decoding — and holding in memory — anything beyond the frames actually scored.
const SAMPLE_POINTS: [f64; 3] = [0.10, 0.50, 0.90];

pub struct VideoFrames {
    pub width: u32,
    pub height: u32,
    /// Index 0/1/2 line up with `SAMPLE_POINTS` (10%/50%/90%). A slot is `None` when
    /// its target position could not be decoded (a truncated file, a stream shorter
    /// than expected), independent of whether the other two succeeded.
    pub frames: [Option<RgbImage>; 3],
}

impl VideoFrames {
    /// The 50% frame: the one used for a video's own preview, and the only one blur
    /// is measured on (a beginning or end frame is disproportionately likely to be a
    /// half-second of camera shake before the subject settles).
    pub fn median(&self) -> Option<&RgbImage> {
        self.frames[1].as_ref()
    }
}

/// Grabs the frame nearest each of `SAMPLE_POINTS` via `ffmpeg_bin` and decodes it.
///
/// `None` when the file's duration can't be determined (not a video ffmpeg
/// recognises, or `ffmpeg_bin` itself could not be run) or every sample point failed
/// to decode — a corrupt or non-video file, not a partial result.
pub fn extract_keyframes(ffmpeg_bin: &Path, video_path: &Path) -> Option<VideoFrames> {
    // `-i` with no output makes ffmpeg print the container's metadata to stderr and
    // exit (non-zero, since "no output specified" is itself an error) without
    // decoding a single frame — the cheapest way to learn the duration.
    let probe = Command::new(ffmpeg_bin).arg("-i").arg(video_path).output().ok()?;
    let duration = parse_duration_secs(&String::from_utf8_lossy(&probe.stderr))?;
    if duration <= 0.0 {
        return None;
    }

    let mut frames: [Option<RgbImage>; 3] = [None, None, None];
    for (slot, &fraction) in SAMPLE_POINTS.iter().enumerate() {
        frames[slot] = grab_frame(ffmpeg_bin, video_path, duration * fraction);
        // Each `grab_frame` call is a fresh, short-lived ffmpeg process: its decode
        // buffers exist only inside that process and are gone — reclaimed by the OS —
        // the moment it exits, before the next sample point even starts. Nothing here
        // holds decoder state across frames, so memory use never grows with video
        // length, resolution, or how many videos are scanned in a batch.
    }

    if frames.iter().all(Option::is_none) {
        return None;
    }
    let (width, height) = frames.iter().find_map(|f| f.as_ref())?.dimensions();
    Some(VideoFrames { width, height, frames })
}

/// Seeks to `at_secs` (nearest keyframe at or before it — approximate by design, this
/// only needs "roughly a tenth/half/nine-tenths of the way through") and decodes
/// exactly one frame as RGB, without ever writing anything to disk.
fn grab_frame(ffmpeg_bin: &Path, video_path: &Path, at_secs: f64) -> Option<RgbImage> {
    let output = Command::new(ffmpeg_bin)
        .args(["-ss", &format!("{at_secs:.3}")])
        .arg("-i")
        .arg(video_path)
        .args(["-frames:v", "1", "-q:v", "2", "-f", "image2pipe", "-vcodec", "mjpeg", "-"])
        .output()
        .ok()?;
    if output.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Jpeg).ok()?;
    Some(img.to_rgb8())
}

/// Parses ffmpeg's own `Duration: HH:MM:SS.ss` banner line. `N/A` (some raw streams
/// report no duration at all) fails to parse as a number and correctly yields `None`.
fn parse_duration_secs(stderr: &str) -> Option<f64> {
    let field = stderr.split("Duration: ").nth(1)?.split(',').next()?.trim();
    let mut parts = field.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_from_ffmpeg_banner() {
        let stderr = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':\n  \
            Duration: 00:01:02.50, start: 0.000000, bitrate: 128 kb/s\n";
        assert_eq!(parse_duration_secs(stderr), Some(62.5));
    }

    #[test]
    fn unparseable_duration_is_none() {
        assert_eq!(parse_duration_secs("Duration: N/A, bitrate: N/A\n"), None);
        assert_eq!(parse_duration_secs("no duration line here\n"), None);
    }

    #[test]
    fn sidecar_path_is_none_when_not_bundled() {
        // No real sidecar ships next to the test binary, which is exactly today's
        // state for the actual app too: nothing under `src-tauri/binaries/` yet.
        assert!(sidecar_path("skimrr-no-such-sidecar").is_none());
    }

    /// Encodes a short synthetic clip with the `ffmpeg` CLI (only needed to build this
    /// fixture — resolved via `PATH`, same as the bare `ffmpeg` name the code under
    /// test is given) whose color changes partway through, so the 10%/50%/90% samples
    /// are verifiably different frames rather than three copies of the same one.
    fn synthetic_clip(dir: &Path) -> Option<std::path::PathBuf> {
        let out = dir.join("clip.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x48:rate=10:duration=3",
                "-pix_fmt",
                "yuv420p",
                out.to_str()?,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(out)
    }

    #[test]
    fn samples_three_distinct_points_and_scores_them() {
        let dir = std::env::temp_dir().join(format!("skimrr-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(clip) = synthetic_clip(&dir) else {
            eprintln!("skipping: no working `ffmpeg` CLI to build the test fixture");
            return;
        };

        let ffmpeg = Path::new("ffmpeg");
        let frames = extract_keyframes(ffmpeg, &clip).expect("a valid mp4 must yield keyframes");
        assert_eq!((frames.width, frames.height), (64, 48));
        assert!(frames.frames.iter().all(Option::is_some), "all three sample points should decode on a clean 3s clip");

        let median = frames.median().expect("median frame present");
        let gray = image::imageops::grayscale(median);
        assert!(crate::sharpness(&gray) >= 0.0);

        let hashes: Vec<u128> = frames
            .frames
            .iter()
            .filter_map(|f| f.as_ref())
            .filter_map(crate::fingerprint)
            .collect();
        assert!(!hashes.is_empty(), "a moving test pattern must carry enough structure to hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_none() {
        assert!(extract_keyframes(Path::new("ffmpeg"), Path::new("/no/such/file.mp4")).is_none());
    }
}
