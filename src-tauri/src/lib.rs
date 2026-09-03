use chrono::NaiveDateTime;
use image::imageops::FilterType;
use image::GrayImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Emitter, Manager, State};
use walkdir::WalkDir;

mod bktree;
mod fusion;
mod license;
mod portable;
mod video;
use license::LicenceState;

const IMAGE_EXTS: [&str; 23] = [
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif",
    // Camera raw: read through the full-size JPEG rendition they embed.
    "dng", "arw", "sr2", "srf", "cr2", "cr3", "nef", "nrw", "orf", "rw2", "raf", "pef",
];

const RAW_EXTS: [&str; 12] = [
    "dng", "arw", "sr2", "srf", "cr2", "cr3", "nef", "nrw", "orf", "rw2", "raf", "pef",
];

/// Formats a phone or camera actually produces, not every container ffmpeg can open:
/// this is `analyze_video`'s scope, not ffmpeg's.
const VIDEO_EXTS: [&str; 7] = ["mp4", "mov", "m4v", "avi", "mkv", "webm", "3gp"];

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Serialize, Deserialize, Clone)]
struct Photo {
    path: String,
    name: String,
    size: u64,
    width: u32,
    height: u32,
    /// EXIF DateTimeOriginal when present, else file mtime (unix seconds)
    taken: i64,
    /// Laplacian variance sharpness score; None when the file could not be decoded
    blur: Option<f64>,
    /// What the webview should display: the file itself, or a cached rendition for
    /// formats no browser engine can show (camera raw, HEIC on Windows).
    preview: String,
    /// A much smaller rendition for grid cells and cover mosaics, where dozens of
    /// these can be on screen at once — decoding `preview` (or an un-downscaled
    /// original) for every one of them is wasted work a scrolling grid repeats
    /// constantly. `None` for a file this was never generated for (an older cache
    /// entry, or the encode failed); callers fall back to `preview`.
    #[serde(default)]
    thumb: Option<String>,
    /// What Bad Shot measured, and what it concluded. The measurements are cached with
    /// the file; the verdict is rebuilt whenever the folder's own thresholds move, so
    /// the two are kept apart rather than folded into one field.
    #[serde(default)]
    measurements: badshot::Measurements,
    #[serde(default)]
    bad_shot: badshot::BadShot,
    /// True for an asset read out of the Photos library rather than walked on disk.
    ///
    /// Two consequences, both load-bearing: it is never a candidate for the trash —
    /// Photos exposes no way to delete a `media item`, so offering it would be offering
    /// something that cannot happen — and it carries no sharpness score, because the
    /// only copy available locally is a resampled derivative whose detail density is
    /// not comparable with a full-size original's.
    #[serde(default)]
    library: bool,
    /// True for a photograph an imported project describes but which is not on this
    /// machine. Its findings are worth showing — that is most of what a project is — but
    /// there is no file to open, share, or move to the trash.
    #[serde(default)]
    missing: bool,
    /// Decimal degrees from the file's own EXIF GPS tags, absent on anything that was
    /// not geotagged — which is most of a scanned folder unless it came off a phone.
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    /// Pixel size of what `preview` actually points at, when that is materially smaller
    /// than the photograph itself.
    ///
    /// Only ever set for a library asset whose original is not on this Mac: `width` and
    /// `height` then describe a frame nobody can see here, and the card would otherwise
    /// claim 4624x3468 while showing a 480x360 stand-in — which reads as the photograph
    /// being poor rather than the preview being small.
    #[serde(default)]
    preview_dims: Option<[u32; 2]>,
    /// Uppercase extension of the original file (`HEIC`, `JPG`, `ARW`…). Kept apart
    /// from `name`, which for a library asset is the camera model rather than a
    /// filename — deriving a format by cutting at the last dot would read "iPhone 13"
    /// as a picture format.
    #[serde(default)]
    format: String,
    /// What took the photograph, from EXIF. `None` for anything that records no model:
    /// a screenshot, an export that dropped its metadata, a scan.
    #[serde(default)]
    device: Option<String>,
    /// Uppercase extension for camera raw files, else None. Present because a raw's
    /// true sensor size is vendor-specific; we show the format rather than the
    /// dimensions of the preview we decoded.
    kind: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Record {
    photo: Photo,
    sha: Option<String>,
    phash: Option<u128>,
}

#[derive(Default)]
struct ScanData {
    /// Embeddings computed so far, keyed by file path. Filled lazily: only photographs
    /// the fingerprint has already proposed as candidates are ever run through the
    /// model, so the cost follows the number of suspected duplicates rather than the
    /// size of the library.
    embeddings: HashMap<String, Vec<f32>>,
    /// Files skipped because their contents live in the cloud, kept so the user can
    /// ask for them without walking the folder again.
    offline: Vec<PathBuf>,
    records: Vec<Record>,
    trashed: HashMap<String, Vec<Record>>,
    /// The folders this scan was given, in the order they were given.
    ///
    /// Kept because an export has to turn every absolute path into a path relative to
    /// one of these — that relativity is the whole reason a project can be opened on
    /// another machine at all.
    roots: Vec<String>,
}

struct ScanState(Mutex<ScanData>);

/* Every lock in this file is taken with `unwrap_or_else(|e| e.into_inner())` rather
   than `unwrap()`.
   A poisoned mutex means another thread panicked while holding it. Propagating that
   panic at every later acquisition turns one failure into a permanent one: the next
   scan panics, and the next regroup, and the next settings read, and the window stays
   open and dead until the user quits and loses the scan. What these locks protect is a
   scan result — at worst stale, or half-updated, which scanning again repairs. A
   recoverable inconsistency is a far better outcome than an application that cannot be
   used, so the poison is stepped over deliberately rather than by accident. */

#[derive(Serialize, Clone)]
struct Progress {
    done: usize,
    total: usize,
    phase: u8,
}

/// Caps `scan-progress` events to roughly one every 250ms or 100 items, whichever
/// comes first, instead of a fixed item-count cadence alone: a fixed count either
/// leaves a slow phase (large raw files) visibly frozen for seconds between updates,
/// or fires far more IPC round-trips than a fast phase (cached hits) can usefully
/// render. Shared across `rayon` workers, so a race letting two threads both decide to
/// emit in the same window is tolerated rather than locked against — a couple of extra
/// events are harmless for a progress bar, and `total` always gets through so the UI
/// lands on a complete bar even if the last few items land in the same window.
struct ProgressThrottle {
    start: std::time::Instant,
    last_emit_ms: std::sync::atomic::AtomicU64,
}

impl ProgressThrottle {
    fn new() -> Self {
        ProgressThrottle {
            start: std::time::Instant::now(),
            last_emit_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn should_emit(&self, done: usize, total: usize) -> bool {
        if done == total || done.is_multiple_of(100) {
            self.mark_emitted();
            return true;
        }
        let now_ms = self.start.elapsed().as_millis() as u64;
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) >= 250 {
            self.mark_emitted();
            true
        } else {
            false
        }
    }

    fn mark_emitted(&self) {
        let now_ms = self.start.elapsed().as_millis() as u64;
        self.last_emit_ms.store(now_ms, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
struct Group {
    /// Indices into View.photos
    indices: Vec<usize>,
    /// Position within `indices` of the suggested keeper
    suggested: usize,
    kind: &'static str,
    similarity: u32,
    /// Which criterion actually decided the suggestion, so the interface can say it.
    /// None when nothing separates the keeper from its closest rival.
    reason: Option<&'static str>,
}

#[derive(Serialize)]
struct View {
    photos: Vec<Photo>,
    groups: Vec<Group>,
    reclaimable_bytes: u64,
    total_files: usize,
}

#[derive(Serialize)]
struct TrashResult {
    batch_id: String,
    count: usize,
}

#[derive(Serialize)]
struct TrashedPhoto {
    /// Where the file currently sits
    stored_path: String,
    /// What the webview can actually display: the moved file, or its cached rendition
    preview: String,
    original: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
struct TrashBatch {
    batch_id: String,
    /// Unix milliseconds, taken from the batch folder name
    when: i64,
    photos: Vec<TrashedPhoto>,
    bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct ManifestEntry {
    original: String,
    stored: String,
    /// Cached rendition for formats the webview cannot show; absent for ordinary images.
    #[serde(default)]
    preview: Option<String>,
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 64-bit difference hash: 9×8 grayscale, one bit per horizontal gradient sign.
/// Second opinion on the fingerprint's proposals.
///
/// The fingerprint is fast and catches plenty, but on a real folder it placed a wide
/// shot of a temple gate between two frames of a burst, 25 bits from the seed against
/// 23 and 30. No threshold separates that, because the fingerprint compares where
/// pixels sit rather than what the photograph shows.
///
/// A small vision model does separate it: on the same folder the two burst frames sit
/// 0.079 and 0.082 away while the next photograph sits 0.189, a margin of 2.3x where
/// the fingerprint managed 0.83. So the fingerprint proposes and the model disposes,
/// which also keeps inference off every photograph in the library.
pub mod badshot;

mod refine {
    use super::*;
    use candle_core::{Device, Tensor};

    /// Beyond this cosine distance two photographs are different pictures. Set between
    /// the measured populations: burst frames landed under 0.09, everything else above
    /// 0.18, so the midpoint leaves room on both sides.
    pub const MAX_DISTANCE: f32 = 0.13;

    const SIDE: usize = 224;
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];

    pub fn model_path(app: &AppHandle) -> Option<PathBuf> {
        app.path()
            .resolve(
                "models/mobilenetv2.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .ok()
            .filter(|p| p.exists())
    }

    pub fn embed(model: &candle_onnx::onnx::ModelProto, preview: &str) -> Option<Vec<f32>> {
        let img = image::open(preview)
            .ok()?
            .resize_exact(SIDE as u32, SIDE as u32, FilterType::Triangle)
            .to_rgb8();
        let mut data = vec![0f32; 3 * SIDE * SIDE];
        for y in 0..SIDE {
            for x in 0..SIDE {
                let p = img.get_pixel(x as u32, y as u32).0;
                for c in 0..3 {
                    data[c * SIDE * SIDE + y * SIDE + x] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
                }
            }
        }
        let input = Tensor::from_vec(data, (1, 3, SIDE, SIDE), &Device::Cpu).ok()?;
        let name = model.graph.as_ref()?.input.first()?.name.clone();
        let mut inputs = HashMap::new();
        inputs.insert(name, input);
        let out = candle_onnx::simple_eval(model, inputs).ok()?;
        let v = out
            .values()
            .next()?
            .flatten_all()
            .ok()?
            .to_vec1::<f32>()
            .ok()?;
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        Some(v.into_iter().map(|x| x / norm).collect())
    }

    pub fn distance(a: &[f32], b: &[f32]) -> f32 {
        1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
    }
}

/// Width of the perceptual fingerprint. Anything scaling a distance against it must
/// use this rather than a literal, which is how a stale 64 survived the widening and
/// reported similarities of 67 million.
const FINGERPRINT_BITS: u32 = 128;

/// Perceptual fingerprint, or `None` when the frame carries no structure to hash.
///
/// 128 bits in three parts, because 64 bits of horizontal grey gradient was measurably
/// not enough: on a real folder a daylight photograph of a moat landed 11 bits from a
/// night shot of a temple, closer than two genuine duplicates of that temple. No
/// threshold can separate images the fingerprint itself cannot tell apart.
///
///  * 64 bits, horizontal luma gradient, as before
///  * 32 bits, vertical luma gradient, which sees composition the horizontal pass misses
///  * 32 bits, colour, which the grey thumbnail threw away entirely and which separates
///    a red temple at night from green trees at noon at a glance
///
/// 256 bits was measured on the same folder and was worse: a finer grid pushed genuine
/// duplicates from 21-28% apart to 27-34%, while the unrelated photograph barely moved,
/// shrinking the separation from 1.81x to 1.56x. Fine detail is exactly what resizing
/// and recompression destroy, so past a point extra resolution costs recall and buys
/// almost no precision. What helped here was new dimensions, not more of the same one.
///
/// A featureless frame still refuses to answer: on clear sky or a wall the comparisons
/// decide on noise, and two unrelated photographs come out identical.
#[cfg(test)]
fn score_of(g: &GrayImage) -> f64 {
    sharpness_with(g, true)
}

/// L'empreinte telle qu'elle serait si la réduction en 9x9 se faisait en amont.
#[cfg(test)]
fn fingerprint_from_9x9(_small: &image::RgbImage) -> Option<u128> {
    fingerprint(_small)
}

fn fingerprint(rgb: &image::RgbImage) -> Option<u128> {
    /// A real but very dark photograph measures around 17; a gradient sky, 1.3.
    const MIN_STRUCTURE: f64 = 6.0;

    /* Do not try to speed this up by averaging the image down before the resize.
       Measured on 400 real library derivatives and 31 full-size photographs: a /8
       pre-reduction is 3.7x faster (15.9s -> 4.3s) and leaves most hashes bit-identical,
       but it pushes a HEIC and its own JPEG export past the 6-bit equivalence
       `heic_pipeline_groups_with_its_jpeg_export` asserts — iPhone photos would stop
       grouping with their exports. A /2 pre-reduction keeps that property but buys only
       1.4x, which is not worth perturbing a fingerprint whose separation margin was
       measured. The cost is paid once per file and cached; that is the right lever. */
    let luma = image::imageops::resize(rgb, 9, 9, FilterType::Triangle);
    let grey = |x: u32, y: u32| {
        let p = luma.get_pixel(x, y).0;
        0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64
    };

    let mut values = Vec::with_capacity(81);
    for y in 0..9 {
        for x in 0..9 {
            values.push(grey(x, y));
        }
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    if variance < MIN_STRUCTURE {
        return None;
    }

    let mut bits: u128 = 0;
    let mut at = 0;
    let mut set = |on: bool, at: &mut u32| {
        if on {
            bits |= 1u128 << *at;
        }
        *at += 1;
    };

    for y in 0..8u32 {
        for x in 0..8u32 {
            set(grey(x, y) < grey(x + 1, y), &mut at);
        }
    }
    // Vertical pass on a coarser grid: 32 bits is plenty to catch a different layout.
    for y in 0..4u32 {
        for x in 0..8u32 {
            set(grey(x, y * 2) < grey(x, y * 2 + 1), &mut at);
        }
    }
    /* Colour, on a 4×4 grid. The comparisons are red against green and green against
    blue, not both against blue: a warm night scene and a green daylight one are both
    red-over-blue and green-over-blue, so that encoding put them 5 bits apart when
    they should be nowhere near each other. */
    let small = image::imageops::resize(rgb, 4, 4, FilterType::Triangle);
    for y in 0..4u32 {
        for x in 0..4u32 {
            let p = small.get_pixel(x, y).0;
            set(p[0] as i32 > p[1] as i32 + 8, &mut at);
            set(p[1] as i32 > p[2] as i32 + 8, &mut at);
        }
    }
    Some(bits)
}

/// A 3×3 box blur, used both to damp sensor noise before measuring and to produce the
/// deliberately softened copy the sharpness ratio compares against.
fn box_blur3(g: &GrayImage) -> GrayImage {
    let (w, h) = g.dimensions();
    let mut out = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let src = g.as_raw();
    let dst = out.as_mut();
    let (wi, hi) = (w as usize, h as usize);

    /* Same arithmetic as the straightforward version, addressed through the buffer
    rather than through get_pixel: the borders keep their own neighbour count, so a
    corner still averages four samples and an edge six. The interior is split out
    because there the count is always nine and the bounds tests are pure overhead;
    integer addition is associative, so summing three column sums gives the byte the
    nested loop gave. `equivalent_to_the_straightforward_version` holds this. */
    for y in 1..hi.saturating_sub(1) {
        let (up, mid, down) = ((y - 1) * wi, y * wi, (y + 1) * wi);
        for x in 1..wi.saturating_sub(1) {
            let col = |o: usize| src[o + x - 1] as u32 + src[o + x] as u32 + src[o + x + 1] as u32;
            dst[mid + x] = ((col(up) + col(mid) + col(down)) / 9) as u8;
        }
    }

    let mut border = |x: usize, y: usize| {
        let mut sum = 0u32;
        let mut n = 0u32;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                if sx >= 0 && sy >= 0 && (sx as usize) < wi && (sy as usize) < hi {
                    sum += src[sy as usize * wi + sx as usize] as u32;
                    n += 1;
                }
            }
        }
        dst[y * wi + x] = (sum / n) as u8;
    };
    for x in 0..wi {
        border(x, 0);
        if hi > 1 {
            border(x, hi - 1);
        }
    }
    for y in 1..hi.saturating_sub(1) {
        border(0, y);
        if wi > 1 {
            border(wi - 1, y);
        }
    }
    out
}

/// Laplacian energy, corrected for contrast in one direction only.
///
/// Raw Laplacian variance scales with contrast as well as with focus, so a sharp
/// photograph shot in haze reads as a failure. Dividing by the region's own contrast
/// fixes that — and breaks the opposite case: a punchy, well-exposed frame gets divided
/// by a large number and sinks below the threshold, which is exactly what users report
/// as "it calls my good photos blurry".
///
/// The correction is therefore capped. Regions duller than `REFERENCE_CONTRAST` are
/// lifted in proportion to how dull they are; everything above it is divided by the same
/// constant, so ordinary photographs keep their spread and are ranked among themselves
/// on focus alone.
fn normalised_detail(g: &GrayImage, x0: u32, y0: u32, x1: u32, y1: u32) -> Option<f64> {
    /// Pixel variance of a typical well-exposed region, measured on the bench fixtures.
    const REFERENCE_CONTRAST: f64 = 200.0;

    let lap = laplacian_variance_region(g, x0, y0, x1, y1);
    if lap < 0.5 {
        return None;
    }
    let (buf, stride) = (g.as_raw(), g.width() as usize);
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let mut n = 0.0;
    for y in y0..y1 {
        let row = &buf[y as usize * stride + x0 as usize..y as usize * stride + x1 as usize];
        for &b in row {
            let v = b as f64;
            sum += v;
            sum_sq += v * v;
            n += 1.0;
        }
    }
    let contrast = (sum_sq / n - (sum / n).powi(2)).max(1.0);
    Some(lap / contrast.min(REFERENCE_CONTRAST))
}

/// Sharpness score.
///
/// A photograph is in focus when *something* in it is sharp: a portrait against a melted
/// background is a keeper, not a mistake. So the frame is measured region by region, and
/// the score is the third-best region rather than the best. One incrusted timestamp or
/// speck of sensor dust makes a single region sharp, which under a maximum was enough to
/// rescue a wholly blurred photograph.
///
/// Each region is divided by its own contrast. Raw Laplacian energy scales with contrast
/// as much as with focus, which made a sharp photograph shot in haze score like a failed
/// one. See `sharpness_separates_sharp_from_blurred` for the labelled cases this is held
/// against.
fn sharpness(gray: &GrayImage) -> f64 {
    sharpness_with(gray, true)
}

/// `normalise` divides each tile by its own contrast; the bench uses it to attribute
/// the improvement between the two independent changes made here.
fn sharpness_with(gray: &GrayImage, normalise: bool) -> f64 {
    const WORK_WIDTH: u32 = 1024;
    const TILE: u32 = 64;
    /// The score is the Nth best tile, not the best and not their average. A single
    /// incrusted timestamp, watermark or speck of sensor dust makes one tile sharp,
    /// which is enough to rescue a wholly blurred photo under a maximum, and enough to
    /// drag an average upward. An order statistic ignores it: a photograph has to be
    /// sharp in several places at once to be called sharp.
    const NTH_BEST: usize = 3;
    /// Share of each side kept for the second, centre-only reading.
    const CENTRE: f64 = 0.55;
    /// The middle gets a veto, but a softened one. A strict minimum scored best on
    /// blurred subjects and worst on the contrast pair the user reported, flipping two
    /// frames out of sixty; multiplying the centre by this before comparing keeps the
    /// gain and gets that pair right sixty times out of sixty.
    const CENTRE_SLACK: f64 = 1.4;

    let resized;
    let g = if gray.width() > WORK_WIDTH {
        let h = ((gray.height() as f64 * WORK_WIDTH as f64 / gray.width() as f64).round() as u32)
            .max(1);
        resized = image::imageops::resize(gray, WORK_WIDTH, h, FilterType::Triangle);
        &resized
    } else {
        gray
    };
    let (w, h) = g.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }

    // One mild smoothing first: raw sensor noise is high-frequency detail, and without
    // this a noisy blurred frame measures as the sharpest thing in the folder.
    let base = box_blur3(g);

    let score = |x_from: u32, y_from: u32, x_to: u32, y_to: u32| -> f64 {
        let mut tiles: Vec<f64> = Vec::new();
        let mut y0 = y_from;
        while y0 + 3 <= y_to {
            let y1 = (y0 + TILE).min(y_to);
            let mut x0 = x_from;
            while x0 + 3 <= x_to {
                let x1 = (x0 + TILE).min(x_to);
                let value = if normalise {
                    normalised_detail(&base, x0, y0, x1, y1)
                } else {
                    let v = laplacian_variance_region(&base, x0, y0, x1, y1);
                    (v >= 0.5).then_some(v)
                };
                if let Some(r) = value {
                    tiles.push(r);
                }
                x0 += TILE;
            }
            y0 += TILE;
        }
        if tiles.is_empty() {
            return 0.0;
        }
        tiles.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        tiles[(NTH_BEST - 1).min(tiles.len() - 1)]
    };

    let whole = score(0, 0, w, h);

    /* And the same reading again over the middle of the frame only, keeping whichever
    is worse. Scoring the whole frame lets a crisp background rescue a photograph
    whose subject is soft, which is the commonest way a portrait fails: measured on
    60 real frames with the middle deliberately blurred, the whole-frame score only
    fell to 80% of the original and separated the two populations 61.6% of the time.
    Taking the lower of the two brings that to 31% and 88.9%.
    It does not punish a deliberately melted background either, since the subject
    sits in the middle and stays sharp there.
    Measured over 60 frames, AUC against a blurred subject: 61.6% whole-frame,
    87.9% here. The cost is one point on uniform blur (99.4% to 98.9% at sigma 1.5),
    which is the right side of that trade. */
    let cw = ((w as f64 * CENTRE) as u32).max(TILE.min(w));
    let ch = ((h as f64 * CENTRE) as u32).max(TILE.min(h));
    let centre = score(
        (w - cw) / 2,
        (h - ch) / 2,
        (w - cw) / 2 + cw,
        (h - ch) / 2 + ch,
    );

    if centre <= 0.0 {
        // A featureless middle says nothing; do not let it condemn the photograph.
        whole
    } else {
        whole.min(centre * CENTRE_SLACK)
    }
}

/// The measure Skimrr shipped through 0.2.0, kept so the bench can show what changed.
#[cfg(test)]
fn sharpness_legacy(gray: &GrayImage) -> f64 {
    const WORK_WIDTH: u32 = 1024;
    const TILE: u32 = 64;

    let resized;
    let g = if gray.width() > WORK_WIDTH {
        let h = ((gray.height() as f64 * WORK_WIDTH as f64 / gray.width() as f64).round() as u32)
            .max(1);
        resized = image::imageops::resize(gray, WORK_WIDTH, h, FilterType::Triangle);
        &resized
    } else {
        gray
    };
    let (w, h) = g.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }

    let mut tiles: Vec<f64> = Vec::new();
    let mut y0 = 0;
    while y0 + 3 <= h {
        let y1 = (y0 + TILE).min(h);
        let mut x0 = 0;
        while x0 + 3 <= w {
            let x1 = (x0 + TILE).min(w);
            tiles.push(laplacian_variance_region(g, x0, y0, x1, y1));
            x0 += TILE;
        }
        y0 += TILE;
    }
    if tiles.is_empty() {
        return laplacian_variance_region(g, 0, 0, w, h);
    }
    tiles.into_iter().fold(0.0, f64::max)
}

/// Variance of the 3×3 Laplacian response inside one region.
fn laplacian_variance_region(g: &GrayImage, x0: u32, y0: u32, x1: u32, y1: u32) -> f64 {
    if x1 < x0 + 3 || y1 < y0 + 3 {
        return 0.0;
    }
    // Straight into the buffer: this runs once per tile per pass, twice per photograph,
    // and get_pixel bounds-checks every one of the five samples it needs.
    let (buf, stride) = (g.as_raw(), g.width() as usize);
    let px = |x: u32, y: u32| buf[y as usize * stride + x as usize] as f64;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let n = ((x1 - x0 - 2) * (y1 - y0 - 2)) as f64;
    for y in y0 + 1..y1 - 1 {
        for x in x0 + 1..x1 - 1 {
            let v = px(x + 1, y) + px(x - 1, y) + px(x, y + 1) + px(x, y - 1) - 4.0 * px(x, y);
            sum += v;
            sum_sq += v * v;
        }
    }
    sum_sq / n - (sum / n).powi(2)
}

/// Camera raw files are TIFF-like containers holding a full-size JPEG rendition.
/// Reading that rendition is both far cheaper than demosaicing the sensor data and
/// closer to what the photographer saw, which is what a perceptual hash wants.
///
/// Raw payloads regularly contain stray `FF D8 FF` byte sequences, so a candidate is
/// only trusted once the JPEG header parses: picking on byte length alone yields a
/// corrupt slice that swallows the real preview.
fn largest_embedded_jpeg(data: &[u8]) -> Option<&[u8]> {
    let mut best: Option<(&[u8], u64)> = None;
    let mut search = 0;
    while let Some(rel) = data[search..]
        .windows(3)
        .position(|w| w == [0xFF, 0xD8, 0xFF])
    {
        let start = search + rel;
        search = start + 3;
        let candidate = &data[start..];
        if candidate.len() < 1024 {
            continue;
        }
        // Reading just the header is cheap; only the winner gets fully decoded.
        let reader = image::ImageReader::with_format(
            std::io::Cursor::new(candidate),
            image::ImageFormat::Jpeg,
        );
        let Ok((w, h)) = reader.into_dimensions() else {
            continue;
        };
        let pixels = w as u64 * h as u64;
        if best.is_none_or(|(_, b)| pixels > b) {
            best = Some((candidate, pixels));
        }
    }
    best.map(|(slice, _)| slice)
}

/// Minimal TIFF/IFD walk that tolerates the vendor magic numbers raw formats use
/// (Sony ARW is `II\x55\x00`, not the standard 42). Returns the largest image
/// dimensions found and the capture date, so a raw is never mistaken for the
/// small preview it carries.
struct RawMeta {
    dims: Option<(u32, u32)>,
    taken: Option<i64>,
    orientation: Option<u16>,
}

fn read_raw_meta(data: &[u8]) -> RawMeta {
    let mut out = RawMeta {
        dims: None,
        taken: None,
        orientation: None,
    };
    if data.len() < 8 {
        return out;
    }
    let le = match &data[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return out,
    };
    let u16at = |o: usize| -> Option<u16> {
        let b = data.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32at = |o: usize| -> Option<u32> {
        let b = data.get(o..o + 4)?;
        Some(if le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };

    let mut best_pixels = 0u64;
    let mut queue = vec![u32at(4).unwrap_or(0) as usize];
    let mut seen = HashSet::new();
    // Raw files nest the full-resolution image in sub-IFDs; walk them breadth-first.
    while let Some(ifd) = queue.pop() {
        if ifd == 0 || ifd + 2 > data.len() || !seen.insert(ifd) || seen.len() > 32 {
            continue;
        }
        let Some(count) = u16at(ifd) else { continue };
        let (mut w, mut h) = (0u32, 0u32);
        for e in 0..count as usize {
            let entry = ifd + 2 + e * 12;
            let (Some(tag), Some(kind), Some(n), Some(value)) = (
                u16at(entry),
                u16at(entry + 2),
                u32at(entry + 4),
                u32at(entry + 8),
            ) else {
                break;
            };
            // SHORT and LONG values under 5 bytes live inline in the entry.
            let scalar = || -> Option<u32> {
                match kind {
                    3 => u16at(entry + 8).map(u32::from),
                    4 => Some(value),
                    _ => None,
                }
            };
            match tag {
                0x0100 | 0xA002 => w = scalar().unwrap_or(w),
                0x0101 | 0xA003 => h = scalar().unwrap_or(h),
                // SubIFDs and the Exif IFD both hold dimensions worth checking.
                0x014A => {
                    if kind == 4 {
                        if n == 1 {
                            queue.push(value as usize);
                        } else {
                            for k in 0..n.min(16) {
                                if let Some(p) = u32at(value as usize + k as usize * 4) {
                                    queue.push(p as usize);
                                }
                            }
                        }
                    }
                }
                0x0112 => {
                    // IFD0 is walked first and is the authoritative one; thumbnail
                    // IFDs carry their own tag and must not override it.
                    if kind == 3 && out.orientation.is_none() {
                        out.orientation = u16at(entry + 8);
                    }
                }
                0x8769 => queue.push(value as usize),
                0x9003 if kind == 2 && n >= 19 => {
                    if let Some(bytes) = data.get(value as usize..value as usize + 19) {
                        if let Ok(s) = std::str::from_utf8(bytes) {
                            for fmt in ["%Y:%m:%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
                                if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
                                    out.taken = Some(dt.and_utc().timestamp());
                                    break;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let pixels = w as u64 * h as u64;
        if pixels > best_pixels {
            best_pixels = pixels;
            out.dims = Some((w, h));
        }
        if let Some(next) = u32at(ifd + 2 + count as usize * 12) {
            queue.push(next as usize);
        }
    }
    out
}

/// HEIC/HEIF is what iPhones shoot by default, and the `image` crate cannot read it,
/// so those files go through a dedicated pure-Rust HEVC decoder.
fn decode_heic(path: &Path) -> Option<image::DynamicImage> {
    let data = std::fs::read(path).ok()?;
    let output = heic::DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgb8)
        .ok()?;
    let buf = image::RgbImage::from_raw(output.width, output.height, output.data)?;
    Some(image::DynamicImage::ImageRgb8(buf))
}

fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

fn is_raw(path: &Path) -> bool {
    ext_of(path).is_some_and(|e| RAW_EXTS.contains(&e.as_str()))
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif"))
        .unwrap_or(false)
}

struct Analysis {
    phash: Option<u128>,
    blur: Option<f64>,
    /// Everything Bad Shot measures that does not depend on the folder: exposure, the
    /// zone reading, faces and their eyes. The verdict built from these is not cached
    /// because it moves with the blur threshold; these numbers never do.
    measurements: badshot::Measurements,
    dims: Option<(u32, u32)>,
    taken: Option<i64>,
    /// A JPEG rendition to cache when the webview cannot display the file itself.
    preview_jpeg: Option<Vec<u8>>,
    /// A small JPEG rendition for grid cells, generated for every format — unlike
    /// `preview_jpeg`, which only exists for formats the webview cannot show at all.
    thumb_jpeg: Option<Vec<u8>>,
}

impl Analysis {
    fn empty() -> Self {
        Analysis {
            phash: None,
            blur: None,
            measurements: badshot::Measurements::default(),
            dims: None,
            taken: None,
            preview_jpeg: None,
            thumb_jpeg: None,
        }
    }
}

/// Cameras record the frame sensor-side up and note how the body was held in an EXIF
/// tag. Browsers honour that tag for files they load directly, but the renditions we
/// encode ourselves carry no EXIF, so the rotation has to be baked in, and the
/// perceptual hash must see the upright image too, or a portrait raw would never match
/// its upright export.
fn apply_orientation(img: image::DynamicImage, orientation: u16) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Quarter turns swap the reported side lengths.
fn oriented_dims((w, h): (u32, u32), orientation: u16) -> (u32, u32) {
    if matches!(orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    }
}

/// EXIF orientation for containers the generic reader understands (JPEG, HEIF, TIFF…).
fn exif_orientation(path: &Path) -> u16 {
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(meta) = exif::Reader::new().read_from_container(&mut reader) else {
        return 1;
    };
    meta.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as u16)
        .unwrap_or(1)
}

/// The camera/phone model from a JPEG's own EXIF `Model` tag — verified against a
/// real cached Photos derivative here: reads back exactly `"iPhone 13"`. Ascii EXIF
/// fields' `display_value()` renders with literal surrounding quote marks, confirmed
/// the same way, hence the trim.
fn camera_model(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let meta = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = meta.get_field(exif::Tag::Model, exif::In::PRIMARY)?;
    let model = field.display_value().to_string();
    let trimmed = model.trim_matches('"').trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Re-encode a decoded image down to something a webview can show cheaply. Sized for
/// the full-screen viewer, not just the grid thumbnails.
fn encode_preview(img: &image::DynamicImage) -> Option<Vec<u8>> {
    const MAX: u32 = 1600;
    let scaled = if img.width() > MAX || img.height() > MAX {
        img.resize(MAX, MAX, FilterType::Triangle)
    } else {
        img.clone()
    };
    let mut out = std::io::Cursor::new(Vec::new());
    scaled
        .to_rgb8()
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .ok()?;
    Some(out.into_inner())
}

/// Re-encode down to grid-thumbnail size — see the `thumb` field doc on `Photo`.
fn encode_thumb(img: &image::DynamicImage) -> Option<Vec<u8>> {
    const MAX: u32 = 320;
    let scaled = if img.width() > MAX || img.height() > MAX {
        img.resize(MAX, MAX, FilterType::Triangle)
    } else {
        img.clone()
    };
    let mut out = std::io::Cursor::new(Vec::new());
    scaled
        .to_rgb8()
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .ok()?;
    Some(out.into_inner())
}

fn analyze_file(path: &Path) -> Analysis {
    analyze_file_with(path, None)
}

/// The same analysis, with a face detector when the caller has one loaded.
///
/// Split rather than threaded through every call site: the detector only exists during
/// a scan, where the model is read once and shared, and the half-dozen other callers —
/// a single thumbnail, a test — have no use for it.
fn analyze_file_with(path: &Path, detector: Option<&candle_onnx::onnx::ModelProto>) -> Analysis {
    if is_raw(path) {
        let Ok(data) = std::fs::read(path) else {
            return Analysis::empty();
        };
        let meta = read_raw_meta(&data);
        let Some(jpeg) = largest_embedded_jpeg(&data) else {
            return Analysis {
                taken: meta.taken,
                ..Analysis::empty()
            };
        };
        let Ok(img) = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg) else {
            return Analysis {
                taken: meta.taken,
                ..Analysis::empty()
            };
        };
        // The embedded rendition is stored sensor-side up; the container's tag says
        // how the camera was actually held.
        let img = apply_orientation(img, meta.orientation.unwrap_or(1));
        let gray = img.to_luma8();
        let rgb = img.to_rgb8();
        return Analysis {
            phash: fingerprint(&rgb),
            blur: Some(sharpness(&gray)),
            measurements: badshot::measure(&rgb, detector),
            // Vendor tags carry the sensor size only on some brands; when they do
            // not, the preview's size is all we can honestly report.
            dims: meta
                .dims
                .map(|d| oriented_dims(d, meta.orientation.unwrap_or(1))),
            taken: meta.taken,
            preview_jpeg: encode_preview(&img),
            thumb_jpeg: encode_thumb(&img),
        };
    }

    if is_heif(path) {
        let Some(img) = decode_heic(path) else {
            return Analysis::empty();
        };
        let img = apply_orientation(img, exif_orientation(path));
        let gray = img.to_luma8();
        let rgb = img.to_rgb8();
        return Analysis {
            phash: fingerprint(&rgb),
            blur: Some(sharpness(&gray)),
            measurements: badshot::measure(&rgb, detector),
            dims: Some((img.width(), img.height())),
            taken: None,
            preview_jpeg: encode_preview(&img),
            thumb_jpeg: encode_thumb(&img),
        };
    }

    match image::open(path) {
        Ok(img) => {
            // The webview applies the EXIF tag itself when it loads the file, so no
            // full-size rendition is cached here — only the small grid thumbnail,
            // which does need the rotation baked in since it is generated fresh.
            let img = apply_orientation(img, exif_orientation(path));
            let gray = img.to_luma8();
            let rgb = img.to_rgb8();
            Analysis {
                phash: fingerprint(&rgb),
                blur: Some(sharpness(&gray)),
                measurements: badshot::measure(&rgb, detector),
                dims: Some(gray.dimensions()),
                taken: None,
                preview_jpeg: None,
                thumb_jpeg: encode_thumb(&img),
            }
        }
        Err(_) => Analysis::empty(),
    }
}

/// Mirrors `analyze_file`'s shape for video files: three sampled frames instead of one
/// decoded image, but the same fingerprint/sharpness/preview fields out the other side,
/// so a video and a photograph slot into the same `Record` and the same clustering pass
/// without either needing to know the other exists.
///
/// `None` for `ffmpeg_bin` (no sidecar resolved — see `video::sidecar_path`) is treated
/// exactly like a file that failed to decode: an empty `Analysis`, not an error, since a
/// scan already tolerates individual files it cannot read.
fn analyze_video(ffmpeg_bin: Option<&Path>, path: &Path) -> Analysis {
    let Some(frames) = ffmpeg_bin.and_then(|bin| video::extract_keyframes(bin, path)) else {
        return Analysis::empty();
    };
    // The median frame first, matching where blur is measured; if it happens to be
    // too featureless to hash (`fingerprint`'s own structure check), fall back to
    // whichever sampled frame does carry enough to fingerprint, same as picking any
    // decodable frame over none.
    let phash = frames
        .median()
        .and_then(fingerprint)
        .or_else(|| frames.frames.iter().flatten().find_map(fingerprint));
    let blur = frames.median().map(|f| sharpness(&image::imageops::grayscale(f)));
    let preview_jpeg = frames.median().and_then(|f| {
        let dynamic = image::DynamicImage::ImageRgb8(f.clone());
        encode_preview(&dynamic)
    });
    Analysis {
        phash,
        blur,
        // Exposure and zones from the same median frame the rest of the video reading
        // uses. No detector: a face in one sampled frame of a clip says nothing about
        // whether the clip is worth keeping, and it would cost a tenth of a second to
        // find out.
        measurements: frames
            .median()
            .map(|f| badshot::measure(f, None))
            .unwrap_or_default(),
        dims: Some((frames.width, frames.height)),
        taken: None,
        preview_jpeg,
        thumb_jpeg: None,
    }
}

fn taken_date(path: &Path, mtime: i64) -> i64 {
    let Ok(file) = std::fs::File::open(path) else {
        return mtime;
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(meta) = exif::Reader::new().read_from_container(&mut reader) else {
        return mtime;
    };
    let Some(field) = meta.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY) else {
        return mtime;
    };
    let s = field.display_value().to_string();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&s, fmt) {
            return dt.and_utc().timestamp();
        }
    }
    mtime
}

/// One coordinate in decimal degrees, from EXIF's split representation: three
/// rationals (degrees, minutes, seconds) in one tag, and the hemisphere letter in
/// another. Both halves are needed — the numbers alone are unsigned, so without the
/// ref tag a photo taken in Santiago is indistinguishable from one taken in Boston.
fn dms_to_degrees(meta: &exif::Exif, coord: exif::Tag, hemisphere: exif::Tag, negative: &str) -> Option<f64> {
    let field = meta.get_field(coord, exif::In::PRIMARY)?;
    let parts = match &field.value {
        exif::Value::Rational(v) if v.len() >= 3 => v,
        _ => return None,
    };
    let degrees = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    if !degrees.is_finite() {
        return None;
    }
    let south_or_west = meta
        .get_field(hemisphere, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string().trim().to_ascii_uppercase() == negative)
        .unwrap_or(false);
    Some(if south_or_west { -degrees } else { degrees })
}

/// What the scan reads out of one EXIF block: where the photograph was taken, and what
/// took it.
///
/// Read together because they sit in the same block. Opening and parsing a file twice
/// to pull two fields out of it is the kind of waste that only becomes visible once a
/// folder has thousands of them in it.
#[derive(Default)]
struct ExifFacts {
    coords: Option<(f64, f64)>,
    model: Option<String>,
}

fn exif_facts(path: &Path) -> ExifFacts {
    let Ok(file) = std::fs::File::open(path) else {
        return ExifFacts::default();
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(meta) = exif::Reader::new().read_from_container(&mut reader) else {
        return ExifFacts::default();
    };

    let coords = (|| {
        let lat = dms_to_degrees(&meta, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, "S")?;
        let lon = dms_to_degrees(&meta, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, "W")?;
        // A camera that writes the tags but never got a fix leaves zeroes, which point
        // at Null Island in the Gulf of Guinea. Nothing is ever photographed there, so
        // the reading is discarded rather than drawn as a visit to the Atlantic.
        if lat == 0.0 && lon == 0.0 {
            return None;
        }
        (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
    })();

    let model = meta
        .get_field(exif::Tag::Model, exif::In::PRIMARY)
        .and_then(|f| {
            let raw = f.display_value().to_string();
            let trimmed = raw.trim_matches('"').trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });

    ExifFacts { coords, model }
}

fn photo_meta(path: &Path, analysis: &Analysis, preview: String) -> Photo {
    let meta = path.metadata().ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Prefer the dimensions we already decoded; imagesize is the fallback for
    // files we could not decode at all.
    let dims = analysis.dims.or_else(|| {
        imagesize::size(path)
            .ok()
            .map(|d| (d.width as u32, d.height as u32))
    });
    let kind = is_raw(path).then(|| ext_of(path).unwrap_or_default().to_ascii_uppercase());
    // Raw containers keep their tags in vendor IFDs the generic reader cannot open, so
    // this yields nothing for them — the same limitation `taken` works around above.
    let facts = exif_facts(path);
    Photo {
        path: path.to_string_lossy().into_owned(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size,
        width: dims.map(|d| d.0).unwrap_or(0),
        height: dims.map(|d| d.1).unwrap_or(0),
        // Raw containers keep their date in vendor IFDs, which the generic EXIF
        // reader cannot open, so the analysis pass hands it over.
        taken: analysis.taken.unwrap_or_else(|| taken_date(path, mtime)),
        blur: analysis.blur,
        measurements: analysis.measurements.clone(),
        bad_shot: badshot::BadShot::default(),
        preview,
        thumb: None,
        library: false,
        missing: false,
        preview_dims: None,
        lat: facts.coords.map(|c| c.0),
        lon: facts.coords.map(|c| c.1),
        format: ext_of(path).unwrap_or_default().to_ascii_uppercase(),
        device: facts.model,
        kind,
    }
}

/// Where cached renditions live. Named after a hash of the path so a rescan of the
/// same library reuses them instead of re-encoding.
fn preview_cache_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_cache_dir().ok()?.join("previews");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Bump whenever the rendition changes shape (baked-in rotation, different size),
/// so stale files on disk are not served for the new behaviour.
const PREVIEW_CACHE_VERSION: u32 = 2;

fn preview_name(path: &Path, tag: &str) -> String {
    let meta = path.metadata().ok();
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let size = meta.map(|m| m.len()).unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(format!("|{PREVIEW_CACHE_VERSION}|{tag}|{mtime}|{size}").as_bytes());
    format!("{:x}.jpg", hasher.finalize())
}

/// Bump when the analysis itself changes meaning, so old entries are not trusted for
/// results they were never computed for.
const SCAN_CACHE_VERSION: u32 = 7;

#[derive(Serialize, Deserialize)]
struct CachedFile {
    mtime: i64,
    size: u64,
    record: Record,
}

#[derive(Serialize, Deserialize, Default)]
struct ScanCache {
    version: u32,
    files: HashMap<String, CachedFile>,
}

/// One cache shared across every folder ever scanned, keyed by each file's own
/// absolute path (`CachedFile` already carries its own mtime/size staleness check).
/// Scanning a different combination of folders — in particular, adding another
/// folder to a scan already on screen — reuses whatever it already has decoded
/// instead of starting over: only genuinely new or changed files cost anything.
/// Folds `records` into the shared cache, keyed by each file's own path.
///
/// Reloads rather than taking the copy its caller read earlier: another scan — a
/// different folder combination, run since or concurrently — may have written entries
/// this one never touched, and those have to survive. This is one ever-growing cache
/// shared by every folder ever scanned, not one scoped to a single run.
fn save_to_scan_cache(cache_file: Option<&PathBuf>, records: &[Record]) {
    let Some(path) = cache_file else {
        return;
    };
    let mut merged = load_scan_cache(Some(path));
    merged.version = SCAN_CACHE_VERSION;
    for r in records {
        let Some(stamp) = file_stamp(Path::new(&r.photo.path)) else {
            continue;
        };
        merged.files.insert(
            r.photo.path.clone(),
            CachedFile {
                mtime: stamp.0,
                size: stamp.1,
                record: r.clone(),
            },
        );
    }
    // An empty index carries nothing and still shows up as a file the settings panel
    // has to explain. A folder with nothing analysable leaves no trace.
    if merged.files.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Ok(bytes) = serde_json::to_vec(&merged) {
        // Best effort: a cache that cannot be written costs time, never correctness.
        let _ = std::fs::write(path, bytes);
    }
}

fn scan_cache_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_cache_dir().ok()?.join("scans");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("cache.json"))
}

fn load_scan_cache(path: Option<&PathBuf>) -> ScanCache {
    let Some(p) = path else {
        return ScanCache::default();
    };
    let Ok(bytes) = std::fs::read(p) else {
        return ScanCache::default();
    };
    match serde_json::from_slice::<ScanCache>(&bytes) {
        Ok(c) if c.version == SCAN_CACHE_VERSION => c,
        _ => ScanCache::default(),
    }
}

/// Directory bundles that look like folders but are managed databases. Walking into a
/// Photos library surfaces its originals as ordinary files; moving one leaves the
/// library pointing at nothing, which costs the user their albums and edits. App
/// bundles are skipped for a duller reason: their icons are not anybody's photos.
const OPAQUE_PACKAGES: [&str; 6] = [
    ".photoslibrary",
    ".photolibrary",
    ".aplibrary",
    ".migratedaplibrary",
    ".lrdata",
    ".app",
];

fn is_opaque_package(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    OPAQUE_PACKAGES.iter().any(|ext| name.ends_with(ext))
}

/// Returned when the chosen folder is itself a managed library, so the UI can explain
/// the export route rather than report a failure.
const IS_LIBRARY: &str = "library";

/// True when the file exists in the directory listing but its contents are not on this
/// disk: iCloud Drive, Dropbox and OneDrive all publish such placeholders. Reading one
/// blocks in an uninterruptible syscall until the download finishes, which on a folder
/// of raws means an app that appears to have hung. A stat costs nothing and never
/// triggers materialisation, unlike opening the file.
#[cfg(target_os = "macos")]
fn is_dataless(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) => m.size() > 0 && m.blocks() == 0,
        Err(_) => false,
    }
}

/// The Windows equivalent: OneDrive marks files kept in the cloud with reparse
/// attributes, and opening one blocks on a download exactly as iCloud does.
#[cfg(target_os = "windows")]
fn is_dataless(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    match std::fs::metadata(path) {
        Ok(m) => {
            let a = m.file_attributes();
            a & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0
        }
        Err(_) => false,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn is_dataless(_path: &Path) -> bool {
    false
}

/// Metadata cheap enough to stat for every file, and sufficient to notice an edit.
fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((mtime, meta.len()))
}

fn run_scan(app: AppHandle, roots: Vec<String>) -> Result<usize, String> {
    let roots: Vec<PathBuf> = roots.into_iter().map(PathBuf::from).collect();
    if roots.is_empty() {
        return Err("not a directory".into());
    }
    for root in &roots {
        if !root.is_dir() {
            return Err("not a directory".into());
        }
        if root.ancestors().any(is_opaque_package) {
            return Err(IS_LIBRARY.into());
        }
    }
    let cache = preview_cache_dir(&app);
    let cancel = app.state::<Cancel>();
    let stopped = || cancel.0.load(Ordering::Relaxed);
    let cache_file = scan_cache_path(&app);
    let cached = load_scan_cache(cache_file.as_ref());
    /* One read of the 233 KB detector for the whole scan, shared by every worker.
       Absent it, the analysis still runs and simply reports no faces — which is the
       right degradation: a missing model must cost the face-aware refinements, never
       the scan itself. */
    let detector = app
        .path()
        .resolve(
            "models/face_detection_yunet_2023mar.onnx",
            tauri::path::BaseDirectory::Resource,
        )
        .ok()
        .filter(|p| p.exists())
        .and_then(|p| candle_onnx::read_file(p).ok());

    // Two selected folders can overlap (one nested inside another, or the very same
    // folder picked twice) — a canonical-path dedupe keeps every file counted once
    // regardless of which of its paths it was reached through.
    let mut seen = HashSet::new();
    let files: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| {
            WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !(e.file_type().is_dir() && is_opaque_package(e.path())))
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .filter(|e| is_image(e.path()))
                .map(|e| e.into_path())
                .collect::<Vec<_>>()
        })
        .filter(|path| seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())))
        .collect();
    // Files whose contents are not on this disk are set aside before anything opens
    // them, so a cloud folder cannot stall the scan in an uninterruptible read.
    let (files, offline): (Vec<PathBuf>, Vec<PathBuf>) =
        files.into_iter().partition(|f| !is_dataless(f));
    let total = files.len();
    let _ = app.emit("scan-skipped", offline.len());
    {
        let state = app.state::<ScanState>();
        state.0.lock().unwrap_or_else(|e| e.into_inner()).offline = offline;
    }
    if stopped() {
        return Err(CANCELLED.into());
    }

    // Phase 1, exact duplicates: only files sharing a byte size can be identical.
    let mut by_size: HashMap<u64, Vec<&PathBuf>> = HashMap::new();
    for f in &files {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        by_size.entry(len).or_default().push(f);
    }
    let candidates: Vec<&PathBuf> = by_size
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    let hash_total = candidates.len();
    let done = AtomicUsize::new(0);
    let throttle = ProgressThrottle::new();
    let _ = app.emit(
        "scan-progress",
        Progress {
            done: 0,
            total: hash_total,
            phase: 1,
        },
    );
    let shas: HashMap<String, String> = candidates
        .into_par_iter()
        .filter_map(|path| {
            if stopped() {
                return None;
            }
            let result = hash_file(path)
                .ok()
                .map(|h| (path.to_string_lossy().into_owned(), h));
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if throttle.should_emit(d, hash_total) {
                let _ = app.emit(
                    "scan-progress",
                    Progress {
                        done: d,
                        total: hash_total,
                        phase: 1,
                    },
                );
            }
            result
        })
        .collect();

    // Phase 2, decode every image once for perceptual hash + sharpness + EXIF date.
    let done = AtomicUsize::new(0);
    let throttle = ProgressThrottle::new();
    let _ = app.emit(
        "scan-progress",
        Progress {
            done: 0,
            total,
            phase: 2,
        },
    );
    if stopped() {
        return Err(CANCELLED.into());
    }
    let records: Vec<Record> = files
        .into_par_iter()
        .filter_map(|path| {
            if stopped() {
                return None;
            }
            let key = path.to_string_lossy().into_owned();
            // Decoding is the expensive half of a scan. Skip it when neither the size
            // nor the modification time has moved, and the rendition is still on disk.
            if let (Some(hit), Some(stamp)) = (cached.files.get(&key), file_stamp(&path)) {
                if hit.mtime == stamp.0
                    && hit.size == stamp.1
                    && (hit.record.photo.preview == key
                        || Path::new(&hit.record.photo.preview).exists())
                    && hit
                        .record
                        .photo
                        .thumb
                        .as_deref()
                        .is_none_or(|p| Path::new(p).exists())
                {
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if throttle.should_emit(d, total) {
                        let _ = app.emit(
                            "scan-progress",
                            Progress {
                                done: d,
                                total,
                                phase: 2,
                            },
                        );
                    }
                    let mut record = hit.record.clone();
                    // The exact-duplicate pass is cheap and rerun every time, so its
                    // verdict always comes from this scan rather than the cache.
                    record.sha = shas.get(&key).cloned();
                    return Some(record);
                }
            }
            let analysis = analyze_file_with(&path, detector.as_ref());
            // Formats the webview cannot render get a cached JPEG rendition; the rest
            // are displayed straight from disk.
            let preview = match (&analysis.preview_jpeg, &cache) {
                (Some(bytes), Some(dir)) => {
                    let file = dir.join(preview_name(&path, "grid"));
                    if !file.exists() {
                        let _ = std::fs::write(&file, bytes);
                    }
                    file.to_string_lossy().into_owned()
                }
                _ => path.to_string_lossy().into_owned(),
            };
            // The grid thumbnail, unlike `preview`, is cached for every format —
            // see the `thumb` field doc on `Photo` for why.
            let thumb = match (&analysis.thumb_jpeg, &cache) {
                (Some(bytes), Some(dir)) => {
                    let file = dir.join(preview_name(&path, "thumb"));
                    if !file.exists() {
                        let _ = std::fs::write(&file, bytes);
                    }
                    Some(file.to_string_lossy().into_owned())
                }
                _ => None,
            };
            let mut photo = photo_meta(&path, &analysis, preview);
            photo.thumb = thumb;
            let sha = shas.get(&photo.path).cloned();
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if throttle.should_emit(d, total) {
                let _ = app.emit(
                    "scan-progress",
                    Progress {
                        done: d,
                        total,
                        phase: 2,
                    },
                );
            }
            Some(Record {
                photo,
                sha,
                phash: analysis.phash,
            })
        })
        .collect();

    // Half a library analysed is not a result: discard it rather than show a
    // partial view the user would mistake for the whole folder.
    if stopped() {
        return Err(CANCELLED.into());
    }

    save_to_scan_cache(cache_file.as_ref(), &records);

    let state = app.state::<ScanState>();
    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    data.records = records;
    data.trashed.clear();
    data.roots = roots.iter().map(|r| r.to_string_lossy().into_owned()).collect();
    Ok(total)
}

fn uf_find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent[rb] = ra;
    }
}

/// The order in which one rendition of a shot beats another: the copy already in the
/// Photos library before any copy on disk, then a raw before its export, then the
/// larger frame, then the sharper one, then the more recent.
///
/// The library comes first for a reason that is still not about quality, though no
/// longer about impossibility: a library asset carries no sharpness score at all — only
/// a resampled derivative is available locally — so every criterion below it would
/// judge it on a blank. Defaulting to the copy that is already filed and backed up, and
/// clearing the loose one, is the sane resolution of a group spanning both sources.
/// It is only a default: choosing the other copy now genuinely works.
fn better(a: &Photo, b: &Photo) -> std::cmp::Ordering {
    a.library
        .cmp(&b.library)
        .then_with(|| a.kind.is_some().cmp(&b.kind.is_some()))
        .then_with(|| (a.width as u64 * a.height as u64).cmp(&(b.width as u64 * b.height as u64)))
        .then_with(|| a.blur.unwrap_or(0.0).total_cmp(&b.blur.unwrap_or(0.0)))
        .then_with(|| a.taken.cmp(&b.taken))
}

/// The first criterion that actually separated the keeper from its closest rival.
///
/// Reading it back rather than storing it during the comparison keeps one definition
/// of the order: `better` decides, this only reports. None means the two are equal on
/// every count, which happens and should be admitted rather than dressed up.
fn deciding_reason(keeper: &Photo, rival: &Photo) -> Option<&'static str> {
    if keeper.library != rival.library {
        return Some("library");
    }
    if keeper.kind.is_some() != rival.kind.is_some() {
        return Some("raw");
    }
    let pixels = |p: &Photo| p.width as u64 * p.height as u64;
    if pixels(keeper) != pixels(rival) {
        return Some("pixels");
    }
    if keeper.blur.unwrap_or(0.0) != rival.blur.unwrap_or(0.0) {
        return Some("sharp");
    }
    if keeper.taken != rival.taken {
        return Some("recent");
    }
    None
}

/// The folder and the file name without its extension, lowercased.
///
/// A camera writes `DSC04812.ARW` and `DSC04812.JPG` for one press of the shutter, so
/// the stem identifies the shot with certainty where a fingerprint only guesses. The
/// folder is part of the key: the same counter comes round again on the next card, and
/// `DSC04812` from two different imports is two different photographs.
fn stem_key(path: &Path) -> Option<(PathBuf, String)> {
    let parent = path.parent()?.to_path_buf();
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    if stem.is_empty() {
        return None;
    }
    Some((parent, stem))
}

/// The whole view: clustering *and* every photograph, with its Bad Shot verdict.
///
/// What a finished scan, an import, or a change to the day filter needs — anything that
/// changes which photographs are in play.
fn compute_view(records: &[Record], threshold: u32) -> View {
    build_view(records, threshold, true)
}

/// The clustering alone, leaving `photos` empty.
///
/// For the one case that dominates in practice: the similarity slider. Moving it cannot
/// change a single photograph — the Bad Shot cuts are percentiles of the folder's own
/// readings and do not depend on the threshold — so the 21 MB of identical photographs
/// that used to cross the bridge on every 200 ms tick said nothing at all. Measured at
/// n=50,000: 21.8 MB per regroup, of which 21.4 MB was photographs and 0.4 MB was the
/// groups that had actually changed.
fn compute_groups(records: &[Record], threshold: u32) -> View {
    build_view(records, threshold, false)
}

fn build_view(records: &[Record], threshold: u32, with_photos: bool) -> View {
    let n = records.len();
    let mut parent: Vec<usize> = (0..n).collect();

    // Exact duplicates share a SHA-256.
    let mut first_sha: HashMap<&str, usize> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        if let Some(sha) = &r.sha {
            match first_sha.get(sha.as_str()) {
                Some(&j) => uf_union(&mut parent, j, i),
                None => {
                    first_sha.insert(sha, i);
                }
            }
        }
    }

    // A raw and its JPEG export: same folder, same stem, one of them raw. This is an
    // exact fact rather than a resemblance, so it holds even when the JPEG was edited
    // hard enough to fingerprint as a different picture, which is where the perceptual
    // pass gives up. Joining here rather than in a pass of its own keeps the invariant
    // that a photograph belongs to exactly one group.
    let mut by_stem: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
    for (i, r) in records.iter().enumerate() {
        if let Some(key) = stem_key(Path::new(&r.photo.path)) {
            by_stem.entry(key).or_default().push(i);
        }
    }
    for members in by_stem.values() {
        let holds_raw = members
            .iter()
            .any(|&i| is_raw(Path::new(&records[i].photo.path)));
        if members.len() < 2 || !holds_raw {
            continue;
        }
        for &j in &members[1..] {
            uf_union(&mut parent, members[0], j);
        }
    }

    // Near duplicates: every member must resemble the same seed, not merely the member
    // that happened to arrive before it. Joining any pair under the threshold is
    // transitive, so A close to B and B close to C drags A and C together even when they
    // are twice the threshold apart; on a real library a handful of near-matches then
    // collapses into one absurd cluster. Clustering around a seed bounds every group.
    //
    // A BK-tree (bktree.rs) was tried here to avoid the O(n^2) scan, but measured 18x
    // *slower* than this plain scan at n=50,000, both on uniform-random hashes and on
    // realistic clustered ones: at the app's threshold (28 of 128 bits), the pruning
    // window is far wider than the ~5.7-bit standard deviation of Hamming distance
    // between unrelated hashes, so almost nothing gets pruned and the tree just pays
    // pointer-chasing overhead.
    //
    // This scan is also deliberately *not* parallel, which is the second thing measured
    // here rather than assumed. `bench_regroup_phases` at n=50,000 on eight cores:
    //
    //     sequential                971 ms
    //     rayon, once per seed     3073 ms   3.2x slower
    //     rayon above 20k only     2288 ms
    //
    // Entering the pool once per seed loses badly, and not only because of the fifty
    // thousand fork/joins: the predicate is an XOR and a popcount, and it is almost
    // always false — 4,999 hits in 1.25 billion tests — so a parallel `collect` spends
    // its time merging per-thread vectors that are empty. Cheap predicate, rare hit,
    // sequential wins. Restoring the parallelism would need a real dataset showing
    // otherwise; the shape of this one says no.
    let phashes: Vec<Option<u128>> = records.iter().map(|r| r.phash).collect();
    let mut taken = vec![false; n];
    for i in 0..n {
        let Some(seed) = phashes[i] else { continue };
        if taken[i] {
            continue;
        }
        let members: Vec<usize> = (i + 1..n)
            .filter(|&j| {
                !taken[j] && phashes[j].is_some_and(|h| (seed ^ h).count_ones() <= threshold)
            })
            .collect();
        for j in members {
            taken[j] = true;
            uf_union(&mut parent, i, j);
        }
        taken[i] = true;
    }

    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = uf_find(&mut parent, i);
        clusters.entry(root).or_default().push(i);
    }

    let mut groups: Vec<Group> = clusters
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| {
            let first_sha = records[members[0]].sha.as_deref();
            let exact = first_sha.is_some()
                && members
                    .iter()
                    .all(|&i| records[i].sha.as_deref() == first_sha);
            let similarity = if exact {
                100
            } else {
                let mut max_dist = 0u32;
                for (a, &i) in members.iter().enumerate() {
                    for &j in &members[a + 1..] {
                        if let (Some(x), Some(y)) = (records[i].phash, records[j].phash) {
                            max_dist = max_dist.max((x ^ y).count_ones());
                        }
                    }
                }
                /* The fingerprint is 128 bits wide, and the subtraction is on an
                unsigned integer: with the old 64 here, any distance above 64
                wrapped around and reported a similarity in the millions. */
                (FINGERPRINT_BITS.saturating_sub(max_dist)) * 100 / FINGERPRINT_BITS
            };
            // Keep the best rendition: raw first, then most pixels, then the sharpest
            // (a burst often holds one blurry frame at the same resolution), then the
            // newest.
            let suggested = members
                .iter()
                .enumerate()
                .max_by(|(_, &a), (_, &b)| better(&records[a].photo, &records[b].photo))
                .map(|(pos, _)| pos)
                .unwrap_or(0);

            // Why that one, in the user's terms. The answer is only meaningful against
            // the photograph it beat, so the reason is read off the runner-up rather
            // than off the winner alone.
            let runner_up = members
                .iter()
                .enumerate()
                .filter(|&(pos, _)| pos != suggested)
                .max_by(|(_, &a), (_, &b)| better(&records[a].photo, &records[b].photo))
                .map(|(_, &i)| i);
            let reason = runner_up
                .map(|i| deciding_reason(&records[members[suggested]].photo, &records[i].photo));
            // A cluster whose members all share one stem is that exact pairing and
            // nothing else; say so instead of quoting a resemblance the rule never used.
            let one_stem = {
                let first = stem_key(Path::new(&records[members[0]].photo.path));
                first.is_some()
                    && members
                        .iter()
                        .all(|&i| stem_key(Path::new(&records[i].photo.path)) == first)
            };
            let holds_raw = members
                .iter()
                .any(|&i| is_raw(Path::new(&records[i].photo.path)));
            Group {
                indices: members,
                suggested,
                kind: if one_stem && holds_raw {
                    "pair"
                } else if exact {
                    "exact"
                } else {
                    "similar"
                },
                similarity,
                reason: reason.flatten(),
            }
        })
        .collect();

    /* Groups wholly inside the Photos library used to be dropped as unresolvable. They
       are not: their surplus copies can be handed to Photos for deletion like any
       other, so they are shown and counted like any other. */
    let reclaimable = |g: &Group| {
        g.indices
            .iter()
            .enumerate()
            .filter(|(position, _)| *position != g.suggested)
            .map(|(_, &i)| records[i].photo.size)
            .sum::<u64>()
    };
    groups.sort_by_key(|g| std::cmp::Reverse(reclaimable(g)));
    let reclaimable_bytes = groups.iter().map(reclaimable).sum();

    /* Both Bad Shot thresholds are percentiles of this folder's own readings, for the
       reason the blur cut has always been one: the score measures how much fine detail
       a frame carries, which has no absolute meaning — a folder of night streets sits
       an order of magnitude above a folder of misty landscapes.
       Faces get their own cut rather than sharing the frame's. Measured on real
       photographs, a face scores 0.4-0.6 where its own frame scores 2-3, because skin
       carries little of the high-frequency detail this reading counts. One shared
       threshold marked every portrait blurred. */
    let percentile = |mut v: Vec<f64>, q: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[((v.len() - 1) as f64 * q).floor() as usize]
    };

    // Both cuts exist only to fill in each photograph's verdict, so when the caller is
    // not asking for the photographs there is nothing to sort and nothing to clone.
    let photos = if with_photos {
        let blur_cut = percentile(records.iter().filter_map(|r| r.photo.blur).collect(), 0.05);
        let face_cut = percentile(
            records
                .iter()
                .filter_map(|r| r.photo.measurements.face_sharpness)
                .collect(),
            0.05,
        );
        records
            .iter()
            .map(|r| {
                let mut photo = r.photo.clone();
                photo.bad_shot =
                    badshot::verdict(photo.blur, blur_cut, face_cut, &photo.measurements);
                photo
            })
            .collect()
    } else {
        Vec::new()
    };

    View {
        photos,
        groups,
        reclaimable_bytes,
        total_files: n,
    }
}

fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // Cross-volume moves can't rename; copy then delete.
        Err(_) => {
            std::fs::copy(src, dst)?;
            std::fs::remove_file(src)
        }
    }
}

/// Raised by `cancel_scan` and read at every checkpoint of `run_scan`. A long scan over
/// a large library is otherwise only escapable by killing the app.
#[derive(Default)]
struct Cancel(AtomicBool);

/// Returned when the user stopped the scan themselves, so the UI can go quietly home
/// instead of showing a failure.
const CANCELLED: &str = "cancelled";

#[tauri::command]
async fn scan_folder(app: AppHandle, paths: Vec<String>) -> Result<usize, String> {
    app.state::<Cancel>().0.store(false, Ordering::Relaxed);
    tauri::async_runtime::spawn_blocking(move || run_scan(app, paths))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize, Clone)]
struct OfflineSet {
    count: usize,
    bytes: u64,
}

/// How many files were set aside, and what downloading them would cost. Shown before
/// anything is fetched, because 2.7 GB over a tethered connection is the user's call.
#[tauri::command]
fn offline_set(state: State<ScanState>) -> OfflineSet {
    let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = data
        .offline
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    OfflineSet {
        count: data.offline.len(),
        bytes,
    }
}

/// Kicks off materialisation for a batch of dataless files, without waiting for it to
/// finish: on macOS via `brctl download`, an async request the OS services in the
/// background. Windows has no equivalent CLI trigger, so each file is opened and read
/// on its own throwaway thread instead — a read is what hydrates a OneDrive
/// placeholder, the same "any access blocks until downloaded" behaviour `is_dataless`'s
/// doc comment already describes for iCloud. Either way, the caller polls
/// `is_dataless` separately to find out when a file actually lands.
#[cfg(target_os = "macos")]
fn request_download(chunk: &[PathBuf]) {
    let _ = std::process::Command::new("brctl")
        .arg("download")
        .args(chunk)
        .status();
}

#[cfg(target_os = "windows")]
fn request_download(chunk: &[PathBuf]) {
    for path in chunk.to_vec() {
        std::thread::spawn(move || {
            let _ = std::fs::read(&path);
        });
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn request_download(_chunk: &[PathBuf]) {}

/// Ask the system to materialise the skipped files, then watch their block counts.
/// Polling a stat never blocks, unlike reading, so this stays cancellable throughout
/// and a stalled download is reported instead of hanging forever.
#[tauri::command]
async fn download_offline(app: AppHandle, state: State<'_, ScanState>) -> Result<usize, String> {
    let paths: Vec<PathBuf> = state.0.lock().unwrap_or_else(|e| e.into_inner()).offline.clone();
    if paths.is_empty() {
        return Ok(0);
    }
    app.state::<Cancel>().0.store(false, Ordering::Relaxed);

    for chunk in paths.chunks(40) {
        if app.state::<Cancel>().0.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        request_download(chunk);
    }

    let total = paths.len();
    let mut best = 0usize;
    let mut idle = 0u32;
    let mut tick = 0u32;
    loop {
        if app.state::<Cancel>().0.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let remaining: Vec<PathBuf> = paths.iter().filter(|p| is_dataless(p)).cloned().collect();
        let ready = total - remaining.len();
        let _ = app.emit(
            "download-progress",
            Progress {
                done: ready,
                total,
                phase: 3,
            },
        );
        if remaining.is_empty() {
            return Ok(ready);
        }
        // No movement for two minutes means the system is not going to deliver these.
        if ready > best {
            best = ready;
            idle = 0;
        } else {
            idle += 1;
            if idle > 120 {
                return Err(format!("stalled:{ready}"));
            }
        }
        // A single upfront request does not reliably trigger every file in a large
        // batch — real-world reports of `brctl` silently dropping some requests are
        // common, and a spawned Windows read can just as easily fail or get skipped.
        // Re-asking for whatever is still missing, every 20s, recovers those instead
        // of spending the whole stall window just watching for a download that was
        // never actually requested a second time.
        tick += 1;
        if tick.is_multiple_of(20) {
            for chunk in remaining.chunks(40) {
                request_download(chunk);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[tauri::command]
fn cancel_scan(state: State<Cancel>) {
    state.0.store(true, Ordering::Relaxed);
}

#[tauri::command]
/// Re-clusters the scan at a new threshold, optionally narrowed to a few days.
///
/// `with_photos` is false when only the similarity slider moved: that cannot change a
/// single photograph, so the view comes back with `photos` empty and the caller keeps
/// the ones it already has.
fn regroup(
    app: AppHandle,
    state: State<ScanState>,
    threshold: u32,
    days: Option<Vec<String>>,
    with_photos: bool,
) -> View {
    let mut guard = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let data = &mut *guard;

    // Narrowing to a few days is how a trip folder becomes tractable: the clustering
    // runs over that subset alone, so nothing outside it can be grouped or counted.
    let subset: Option<Vec<Record>> = days.filter(|d| !d.is_empty()).map(|days| {
        let wanted: HashSet<String> = days.into_iter().collect();
        data.records
            .iter()
            .filter(|r| wanted.contains(&day_key(r.photo.taken)))
            .cloned()
            .collect()
    });
    let records: &[Record] = subset.as_deref().unwrap_or(&data.records);

    let mut view = if with_photos {
        compute_view(records, threshold)
    } else {
        compute_groups(records, threshold)
    };
    refine_groups(&app, &mut data.embeddings, records, &mut view);
    view
}

/// Runs the model over the members of each proposed group and drops those that turn out
/// to be different photographs. Only groups reach the model, and only once per file:
/// the fingerprint has already discarded everything it can, so this is the expensive
/// opinion asked sparingly.
/// Runs the model over the members of each proposed group and drops those that turn out
/// to be different photographs.
///
/// Reads the photographs out of `records` rather than out of `view.photos`, because the
/// view may deliberately carry none: a regroup that only moved the similarity slider
/// leaves them out, and indexing an empty array here would turn a saving into a crash.
fn refine_groups(
    app: &AppHandle,
    embeddings: &mut HashMap<String, Vec<f32>>,
    records: &[Record],
    view: &mut View,
) {
    let Some(path) = refine::model_path(app) else {
        return;
    };
    let Ok(model) = candle_onnx::read_file(path) else {
        return;
    };

    for group in view.groups.iter() {
        for &index in &group.indices {
            let Some(photo) = records.get(index).map(|r| &r.photo) else {
                continue;
            };
            if embeddings.contains_key(&photo.path) {
                continue;
            }
            if let Some(v) = refine::embed(&model, &photo.preview) {
                embeddings.insert(photo.path.clone(), v);
            }
        }
    }

    let embeddings = &*embeddings;
    view.groups.retain_mut(|group| {
        let Some(seed) = group
            .indices
            .get(group.suggested)
            .and_then(|&i| records.get(i).and_then(|r| embeddings.get(&r.photo.path)))
        else {
            // No opinion is not a verdict: leave the group as the fingerprint left it.
            return true;
        };
        let keeper = group.indices[group.suggested];
        group.indices.retain(|&i| {
            i == keeper
                || records
                    .get(i)
                    .and_then(|r| embeddings.get(&r.photo.path))
                    .map(|v| refine::distance(seed, v) <= refine::MAX_DISTANCE)
                    .unwrap_or(true)
        });
        group.suggested = group.indices.iter().position(|&i| i == keeper).unwrap_or(0);
        group.indices.len() > 1
    });

    // The headline figures describe the groups, so they are recomputed from what
    // survived rather than left describing the proposal.
    view.reclaimable_bytes = view
        .groups
        .iter()
        .map(|g| {
            g.indices
                .iter()
                .enumerate()
                .filter(|(pos, _)| *pos != g.suggested)
                .map(|(_, &i)| records.get(i).map(|r| r.photo.size).unwrap_or(0))
                .sum::<u64>()
        })
        .sum();
}

/// Calendar day of a capture, as YYYY-MM-DD in the machine's own timezone, which is
/// the day the photographer remembers rather than the one UTC would name.
fn day_key(taken: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(taken, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// One calendar day of the scanned folder, for the gallery.
#[derive(Serialize)]
struct Day {
    key: String,
    count: usize,
    bytes: u64,
    /// Preview paths for the first few photos, enough to recognise the day.
    covers: Vec<String>,
}

#[tauri::command]
fn days(state: State<ScanState>) -> Vec<Day> {
    let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let mut by_day: HashMap<String, (usize, u64, Vec<String>)> = HashMap::new();
    for record in &data.records {
        let entry = by_day.entry(day_key(record.photo.taken)).or_default();
        entry.0 += 1;
        entry.1 += record.photo.size;
        if entry.2.len() < 4 {
            entry.2.push(
                record
                    .photo
                    .thumb
                    .clone()
                    .unwrap_or_else(|| record.photo.preview.clone()),
            );
        }
    }
    let mut out: Vec<Day> = by_day
        .into_iter()
        .map(|(key, (count, bytes, covers))| Day {
            key,
            count,
            bytes,
            covers,
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Same day-by-day shape as `days()`, but built from the Destination library instead
/// of the current Source scan — the Gallery tab's "also show what's already in
/// Photos, for the days that matter" view. Deliberately scoped to `only_dates` (the
/// Source scan's own day keys) rather than the whole library: a real library can span
/// years, and nobody asking "what do I already have from this trip" wants every
/// unrelated day mixed in — this is an explicit choice the user makes per scan, not
/// something that runs unasked. Trashed and hidden assets are left out, matching what
/// Photos' own main grid shows. Covers come from `fusion::find_thumbnail`'s
/// locally-cached derivatives, so this never triggers an iCloud download just to
/// render a thumbnail, and thumbnail work only happens for days that survive the
/// `only_dates` filter, not the whole library.
#[cfg(target_os = "macos")]
#[tauri::command]
fn photos_days(library_path: String, only_dates: Vec<String>) -> Result<Vec<Day>, String> {
    let lib = Path::new(&library_path);
    let index = fusion::read_photos_index(lib)?;
    let wanted: HashSet<String> = only_dates.into_iter().collect();

    let mut by_day: HashMap<String, (usize, u64, Vec<&fusion::PhotosAsset>)> = HashMap::new();
    for asset in &index {
        if asset.trashed || asset.hidden {
            continue;
        }
        let Some(taken) = asset.taken else {
            continue;
        };
        let key = day_key(taken);
        if !wanted.contains(&key) {
            continue;
        }
        let entry = by_day.entry(key).or_default();
        entry.0 += 1;
        entry.1 += asset.size.unwrap_or(0);
        entry.2.push(asset);
    }

    let mut out: Vec<Day> = by_day
        .into_iter()
        .map(|(key, (count, bytes, assets))| {
            let mut covers = Vec::new();
            for asset in &assets {
                if covers.len() >= 4 {
                    break;
                }
                if let Some(thumb) = fusion::find_thumbnail(lib, &asset.uuid) {
                    covers.push(thumb.to_string_lossy().into_owned());
                }
            }
            Day {
                key,
                count,
                bytes,
                covers,
            }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Every non-trashed, non-hidden Destination asset taken on exactly `date`, each
/// resolved to its own locally-cached thumbnail — unlike `photos_days`, which caps
/// every day at 4 covers so the whole Gallery stays cheap to load, this runs for one
/// specific day, on demand, only when the viewer actually opens it. Shaped as `Photo`
/// so the existing viewer can show these next to Source photos without a separate
/// code path; `blur`/`kind` are always `None` since neither is computed for a
/// Photos-library asset, and `preview` points at the cached derivative, not the
/// original — this never triggers an iCloud download just to look at a day.
#[cfg(target_os = "macos")]
#[tauri::command]
fn photos_day_detail(library_path: String, date: String) -> Result<Vec<Photo>, String> {
    let lib = Path::new(&library_path);
    let index = fusion::read_photos_index(lib)?;

    Ok(index
        .into_iter()
        .filter(|a| !a.trashed && !a.hidden)
        .filter_map(|a| {
            let taken = a.taken?;
            if day_key(taken) != date {
                return None;
            }
            let thumb = fusion::find_thumbnail(lib, &a.uuid)?;
            let preview = thumb.to_string_lossy().into_owned();
            // Photos renames every imported file to its own UUID on import, so
            // `a.filename` reads as noise (`8136520B-...heic`); the camera model,
            // read straight from the cached derivative's own EXIF, is what a person
            // actually recognises a shot by. Falls back to the UUID name on whatever
            // has none — a screenshot, an edited export, anything not camera-shot.
            // Photos keeps the original extension on the UUID name it renames to, so
            // the real format survives even though the filename itself is noise.
            let format = Path::new(&a.filename)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_uppercase())
                .unwrap_or_default();
            let device = camera_model(&thumb);
            let name = device.clone().unwrap_or(a.filename);
            // Header only, no decode: `imagesize` reads the dimensions and stops.
            let preview_dims = imagesize::size(&thumb)
                .ok()
                .map(|d| [d.width as u32, d.height as u32]);
            Some(Photo {
                path: preview.clone(),
                name,
                size: a.size.unwrap_or(0),
                width: a.width,
                height: a.height,
                taken,
                blur: None,
                measurements: badshot::Measurements::default(),
                bad_shot: badshot::BadShot::default(),
                preview,
                // Photos' own cached derivative is already thumbnail-sized — no
                // second, smaller rendition to generate here.
                thumb: None,
                library: true,
                missing: false,
                // Straight from `Photos.sqlite`: the cached derivative this points at
                // carries no EXIF of its own, but the library's database has the
                // position for every asset it knows about.
                lat: a.lat,
                lon: a.lon,
                preview_dims,
                format,
                device,
                kind: None,
            })
        })
        .collect())
}

/// The small cached rendition for a Destination-folder cover, mirroring the source
/// scan's own `thumb` field so both sides of the Gallery render at the same size.
/// `None` when there is no cache directory or the file could not be decoded — the
/// caller falls back to the original path, same as the source scan does.
#[cfg(not(target_os = "macos"))]
fn destination_thumb(path: &Path, cache: Option<&Path>) -> Option<String> {
    let dir = cache?;
    let bytes = analyze_file(path).thumb_jpeg?;
    let file = dir.join(preview_name(path, "thumb"));
    if !file.exists() {
        let _ = std::fs::write(&file, bytes);
    }
    Some(file.to_string_lossy().into_owned())
}

/// Windows and Linux have no opaque photo library to read the way macOS Photos does —
/// the Photos app on Windows, and every photo manager on Linux, just index whatever
/// folders they are pointed at, the same folders the Source scan itself can already
/// see. So the Destination here is a plain folder (typically the Pictures directory,
/// see `default_photos_library_path`), walked the same way `run_scan` walks a Source
/// folder, but scoped to `only_dates` and without any hashing or clustering — this is
/// a look, not an analysis. Dataless (cloud-only OneDrive) files are skipped rather
/// than triggering a download just to read a date.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn photos_days(
    app: AppHandle,
    library_path: String,
    only_dates: Vec<String>,
) -> Result<Vec<Day>, String> {
    photos_days_in(&library_path, only_dates, preview_cache_dir(&app).as_deref())
}

/// The actual folder walk, taking the cache directory as a plain path rather than an
/// `AppHandle` — `AppHandle` is a concrete type tied to the real Wry runtime, which a
/// mock `AppHandle<MockRuntime>` in a test can't stand in for, so keeping the app
/// handle out of this function entirely is what makes it testable at all.
#[cfg(not(target_os = "macos"))]
fn photos_days_in(
    library_path: &str,
    only_dates: Vec<String>,
    cache: Option<&Path>,
) -> Result<Vec<Day>, String> {
    let root = Path::new(library_path);
    if !root.is_dir() {
        return Err("not a directory".into());
    }
    let wanted: HashSet<String> = only_dates.into_iter().collect();

    let mut by_day: HashMap<String, (usize, u64, Vec<PathBuf>)> = HashMap::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && is_opaque_package(e.path())))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| is_image(e.path()))
    {
        let path = entry.into_path();
        if is_dataless(&path) {
            continue;
        }
        let Some((mtime, size)) = file_stamp(&path) else {
            continue;
        };
        let key = day_key(taken_date(&path, mtime));
        if !wanted.contains(&key) {
            continue;
        }
        let day = by_day.entry(key).or_default();
        day.0 += 1;
        day.1 += size;
        if day.2.len() < 4 {
            day.2.push(path);
        }
    }

    let mut out: Vec<Day> = by_day
        .into_iter()
        .map(|(key, (count, bytes, paths))| {
            let covers = paths
                .into_iter()
                .map(|p| {
                    destination_thumb(&p, cache)
                        .unwrap_or_else(|| p.to_string_lossy().into_owned())
                })
                .collect();
            Day {
                key,
                count,
                bytes,
                covers,
            }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// The folder-backed counterpart to the macOS `photos_day_detail` above: every image
/// in the Destination folder taken on exactly `date`, each given the same
/// preview/thumb treatment `run_scan` gives a Source photo, so the two render
/// identically once merged in the Gallery grid.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn photos_day_detail(
    app: AppHandle,
    library_path: String,
    date: String,
) -> Result<Vec<Photo>, String> {
    photos_day_detail_in(&library_path, &date, preview_cache_dir(&app).as_deref())
}

/// See `photos_days_in`'s doc comment for why the cache directory is a plain path
/// here rather than an `AppHandle`.
#[cfg(not(target_os = "macos"))]
fn photos_day_detail_in(
    library_path: &str,
    date: &str,
    cache: Option<&Path>,
) -> Result<Vec<Photo>, String> {
    let root = Path::new(library_path);
    if !root.is_dir() {
        return Err("not a directory".into());
    }

    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.file_type().is_dir() && is_opaque_package(e.path())))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| is_image(e.path()))
    {
        let path = entry.into_path();
        if is_dataless(&path) {
            continue;
        }
        let Some((mtime, _)) = file_stamp(&path) else {
            continue;
        };
        if day_key(taken_date(&path, mtime)) != date {
            continue;
        }
        let analysis = analyze_file(&path);
        let preview = match (&analysis.preview_jpeg, cache) {
            (Some(bytes), Some(dir)) => {
                let file = dir.join(preview_name(&path, "grid"));
                if !file.exists() {
                    let _ = std::fs::write(&file, bytes);
                }
                file.to_string_lossy().into_owned()
            }
            _ => path.to_string_lossy().into_owned(),
        };
        let thumb = match (&analysis.thumb_jpeg, cache) {
            (Some(bytes), Some(dir)) => {
                let file = dir.join(preview_name(&path, "thumb"));
                if !file.exists() {
                    let _ = std::fs::write(&file, bytes);
                }
                Some(file.to_string_lossy().into_owned())
            }
            _ => None,
        };
        let mut photo = photo_meta(&path, &analysis, preview);
        photo.thumb = thumb;
        out.push(photo);
    }
    Ok(out)
}

/// Move `paths` into a fresh batch folder under `trash_root`, writing a manifest
/// so the move can be undone. All-or-nothing: a failure rolls the batch back.
fn trash_to(trash_root: &Path, paths: &[(String, Option<String>)]) -> Result<String, String> {
    let batch_id = chrono::Utc::now().timestamp_millis().to_string();
    let dir = trash_root.join(&batch_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut manifest: Vec<ManifestEntry> = Vec::new();
    for (i, (p, preview)) in paths.iter().enumerate() {
        let src = Path::new(p);
        let stored = format!(
            "{}_{}",
            i,
            src.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        );
        let dst = dir.join(&stored);
        if let Err(e) = move_file(src, &dst) {
            for entry in &manifest {
                let _ = move_file(&dir.join(&entry.stored), Path::new(&entry.original));
            }
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("{}: {}", p, e));
        }
        manifest.push(ManifestEntry {
            original: p.clone(),
            stored,
            preview: preview.clone(),
        });
    }
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(batch_id)
}

/// Put every file of `batch_id` back where it came from and drop the batch folder.
fn undo_from(trash_root: &Path, batch_id: &str) -> Result<usize, String> {
    if batch_id.contains('/') || batch_id.contains('\\') || batch_id.contains("..") {
        return Err("invalid batch id".into());
    }
    let dir = trash_root.join(batch_id);
    let manifest: Vec<ManifestEntry> = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    for entry in &manifest {
        let original = Path::new(&entry.original);
        if let Some(parent) = original.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        move_file(&dir.join(&entry.stored), original).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(manifest.len())
}

/// Read every batch currently in the trash, newest first.
fn read_trash(trash_root: &Path) -> Vec<TrashBatch> {
    let Ok(entries) = std::fs::read_dir(trash_root) else {
        return Vec::new();
    };
    let mut batches: Vec<TrashBatch> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|entry| {
            let dir = entry.path();
            let batch_id = dir.file_name()?.to_string_lossy().into_owned();
            let manifest: Vec<ManifestEntry> =
                serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
            let photos: Vec<TrashedPhoto> = manifest
                .into_iter()
                .map(|m| {
                    let stored = dir.join(&m.stored);
                    let size = stored.metadata().map(|md| md.len()).unwrap_or(0);
                    let stored_path = stored.to_string_lossy().into_owned();
                    TrashedPhoto {
                        preview: m.preview.clone().unwrap_or_else(|| stored_path.clone()),
                        stored_path,
                        name: Path::new(&m.original)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        original: m.original,
                        size,
                    }
                })
                .collect();
            let bytes = photos.iter().map(|p| p.size).sum();
            Some(TrashBatch {
                when: batch_id.parse().unwrap_or(0),
                batch_id,
                photos,
                bytes,
            })
        })
        .collect();
    batches.sort_by_key(|b| std::cmp::Reverse(b.when));
    batches
}

fn trash_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("trash"))
}

/// Brings the Photos library's own photographs into the scan, so a loose copy on disk
/// and the copy already filed in Photos land in the same duplicate group.
///
/// Scoped by `only_dates` to the days the scan already covers, for the same reason
/// `photos_days` is: nobody asking about one folder has asked to have their whole
/// library pulled in.
///
/// What these records deliberately are not:
///
/// - **Not sharpness-scored.** The only copy on this Mac is a resampled derivative —
///   an "Optimize Mac Storage" library keeps almost no originals locally — and its
///   detail density is not comparable with a full-size frame's. The blur threshold is
///   a percentile of the scan's own scores, so mixing the two populations would
///   misjudge both. `blur` stays `None`, which every blur reader already skips.
/// - **Not SHA'd.** Byte equality is a claim about files, and the derivative is not
///   the asset. The perceptual fingerprint is the honest comparison here, and it is
///   unbothered by the rescale: it reduces any frame to a small grid regardless.
/// - **Not deletable.** `Photo::library` carries that downstream; `better` also ranks
///   these first so the surplus copy proposed is always the one on disk.
///
/// Merges only; building the view is `regroup`'s job, which the caller runs next. Two
/// passes would mean running the model over every proposed group twice.
///
/// Read-only against the library, so no licence gate — the gate is on the trash.
#[cfg(target_os = "macos")]
#[tauri::command]
fn include_photos_in_scan(
    app: AppHandle,
    state: State<ScanState>,
    library_path: String,
    only_dates: Vec<String>,
) -> Result<usize, String> {
    let lib = Path::new(&library_path);
    let cache_file = scan_cache_path(&app);
    let cached = load_scan_cache(cache_file.as_ref());
    let wanted: HashSet<String> = only_dates.into_iter().collect();
    let candidates: Vec<fusion::PhotosAsset> = fusion::read_photos_index(lib)?
        .into_iter()
        .filter(|a| !a.trashed && !a.hidden)
        .filter(|a| a.taken.is_some_and(|t| wanted.contains(&day_key(t))))
        .collect();

    // Decoding each derivative is the whole cost here, and every one is independent.
    let records: Vec<Record> = candidates
        .into_par_iter()
        .filter_map(|asset| {
            let taken = asset.taken?;
            let thumb = fusion::find_thumbnail(lib, &asset.uuid)?;
            let preview = thumb.to_string_lossy().into_owned();

            /* Decoding and hashing is the entire cost of this command — measured at
               about 60 ms per asset, against under 2 ms for everything else put
               together. A derivative that has not changed gives the same answer it
               gave last time, so the second time this is asked it costs nothing. The
               `library` check keeps a folder photo that happens to share a path — an
               exported copy sitting where a derivative used to be — from being read
               back as a library record. */
            if let (Some(hit), Some(stamp)) = (cached.files.get(&preview), file_stamp(&thumb)) {
                if hit.mtime == stamp.0 && hit.size == stamp.1 && hit.record.photo.library {
                    return Some(hit.record.clone());
                }
            }

            let rgb = image::open(&thumb).ok()?.to_rgb8();
            let format = Path::new(&asset.filename)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_uppercase())
                .unwrap_or_default();
            let device = camera_model(&thumb);
            let name = device.clone().unwrap_or(asset.filename);
            // Header only, no decode: `imagesize` reads the dimensions and stops.
            let preview_dims = imagesize::size(&thumb)
                .ok()
                .map(|d| [d.width as u32, d.height as u32]);
            Some(Record {
                photo: Photo {
                    path: preview.clone(),
                    name,
                    // The asset's own figures, not the derivative's: they describe the
                    // photograph Photos holds, which is what the group is about.
                    size: asset.size.unwrap_or(0),
                    width: asset.width,
                    height: asset.height,
                    taken,
                    blur: None,
                    measurements: badshot::Measurements::default(),
                    bad_shot: badshot::BadShot::default(),
                    preview,
                    thumb: None,
                    library: true,
                    missing: false,
                    lat: asset.lat,
                    lon: asset.lon,
                    preview_dims,
                    format,
                    device,
                    kind: None,
                },
                sha: None,
                phash: fingerprint(&rgb),
            })
        })
        .collect();

    save_to_scan_cache(cache_file.as_ref(), &records);

    let merged = records.len();
    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    // Idempotent: asking twice must refresh the library's contribution, not double it.
    data.records.retain(|r| !r.photo.library);
    data.records.extend(records);
    Ok(merged)
}

/// Windows and Linux have no library bundle to pull from — what `photos_days` shows
/// there is the Pictures folder, whose files are ordinary files the user can already
/// add to a scan as a folder. Nothing to merge, so the view is handed back untouched.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn include_photos_in_scan(
    app: AppHandle,
    state: State<ScanState>,
    library_path: String,
    only_dates: Vec<String>,
) -> Result<usize, String> {
    let _ = (app, state, library_path, only_dates);
    Ok(0)
}

/// The library at the standard macOS location, when there is one — a starting point
/// for the picker, not a guarantee (a renamed library, or several, are both normal).
#[cfg(target_os = "macos")]
#[tauri::command]
fn default_photos_library_path(app: AppHandle) -> Option<String> {
    let path = app
        .path()
        .picture_dir()
        .ok()?
        .join("Photos Library.photoslibrary");
    path.exists().then(|| path.to_string_lossy().into_owned())
}

/// Windows and Linux have no library bundle to detect — the closest equivalent is
/// simply the Pictures folder itself, the same default the Windows Photos app and
/// most Linux photo tools already point at.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn default_photos_library_path(app: AppHandle) -> Option<String> {
    let path = app.path().picture_dir().ok()?;
    path.exists().then(|| path.to_string_lossy().into_owned())
}

/// Hands a set of files to another application — sending the photos that survived a
/// clean-up on to an editor (Lightroom, Capture One, Affinity…) without leaving Skimrr.
///
/// Deliberately launches the chosen app on the originals rather than copying them
/// anywhere: every serious photo editor imports by reference, and staging a second set
/// on disk would recreate exactly the duplication this app exists to remove.
#[cfg(target_os = "macos")]
#[tauri::command]
fn share_to_app(app_path: String, paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // `open -a` is the only way to target a `.app`: a bundle is a directory, not an
    // executable, so it cannot be spawned. `open` returns as soon as the app has been
    // handed the files, so waiting here does not block on the editor's own startup.
    let status = std::process::Command::new("open")
        .arg("-a")
        .arg(&app_path)
        .args(&paths)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("open_failed".into());
    }
    Ok(())
}

/// Windows and Linux have no `open -a` equivalent aimed at one chosen application, but
/// there the picked file IS the executable, so the paths go straight on its command
/// line. Spawned without waiting, unlike macOS: here the child process is the editor
/// itself, and waiting on it would hang until the user quits it.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn share_to_app(app_path: String, paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    std::process::Command::new(&app_path)
        .args(&paths)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn trash_photos(
    app: AppHandle,
    state: State<ScanState>,
    licence: State<LicenceState>,
    paths: Vec<String>,
) -> Result<TrashResult, String> {
    // Scanning and reviewing are free; moving files is what a licence buys.
    if !license::is_active(&licence) {
        return Err("licence_required".into());
    }
    /* A library photo's `path` points inside the Photos bundle, at the cached
       derivative the app read it from. Moving that would not delete anything from
       Photos — it would amputate the library's own cache. The UI already keeps these
       out of any selection; this refuses them outright, because the cost of a bug
       getting through is damage to data this app never owned. */
    {
        let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
        let wanted: HashSet<&String> = paths.iter().collect();
        if data
            .records
            .iter()
            .any(|r| r.photo.library && wanted.contains(&r.photo.path))
        {
            return Err("library_photo".into());
        }
    }
    // Pair every path with its cached rendition so the trash can still show raw files.
    let with_previews: Vec<(String, Option<String>)> = {
        let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
        paths
            .iter()
            .map(|p| {
                let preview = data
                    .records
                    .iter()
                    .find(|r| &r.photo.path == p)
                    .filter(|r| r.photo.preview != r.photo.path)
                    .map(|r| r.photo.preview.clone());
                (p.clone(), preview)
            })
            .collect()
    };
    let batch_id = trash_to(&trash_root(&app)?, &with_previews)?;

    let set: HashSet<&String> = paths.iter().collect();
    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let records = std::mem::take(&mut data.records);
    let (moved, kept): (Vec<Record>, Vec<Record>) = records
        .into_iter()
        .partition(|r| set.contains(&r.photo.path));
    data.records = kept;
    data.trashed.insert(batch_id.clone(), moved);

    Ok(TrashResult {
        batch_id,
        count: paths.len(),
    })
}

#[tauri::command]
fn undo_trash(app: AppHandle, state: State<ScanState>, batch_id: String) -> Result<usize, String> {
    let count = undo_from(&trash_root(&app)?, &batch_id)?;

    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(records) = data.trashed.remove(&batch_id) {
        data.records.extend(records);
    }
    Ok(count)
}

/// Full-resolution rendition for the viewer, built on first request rather than during
/// the scan: a whole library's worth of these would cost far more than the handful a
/// user actually opens.
///
/// For camera raw the ceiling is the rendition the file embeds. Reaching real sensor
/// resolution would mean demosaicing, which Skimrr deliberately does not do.
/// The library asset's own UUID, read back out of the path of one of its derivatives.
///
/// Photos names them `<uuid>_<numbers>_c.jpeg`, so a photo's identity travels with the
/// only path this app ever holds for it — no UUID has to be carried on every `Photo`.
/// Validated rather than trusted: the result is fed to PhotoKit and, before that, into
/// an AppleScript string.
#[cfg(target_os = "macos")]
fn library_uuid(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('_').next())
        .filter(|u| u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .map(str::to_owned)
}

/// Removes assets from the Photos library itself, which is the only way a duplicate
/// that lives there can actually be resolved.
///
/// Not Skimrr's own trash, and the difference matters: this app does not move the file,
/// it asks Photos to delete it. The asset lands in Photos' "Recently Deleted" and stays
/// recoverable for thirty days — verified on a disposable photo imported for the
/// purpose, whose row came back with `ZTRASHEDSTATE = 1` rather than disappearing. But
/// the undo lives in Photos, not here, and `undo_trash` will never bring it back.
///
/// Everything this needs was verified against the real system rather than assumed: the
/// library's own UUID is accepted unchanged as a PhotoKit local identifier, and access
/// is granted once the app declares `NSPhotoLibraryUsageDescription` — without that key
/// macOS denies instantly instead of asking.
///
/// Licence-gated exactly like `trash_photos`: reviewing is free, destroying is not.
#[cfg(target_os = "macos")]
#[tauri::command]
fn delete_from_photos(
    state: State<ScanState>,
    licence: State<LicenceState>,
    paths: Vec<String>,
) -> Result<usize, String> {
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::{NSArray, NSString};
    use objc2_photos::{PHAsset, PHAssetChangeRequest, PHPhotoLibrary};

    if !license::is_active(&licence) {
        return Err("licence_required".into());
    }

    // Only paths the scan actually holds as library assets: a caller must not be able
    // to name an arbitrary UUID and have it deleted.
    let uuids: Vec<String> = {
        let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
        let wanted: HashSet<&String> = paths.iter().collect();
        data.records
            .iter()
            .filter(|r| r.photo.library && wanted.contains(&r.photo.path))
            .filter_map(|r| library_uuid(Path::new(&r.photo.path)))
            .collect()
    };
    if uuids.is_empty() {
        return Ok(0);
    }

    let strings: Vec<objc2::rc::Retained<NSString>> =
        uuids.iter().map(|u| NSString::from_str(u)).collect();
    let refs: Vec<&NSString> = strings.iter().map(|s| &**s).collect();
    let identifiers = NSArray::from_slice(&refs);

    let assets = unsafe { PHAsset::fetchAssetsWithLocalIdentifiers_options(&identifiers, None) };
    let found = unsafe { assets.count() };
    if found == 0 {
        return Err("no_such_assets".into());
    }

    let library = unsafe { PHPhotoLibrary::sharedPhotoLibrary() };
    let block = block2::RcBlock::new(move || {
        let enumerable = ProtocolObject::from_ref(&*assets);
        unsafe { PHAssetChangeRequest::deleteAssets(enumerable) };
    });
    // `dispatch_block_t` is a raw pointer on this side of the bridge. The block is kept
    // alive by `block` for the whole call, and the call is synchronous, so nothing can
    // outlive it.
    let change: *mut block2::Block<dyn Fn()> = &*block as *const _ as *mut _;
    unsafe { library.performChangesAndWait_error(change) }
        .map_err(|e| e.localizedDescription().to_string())?;

    // Gone from the library means gone from the view; the next merge would drop them
    // anyway, since `read_photos_index` already skips trashed assets.
    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let removed: HashSet<&String> = paths.iter().collect();
    data.records
        .retain(|r| !(r.photo.library && removed.contains(&r.photo.path)));

    Ok(found)
}

/// Asks Photos for the actual original behind a library asset, and returns a path to it.
///
/// With iCloud storage optimised, the only copy on this Mac is a derivative — measured
/// at 480x360 for a frame the database reports as 4624x3468. Everything the app states
/// about such a photo describes the original, so showing the stand-in full-screen makes
/// the better copy look like the worse one, which is exactly backwards when the two are
/// side by side in a duplicate group.
///
/// Verified against the real scripting dictionary, not assumed: Photos exposes
/// `export <media items> to <folder> [with using originals]`, and `media item id`
/// accepts the library's own asset UUID unchanged. Measured on an 11 MB frame held only
/// in iCloud: 2.6 s, and the bytes exported match what the database reported exactly.
///
/// On demand only, never during a scan: it downloads from iCloud, it needs permission
/// to control Photos, and it takes seconds. Exported once, then reused from the cache.
#[cfg(target_os = "macos")]
#[tauri::command]
async fn library_original(app: AppHandle, path: String, thumb: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let derivative = PathBuf::from(&path);
        // Derivatives are named `<uuid>_<numbers>_c.jpeg`, so the asset's identity is
        // already in the path — no need to carry a UUID around on every Photo.
        let uuid = derivative
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('_').next())
            .filter(|u| {
                u.len() == 36 && u.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            })
            .ok_or("not a library derivative")?
            .to_string();

        let dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("originals")
            .join(&uuid);
        // Photos names the export after the asset's own original filename, which this
        // side does not know in advance — so the per-UUID folder is read back rather
        // than a filename being predicted.
        let first_file = |dir: &Path| -> Option<String> {
            std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
                let p = e.path();
                p.is_file().then(|| p.to_string_lossy().into_owned())
            })
        };
        /* A grid cell wants a grid-sized picture. Pointing an `<img>` at the eleven
           megabytes of an original would have the webview decode a sixteen megapixel
           frame into a thumbnail slot, once per card on screen — so the reduced
           rendition is built from the original and cached beside it. */
        let reduced = dir.join("thumb.jpg");
        let wanted = |dir: &Path| -> Option<String> {
            if thumb {
                return reduced.exists().then(|| reduced.to_string_lossy().into_owned());
            }
            first_file(dir).filter(|p| Path::new(p) != reduced)
        };
        if let Some(existing) = wanted(&dir) {
            return Ok(existing);
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let script = format!(
            "tell application \"Photos\"\n    export {{media item id \"{uuid}\"}} to POSIX file \"{}\" with using originals\nend tell",
            dir.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
        );
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            let _ = std::fs::remove_dir(&dir);
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        let original = first_file(&dir)
            .filter(|p| Path::new(p) != reduced)
            .ok_or_else(|| "Photos exported nothing".to_string())?;
        if !thumb {
            return Ok(original);
        }
        let decoded = image::open(&original).map_err(|e| e.to_string())?;
        let bytes = encode_thumb(&decoded).ok_or("could not build a thumbnail")?;
        std::fs::write(&reduced, bytes).map_err(|e| e.to_string())?;
        Ok(reduced.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn detail_preview(app: AppHandle, path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = PathBuf::from(&path);
        if !is_raw(&src) && !is_heif(&src) {
            // The webview reads these straight from disk, at full resolution already.
            return Ok(path);
        }
        let Some(dir) = preview_cache_dir(&app) else {
            return Ok(path);
        };
        let file = dir.join(preview_name(&src, "detail"));
        if file.exists() {
            return Ok(file.to_string_lossy().into_owned());
        }

        let img = if is_raw(&src) {
            let data = std::fs::read(&src).map_err(|e| e.to_string())?;
            let meta = read_raw_meta(&data);
            let jpeg = largest_embedded_jpeg(&data).ok_or("no embedded rendition")?;
            let decoded = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?;
            apply_orientation(decoded, meta.orientation.unwrap_or(1))
        } else {
            let decoded = decode_heic(&src).ok_or("could not decode")?;
            apply_orientation(decoded, exif_orientation(&src))
        };

        let mut out = std::io::Cursor::new(Vec::new());
        img.to_rgb8()
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;
        std::fs::write(&file, out.into_inner()).map_err(|e| e.to_string())?;
        Ok(file.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_trash(app: AppHandle) -> Result<Vec<TrashBatch>, String> {
    Ok(read_trash(&trash_root(&app)?))
}

/// Delete the trash for good. This is the only place Skimrr removes photo data,
/// and it is always an explicit, confirmed user action.
#[tauri::command]
fn empty_trash(app: AppHandle, state: State<ScanState>) -> Result<usize, String> {
    let root = trash_root(&app)?;
    let batches = read_trash(&root);
    let count = batches.iter().map(|b| b.photos.len()).sum();
    for batch in batches {
        std::fs::remove_dir_all(root.join(&batch.batch_id)).map_err(|e| e.to_string())?;
    }
    state.0.lock().unwrap_or_else(|e| e.into_inner()).trashed.clear();
    Ok(count)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(ScanState(Mutex::new(ScanData::default())))
        .manage(Cancel::default())
        .manage(LicenceState::new())
        .manage(portable::PendingOpen::default())
        .setup(|app| {
            // A `.skimrr` double-clicked while Skimrr was not running arrives here, as an
            // argument. It is put aside rather than opened: the interface has not been
            // built yet, and an encrypted project needs a password anyway.
            if let Some(path) = portable::from_argv() {
                *app.state::<portable::PendingOpen>().0.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
            }
            // Silent, once per launch, and only when the receipt is old enough.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<LicenceState>();
                license::revalidate_if_due(&state).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan_folder,
            cancel_scan,
            offline_set,
            download_offline,
            regroup,
            days,
            photos_days,
            photos_day_detail,
            default_photos_library_path,
            include_photos_in_scan,
            share_to_app,
            trash_photos,
            #[cfg(target_os = "macos")]
            delete_from_photos,
            undo_trash,
            list_trash,
            empty_trash,
            detail_preview,
            #[cfg(target_os = "macos")]
            library_original,
            cache_usage,
            clear_cache,
            app_version,
            license::licence_status,
            license::activate_licence,
            license::deactivate_licence,
            portable::export_estimate,
            portable::export_project,
            portable::peek_project,
            portable::import_project,
            portable::take_pending_project
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        // Built and run in two steps rather than one so the run loop can be watched:
        // macOS delivers a double-clicked file to an application that is *already*
        // running through this event, and nothing else would ever go looking for it.
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &_event {
                portable::opened_urls(_app, urls);
            }
        });
}

/// What the two caches weigh, so the settings panel can say it rather than guess.
#[derive(Debug, Clone, Copy, Default, Serialize)]
struct CacheUsage {
    previews_bytes: u64,
    previews_files: u64,
    scans_bytes: u64,
    scans_files: u64,
}

/// Size and count of one directory, one level deep, which is how both caches are laid
/// out. Unreadable entries are skipped rather than failing the whole reading: a figure
/// that is slightly low is more useful than an error.
fn dir_usage(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0;
    let mut files = 0;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

fn cache_dirs(app: &AppHandle) -> Option<(PathBuf, PathBuf)> {
    let base = app.path().app_cache_dir().ok()?;
    Some((base.join("previews"), base.join("scans")))
}

#[tauri::command]
fn cache_usage(app: AppHandle) -> CacheUsage {
    let Some((previews, scans)) = cache_dirs(&app) else {
        return CacheUsage::default();
    };
    let (previews_bytes, previews_files) = dir_usage(&previews);
    let (scans_bytes, scans_files) = dir_usage(&scans);
    CacheUsage {
        previews_bytes,
        previews_files,
        scans_bytes,
        scans_files,
    }
}

/// Empties both caches and answers with what was freed.
///
/// Nothing here is user data: renditions are rebuilt on the next scan and the scan
/// cache only spares re-analysis. The local trash lives elsewhere and is never touched,
/// because that one does hold photographs waiting to be restored.
#[tauri::command]
fn clear_cache(app: AppHandle) -> Result<CacheUsage, String> {
    let freed = cache_usage(app.clone());
    let Some((previews, scans)) = cache_dirs(&app) else {
        return Err("no_cache_dir".into());
    };
    for dir in [previews, scans] {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
                    std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    Ok(freed)
}

/// The version the user is actually running, for a bug report that means something.
#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[cfg(test)]
mod cache_tests {
    use super::dir_usage;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skimrr-test-{tag}-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dir_usage_counts_files_and_ignores_subdirectories() {
        let dir = temp_dir("usage");
        std::fs::write(dir.join("a.jpg"), vec![0u8; 300]).unwrap();
        std::fs::write(dir.join("b.jpg"), vec![0u8; 700]).unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/c.jpg"), vec![0u8; 5000]).unwrap();

        let (bytes, files) = dir_usage(&dir);
        assert_eq!(files, 2, "only the files at this level are counted");
        assert_eq!(
            bytes, 1000,
            "a nested directory must not inflate the figure"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dir_usage_is_zero_for_a_directory_that_does_not_exist() {
        let dir = temp_dir("absent");
        assert_eq!(dir_usage(&dir.join("nope")), (0, 0));
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Fixtures in the test modules always carry structure, so a refusal is a failure.
#[cfg(test)]
fn to_rgb(img: &GrayImage) -> image::RgbImage {
    image::RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let v = img.get_pixel(x, y)[0];
        image::Rgb([v, v, v])
    })
}

/// The same, hashed. Only the tests call it; it lived outside `cfg(test)` and was
/// therefore compiled into the shipped binary while reading as dead code.
#[cfg(test)]
fn hash_of(img: &GrayImage) -> u128 {
    fingerprint(&to_rgb(img)).expect("fixture has enough structure to hash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    /// Same minimal `Photos.sqlite` shape `fusion::tests` builds, plus one real
    /// thumbnail JPEG on disk, so `photos_day_detail` can be exercised end to end
    /// without a real Photos library. `taken` is a Unix timestamp; ground truth for
    /// which day it falls on comes from `day_key` itself, not a hand-computed date,
    /// so the test can't drift from whatever `day_key`'s own timezone handling does.
    #[cfg(target_os = "macos")]
    fn synthetic_library_with_thumbnail(dir: &Path, uuid: &str, taken: i64) {
        let db_dir = dir.join("database");
        std::fs::create_dir_all(&db_dir).unwrap();
        let conn = rusqlite::Connection::open(db_dir.join("Photos.sqlite")).unwrap();
        let mac_seconds = (taken - 978_307_200) as f64;
        conn.execute_batch(&format!(
            "CREATE TABLE ZASSET (
                Z_PK INTEGER, ZADDITIONALATTRIBUTES INTEGER,
                ZUUID TEXT, ZFILENAME TEXT, ZDATECREATED REAL,
                ZWIDTH INTEGER, ZHEIGHT INTEGER, ZDURATION REAL,
                ZTRASHEDSTATE INTEGER, ZHIDDEN INTEGER,
                ZLATITUDE REAL, ZLONGITUDE REAL
            );
            CREATE TABLE ZADDITIONALASSETATTRIBUTES (
                Z_PK INTEGER, ZORIGINALFILESIZE INTEGER
            );
            INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (1, 123456);
            INSERT INTO ZASSET VALUES
                (1, 1, '{uuid}', 'IMG_0001.HEIC', {mac_seconds}, 100, 200, 0.0, 0, 0, -180.0, -180.0);"
        ))
        .unwrap();

        let hex = uuid.chars().next().unwrap().to_ascii_uppercase().to_string();
        let thumb_dir = dir.join("resources/derivatives").join(&hex);
        std::fs::create_dir_all(&thumb_dir).unwrap();
        image::RgbImage::new(4, 4)
            .save_with_format(
                thumb_dir.join(format!("{uuid}_1_1_c.jpeg")),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn photos_day_detail_finds_only_the_matching_day() {
        let dir = std::env::temp_dir().join(format!("skimrr-day-detail-test-{}", std::process::id()));
        let uuid = "0283FD35-7126-4035-ADC4-DC6BA3A8505C";
        let taken = 1_718_452_800; // an arbitrary, fixed instant
        let day = day_key(taken);
        synthetic_library_with_thumbnail(&dir, uuid, taken);

        let found = photos_day_detail(dir.to_string_lossy().into_owned(), day.clone()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "IMG_0001.HEIC");
        assert_eq!(found[0].size, 123456);
        assert!(found[0].blur.is_none());
        assert!(!found[0].preview.is_empty());

        let other_day = photos_day_detail(dir.to_string_lossy().into_owned(), "1999-01-01".into()).unwrap();
        assert!(other_day.is_empty(), "a different day must return nothing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Windows/Linux counterpart of the macOS test above: a plain folder instead
    /// of a Photos.sqlite library, exercising the `_in` functions directly (no
    /// `AppHandle` needed — see their doc comments) with a real cache directory so
    /// the thumbnail-writing path runs too, not just the day-matching. `taken` comes
    /// from the file's own mtime, since a freshly-written synthetic JPEG carries no
    /// EXIF date — ground truth is `day_key(taken_date(...))` itself, the same
    /// function `photos_days_in`/`photos_day_detail_in` use internally, so the test
    /// can't drift from it.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn photos_days_and_detail_find_only_the_matching_day() {
        let dir = std::env::temp_dir().join(format!("skimrr-dest-folder-test-{}", std::process::id()));
        let cache_dir = std::env::temp_dir().join(format!("skimrr-dest-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let path = dir.join("photo.jpg");
        image::RgbImage::new(4, 4)
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();

        let (mtime, _) = file_stamp(&path).unwrap();
        let day = day_key(taken_date(&path, mtime));
        let library_path = dir.to_string_lossy().into_owned();

        let days = photos_days_in(&library_path, vec![day.clone()], Some(&cache_dir)).unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].key, day);
        assert_eq!(days[0].count, 1);
        assert_eq!(days[0].covers.len(), 1);

        let other = photos_days_in(&library_path, vec!["1999-01-01".into()], Some(&cache_dir)).unwrap();
        assert!(other.is_empty(), "a different day must return nothing");

        let detail = photos_day_detail_in(&library_path, &day, Some(&cache_dir)).unwrap();
        assert_eq!(detail.len(), 1);
        assert!(!detail[0].preview.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn progress_throttle_always_lets_the_final_item_through() {
        let throttle = ProgressThrottle::new();
        assert!(throttle.should_emit(1, 1), "the only item is also the last one");
        assert!(throttle.should_emit(37, 37));
    }

    #[test]
    fn progress_throttle_emits_every_hundredth_item_regardless_of_elapsed_time() {
        let throttle = ProgressThrottle::new();
        assert!(!throttle.should_emit(1, 1000), "no time has passed and this isn't a multiple of 100");
        assert!(throttle.should_emit(100, 1000));
        assert!(!throttle.should_emit(101, 1000), "just emitted; too soon for another on count alone");
        assert!(throttle.should_emit(200, 1000));
    }

    fn checkerboard() -> GrayImage {
        GrayImage::from_fn(64, 64, |x, y| {
            Luma([if (x + y) % 2 == 0 { 255 } else { 0 }])
        })
    }

    fn gradient() -> GrayImage {
        GrayImage::from_fn(64, 64, |x, _| Luma([(x * 4) as u8]))
    }

    #[test]
    fn sharp_image_scores_higher_than_smooth() {
        assert!(sharpness(&checkerboard()) > sharpness(&gradient()) * 10.0);
    }

    /// Draw fine texture in one region and leave the rest smooth. This is the shape of
    /// a portrait with a melted background: the shot is in focus and must score like
    /// it, which averaging over the whole frame gets wrong.
    /// A photograph with a sharp subject against a soft background is a good photograph,
    /// not a failed one. Scoring the frame as a whole drowns the subject in the blur
    /// that surrounds it, which is why the score is taken from regions.
    #[test]
    fn a_sharp_subject_survives_a_soft_background() {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let (w, h) = photo.dimensions();
        let mut bokeh = image::imageops::blur(&photo, 5.0);
        // Paste the untouched centre back: subject in focus, everything else soft.
        let (cx, cy) = (w / 2, h / 2);
        for y in cy.saturating_sub(h / 6)..(cy + h / 6).min(h) {
            for x in cx.saturating_sub(w / 6)..(cx + w / 6).min(w) {
                bokeh.put_pixel(x, y, *photo.get_pixel(x, y));
            }
        }

        let all_sharp = sharpness(&photo);
        let all_blurred = sharpness(&image::imageops::blur(&photo, 5.0));
        let subject = sharpness(&bokeh);

        assert!(
            subject > all_blurred * 2.0,
            "a photo with a subject in focus must not read like a blurred one: \
             {subject:.3} vs {all_blurred:.3}"
        );
        assert!(
            subject > all_sharp * 0.4,
            "and it must stay in the neighbourhood of a fully sharp frame: \
             {subject:.3} vs {all_sharp:.3}"
        );
    }

    /// Blur the whole frame and the score must collapse, subject or not.
    #[test]
    fn a_globally_blurred_frame_still_scores_low() {
        let sharp = checkerboard();
        let blurred = image::imageops::blur(&sharp, 3.0);
        assert!(
            sharpness(&blurred) < sharpness(&sharp) / 10.0,
            "blur must still be detected"
        );
    }

    #[test]
    fn dhash_survives_brightness_shift() {
        let base = gradient();
        let brighter = GrayImage::from_fn(64, 64, |x, y| {
            Luma([base.get_pixel(x, y)[0].saturating_add(30)])
        });
        let dist = (hash_of(&base) ^ hash_of(&brighter)).count_ones();
        assert!(dist <= 4, "distance {dist} too large");
    }

    /// Splits `compute_view` into its parts, because "three seconds" is not a finding
    /// until you know which second is which.
    #[test]
    #[ignore = "timing, run explicitly with --ignored --release"]
    fn bench_regroup_phases() {
        use rayon::prelude::*;
        use std::time::Instant;

        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let mut state = 0xABCDEF0123456789u64;
        let n = 50_000usize;
        let hashes: Vec<Option<u128>> = (0..n)
            .map(|i| {
                Some(if i % 10 == 0 && i > 0 {
                    0u128
                } else {
                    ((next(&mut state) as u128) << 64) | next(&mut state) as u128
                })
            })
            .collect();
        let paths: Vec<String> = (0..n)
            .map(|i| format!("/Users/someone/Pictures/2024/{:02}/IMG_{:05}.HEIC", i % 12, i))
            .collect();
        let threshold = 28u32;

        // (a) exactly the shape in `compute_view`: rayon entered once per seed
        let started = Instant::now();
        let mut taken = vec![false; n];
        let mut pairs = 0usize;
        for i in 0..n {
            let Some(seed) = hashes[i] else { continue };
            if taken[i] { continue; }
            let members: Vec<usize> = (i + 1..n)
                .into_par_iter()
                .filter(|&j| !taken[j] && hashes[j].is_some_and(|h| (seed ^ h).count_ones() <= threshold))
                .collect();
            for j in members { taken[j] = true; pairs += 1; }
            taken[i] = true;
        }
        let per_seed_parallel = started.elapsed();

        // (b) the same scan with no rayon at all
        let started = Instant::now();
        let mut taken = vec![false; n];
        let mut pairs_b = 0usize;
        for i in 0..n {
            let Some(seed) = hashes[i] else { continue };
            if taken[i] { continue; }
            let members: Vec<usize> = (i + 1..n)
                .filter(|&j| !taken[j] && hashes[j].is_some_and(|h| (seed ^ h).count_ones() <= threshold))
                .collect();
            for j in members { taken[j] = true; pairs_b += 1; }
            taken[i] = true;
        }
        let serial = started.elapsed();

        // (c) rayon only while the remaining range is worth splitting
        let started = Instant::now();
        let mut taken = vec![false; n];
        let mut pairs_c = 0usize;
        for i in 0..n {
            let Some(seed) = hashes[i] else { continue };
            if taken[i] { continue; }
            let hit = |j: &usize| !taken[*j] && hashes[*j].is_some_and(|h| (seed ^ h).count_ones() <= threshold);
            let members: Vec<usize> = if n - i > 20_000 {
                (i + 1..n).into_par_iter().filter(hit).collect()
            } else {
                (i + 1..n).filter(hit).collect()
            };
            for j in members { taken[j] = true; pairs_c += 1; }
            taken[i] = true;
        }
        let hybrid = started.elapsed();

        // (d) what the path bookkeeping costs on its own
        let started = Instant::now();
        let mut by_stem: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
        for (i, path) in paths.iter().enumerate() {
            if let Some(key) = stem_key(Path::new(path)) {
                by_stem.entry(key).or_default().push(i);
            }
        }
        let stems = started.elapsed();

        // (e) sequential, and without the per-seed Vec: union as we go
        let started = Instant::now();
        let mut taken = vec![false; n];
        let mut parent: Vec<usize> = (0..n).collect();
        let mut pairs_e = 0usize;
        for i in 0..n {
            let Some(seed) = hashes[i] else { continue };
            if taken[i] { continue; }
            for j in i + 1..n {
                if taken[j] { continue; }
                if hashes[j].is_some_and(|h| (seed ^ h).count_ones() <= threshold) {
                    taken[j] = true;
                    uf_union(&mut parent, i, j);
                    pairs_e += 1;
                }
            }
            taken[i] = true;
        }
        let inline = started.elapsed();

        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        eprintln!("BENCH phases n={n}  (pairs {pairs}/{pairs_b}/{pairs_c}/{pairs_e})");
        eprintln!("  scan, sequentiel sans Vec {:>8.1} ms", ms(inline));
        eprintln!("  scan, rayon par graine   {:>8.1} ms", ms(per_seed_parallel));
        eprintln!("  scan, sequentiel          {:>8.1} ms", ms(serial));
        eprintln!("  scan, hybride             {:>8.1} ms", ms(hybrid));
        eprintln!("  indexation par racine     {:>8.1} ms", ms(stems));
        eprintln!("  coeurs disponibles        {:>8}", rayon::current_num_threads());
    }

    /// The two façades must agree about the clustering, or the slider would quietly
    /// show a different answer from the one a scan shows.
    #[test]
    fn groups_only_agrees_with_the_full_view() {
        let records = vec![
            record("/a/one.jpg", Some("aaa"), Some(0), 100),
            record("/a/two.jpg", Some("aaa"), Some(0), 100),
            record("/a/three.jpg", Some("bbb"), Some(0b1111), 200),
            record("/a/far.jpg", Some("ccc"), Some(u128::MAX), 300),
        ];
        for threshold in [0u32, 4, 8, 28, 128] {
            let full = compute_view(&records, threshold);
            let lean = compute_groups(&records, threshold);
            assert_eq!(
                full.groups.iter().map(|g| (g.indices.clone(), g.suggested, g.kind)).collect::<Vec<_>>(),
                lean.groups.iter().map(|g| (g.indices.clone(), g.suggested, g.kind)).collect::<Vec<_>>(),
                "the groups must not depend on whether the photographs were asked for (threshold {threshold})"
            );
            assert_eq!(full.reclaimable_bytes, lean.reclaimable_bytes);
            assert_eq!(full.total_files, lean.total_files);
            assert_eq!(full.photos.len(), records.len());
            assert!(lean.photos.is_empty(), "the lean view must carry no photographs");
        }
    }

    /// Where the time actually goes when the similarity slider moves.
    ///
    /// The Hamming scan has been measured before (see `bktree.rs`) and is not the
    /// suspect. This times the whole of `compute_view` against the two things it does
    /// besides comparing hashes — cloning every photograph, and handing the result to
    /// the webview as JSON — on a library the size of a real one.
    #[test]
    #[ignore = "timing, run explicitly with --ignored --release"]
    fn bench_regroup_cost() {
        use std::time::Instant;

        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn random_hash(state: &mut u64) -> u128 {
            ((next(state) as u128) << 64) | next(state) as u128
        }

        let mut state = 0xABCDEF0123456789u64;
        let n = 50_000;
        // The same shape as the BK-tree benchmark: mostly unrelated singletons with a
        // minority of tight clusters, because that is what a real library looks like.
        let mut records: Vec<Record> = Vec::with_capacity(n);
        for i in 0..n {
            let hash = if i % 10 == 0 && i > 0 {
                // one in ten is a near-copy of its predecessor
                records[i - 1].phash.unwrap() ^ (1u128 << (next(&mut state) % 128))
            } else {
                random_hash(&mut state)
            };
            let mut r = record(
                &format!("/Users/someone/Pictures/2024/{:02}/IMG_{:05}.HEIC", i % 12, i),
                Some(&format!("{i:064x}")),
                Some(hash),
                4_800_000 + i as u64,
            );
            r.photo.name = format!("IMG_{i:05}.HEIC");
            r.photo.preview = format!("/Users/someone/Library/Caches/previews/{i:016x}.jpg");
            r.photo.thumb = Some(format!("/Users/someone/Library/Caches/previews/{i:016x}-t.jpg"));
            r.photo.device = Some("iPhone 13 Pro".into());
            r.photo.blur = Some(80.0 + (i % 400) as f64);
            records.push(r);
        }

        let started = Instant::now();
        let view = compute_view(&records, 28);
        let clustering = started.elapsed();

        let started = Instant::now();
        let json = serde_json::to_string(&view).unwrap();
        let serialising = started.elapsed();

        // What a slider move costs on its own: the photographs are identical either
        // way, so every byte of them here is spent to say nothing new.
        let photos_json = serde_json::to_string(&view.photos).unwrap();
        let groups_json = serde_json::to_string(&view.groups).unwrap();

        eprintln!("BENCH n={n}");
        eprintln!("  compute_view      {:>8.1} ms", clustering.as_secs_f64() * 1000.0);
        eprintln!("  serialise view    {:>8.1} ms", serialising.as_secs_f64() * 1000.0);
        eprintln!("  payload total     {:>8.1} MB", json.len() as f64 / 1048576.0);
        eprintln!("    of which photos {:>8.1} MB", photos_json.len() as f64 / 1048576.0);
        eprintln!("    of which groups {:>8.1} MB", groups_json.len() as f64 / 1048576.0);
        eprintln!("  groups found      {:>8}", view.groups.len());

        // And the path a slider move actually takes now.
        let started = Instant::now();
        let lean = compute_groups(&records, 28);
        let lean_time = started.elapsed();
        let lean_json = serde_json::to_string(&lean).unwrap();
        eprintln!("  -- slider move --");
        eprintln!("  compute_groups    {:>8.1} ms", lean_time.as_secs_f64() * 1000.0);
        eprintln!("  payload           {:>8.2} MB", lean_json.len() as f64 / 1048576.0);
    }

    fn record(path: &str, sha: Option<&str>, phash: Option<u128>, size: u64) -> Record {
        Record {
            photo: Photo {
                path: path.into(),
                name: path.into(),
                size,
                width: 100,
                height: 100,
                taken: 0,
                blur: None,
                measurements: badshot::Measurements::default(),
                bad_shot: badshot::BadShot::default(),
                preview: path.into(),
                thumb: None,
                library: false,
                missing: false,
                preview_dims: None,
                lat: None,
                lon: None,
                format: "JPG".into(),
                device: None,
                kind: None,
            },
            sha: sha.map(String::from),
            phash,
        }
    }

    /// An asset read out of the Photos library, which the scan can compare against but
    /// never move.
    fn library_record(path: &str, phash: Option<u128>, size: u64) -> Record {
        let mut r = record(path, None, phash, size);
        r.photo.library = true;
        r
    }

    /// A loose copy on disk and the copy already filed in Photos must resolve one way
    /// only: keep the filed one. It is not a quality judgement — Photos exposes no way
    /// to delete a `media item`, so suggesting the library copy as surplus would be
    /// suggesting something that cannot be carried out.
    #[test]
    fn the_library_copy_is_the_one_kept() {
        let records = vec![
            record("/cards/IMG_1.jpg", None, Some(0), 5_000_000),
            library_record("/lib/IMG_1.jpeg", Some(0), 4_000_000),
        ];
        let view = compute_view(&records, 8);
        assert_eq!(view.groups.len(), 1, "the two copies should group");
        let group = &view.groups[0];
        assert!(
            view.photos[group.indices[group.suggested]].library,
            "the keeper must be the copy in the library"
        );
        assert_eq!(group.reason, Some("library"));
        assert_eq!(
            view.reclaimable_bytes, 5_000_000,
            "only the loose copy can actually be reclaimed"
        );
    }

    /// Two copies that are both already in Photos are a duplicate like any other. This
    /// used to be dropped as unresolvable, back when nothing in the library could be
    /// removed; the surplus copy is now handed to Photos for deletion, so the group is
    /// shown and its bytes are counted.
    #[test]
    fn a_group_wholly_inside_the_library_is_kept() {
        let records = vec![
            library_record("/lib/A.jpeg", Some(0), 4_000_000),
            library_record("/lib/B.jpeg", Some(0), 5_000_000),
        ];
        let view = compute_view(&records, 8);
        assert_eq!(view.groups.len(), 1, "two copies of one photo still group");
        let group = &view.groups[0];
        assert_eq!(group.indices.len(), 2);
        assert_eq!(
            view.reclaimable_bytes,
            records[group.indices.iter().find(|&&i| i != group.indices[group.suggested]).copied().unwrap()].photo.size,
            "the copy not kept is what can be freed"
        );
    }

    /// A raw as the scan builds it: the format tag is what the keeper ranking reads to
    /// put a raw ahead of its own export, so a fixture without it tests nothing.
    fn raw_record(path: &str, phash: Option<u128>, size: u64) -> Record {
        let mut r = record(path, None, phash, size);
        r.photo.kind = Some("ARW".into());
        r
    }

    /// A raw and its JPEG are one press of the shutter, whatever the fingerprints say.
    /// The pair is the case the perceptual pass is least able to catch: a JPEG exported
    /// with a heavy treatment no longer resembles the file it came from.
    #[test]
    fn a_raw_and_its_jpeg_are_paired_by_name_however_different_they_look() {
        let records = vec![
            raw_record("/card/DSC04812.ARW", Some(0), 20_000_000),
            record("/card/DSC04812.JPG", None, Some(u128::MAX), 4_000_000),
        ];
        let view = compute_view(&records, 8);

        assert_eq!(view.groups.len(), 1, "the pair must be grouped");
        assert_eq!(view.groups[0].kind, "pair");
        assert_eq!(view.groups[0].indices.len(), 2);
        let keeper = view.groups[0].indices[view.groups[0].suggested];
        assert_eq!(
            view.photos[keeper].path, "/card/DSC04812.ARW",
            "the raw is the one worth keeping"
        );
    }

    /// The counter comes round again on the next card. Two imports of DSC04812 are two
    /// photographs, and pairing them by name alone would propose deleting one of them.
    #[test]
    fn the_same_name_in_two_folders_is_not_a_pair() {
        let records = vec![
            raw_record("/tokyo/DSC04812.ARW", Some(0), 20_000_000),
            record("/paris/DSC04812.JPG", None, Some(u128::MAX), 4_000_000),
        ];
        assert!(compute_view(&records, 8).groups.is_empty());
    }

    /// The fast blur must be the slow blur, byte for byte.
    ///
    /// Rewriting a filter for speed is where a detector quietly changes its mind, so
    /// the naive version stays here as the reference and the two are compared on a
    /// deterministic noise field, borders included, at sizes that exercise every
    /// degenerate case.
    #[test]
    fn box_blur_is_equivalent_to_the_straightforward_version() {
        fn naive(g: &GrayImage) -> GrayImage {
            let (w, h) = g.dimensions();
            let mut out = GrayImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let mut sum = 0u32;
                    let mut n = 0u32;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                            if sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h {
                                sum += g.get_pixel(sx as u32, sy as u32)[0] as u32;
                                n += 1;
                            }
                        }
                    }
                    out.put_pixel(x, y, image::Luma([(sum / n) as u8]));
                }
            }
            out
        }

        for (w, h) in [(1, 1), (1, 7), (7, 1), (2, 2), (3, 3), (17, 11), (64, 48)] {
            let mut seed = 0x2545F491_4F6CDD1Du64;
            let img = GrayImage::from_fn(w, h, |_, _| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                image::Luma([(seed >> 24) as u8])
            });
            assert_eq!(
                box_blur3(&img).into_raw(),
                naive(&img).into_raw(),
                "{w}x{h} must blur identically"
            );
        }
    }

    /// Is the optimised downscale worth its change of values?
    ///
    /// The blur tab reads percentiles of the folder, so what has to survive is the
    /// order of the photographs, not the absolute figures. This reports Spearman
    /// between the two and the time each costs.
    ///
    /// SKIMRR_BENCH=/folder cargo test bench_filter -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_filter() {
        let Ok(dir) = std::env::var("SKIMRR_BENCH") else {
            return;
        };
        let files: Vec<PathBuf> = WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && is_image(e.path()))
            .map(|e| e.into_path())
            .take(200)
            .collect();

        let mut triangle = Vec::new();
        let mut boxed = Vec::new();
        let mut t_triangle = std::time::Duration::ZERO;
        let mut t_boxed = std::time::Duration::ZERO;
        let mut hash_same = 0usize;
        let mut hash_dist = Vec::new();

        for path in &files {
            let Ok(img) = image::open(path) else { continue };
            let gray = img.to_luma8();
            let rgb = img.to_rgb8();
            let h = (gray.height() as f64 * 1024.0 / gray.width() as f64).round() as u32;

            let t = std::time::Instant::now();
            let a = image::imageops::resize(&gray, 1024, h.max(1), FilterType::Triangle);
            t_triangle += t.elapsed();
            let t = std::time::Instant::now();
            let b = image::imageops::thumbnail(&gray, 1024, h.max(1));
            t_boxed += t.elapsed();

            triangle.push(score_of(&a));
            boxed.push(score_of(&b));

            // Et la même question pour l'empreinte, qui réduit en 9x9.
            let fa = fingerprint(&rgb);
            let small = image::imageops::thumbnail(&rgb, 9, 9);
            let fb = fingerprint_from_9x9(&small);
            match (fa, fb) {
                (Some(x), Some(y)) => {
                    if x == y {
                        hash_same += 1
                    }
                    hash_dist.push((x ^ y).count_ones());
                }
                (None, None) => hash_same += 1,
                _ => hash_dist.push(128),
            }
        }

        let rank = |v: &[f64]| {
            let mut idx: Vec<usize> = (0..v.len()).collect();
            idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
            let mut r = vec![0.0; v.len()];
            for (place, &i) in idx.iter().enumerate() {
                r[i] = place as f64
            }
            r
        };
        let (ra, rb) = (rank(&triangle), rank(&boxed));
        let n = ra.len() as f64;
        let mean = n / 2.0 - 0.5;
        let cov: f64 = ra
            .iter()
            .zip(&rb)
            .map(|(a, b)| (a - mean) * (b - mean))
            .sum();
        let va: f64 = ra.iter().map(|a| (a - mean).powi(2)).sum();
        let vb: f64 = rb.iter().map(|b| (b - mean).powi(2)).sum();
        println!("\n{} photos", ra.len());
        println!("netteté : Spearman {:.5}", cov / (va.sqrt() * vb.sqrt()));
        println!(
            "  triangle {:.2} ms/fichier, boîte {:.2} ms/fichier",
            t_triangle.as_secs_f64() * 1e3 / n,
            t_boxed.as_secs_f64() * 1e3 / n
        );
        let mean_dist = hash_dist.iter().sum::<u32>() as f64 / hash_dist.len().max(1) as f64;
        println!(
            "empreinte : {} identiques sur {}, distance moyenne {:.2} bits sur 128",
            hash_same,
            files.len(),
            mean_dist
        );
    }

    /// Where a scan actually spends its time, on a real folder.
    ///
    /// SKIMRR_BENCH=/path/to/folder cargo test bench_stages -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_stages() {
        let Ok(dir) = std::env::var("SKIMRR_BENCH") else {
            eprintln!("set SKIMRR_BENCH to a folder of photographs");
            return;
        };
        let files: Vec<PathBuf> = WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && is_image(e.path()))
            .map(|e| e.into_path())
            .take(200)
            .collect();
        assert!(!files.is_empty(), "no photographs under {dir}");

        let mut decode = std::time::Duration::ZERO;
        let mut luma = std::time::Duration::ZERO;
        let mut rgb = std::time::Duration::ZERO;
        let mut hash = std::time::Duration::ZERO;
        let mut sharp = std::time::Duration::ZERO;
        let mut preview = std::time::Duration::ZERO;
        let mut pixels: u64 = 0;
        let mut resize_t = std::time::Duration::ZERO;
        let mut blur_t = std::time::Duration::ZERO;
        let mut tile_t = std::time::Duration::ZERO;

        for path in &files {
            let t = std::time::Instant::now();
            let Ok(img) = image::open(path) else { continue };
            decode += t.elapsed();
            pixels += img.width() as u64 * img.height() as u64;

            let t = std::time::Instant::now();
            let gray = img.to_luma8();
            luma += t.elapsed();

            let t = std::time::Instant::now();
            let colour = img.to_rgb8();
            rgb += t.elapsed();

            let t = std::time::Instant::now();
            let _ = fingerprint(&colour);
            hash += t.elapsed();

            let t = std::time::Instant::now();
            let _ = sharpness(&gray);
            sharp += t.elapsed();

            // La même chose décomposée, pour savoir où va ce temps.
            let t = std::time::Instant::now();
            let small = if gray.width() > 1024 {
                let hh = (gray.height() as f64 * 1024.0 / gray.width() as f64).round() as u32;
                image::imageops::resize(&gray, 1024, hh.max(1), FilterType::Triangle)
            } else {
                gray.clone()
            };
            resize_t += t.elapsed();
            let t = std::time::Instant::now();
            let blurred = box_blur3(&small);
            blur_t += t.elapsed();
            let t = std::time::Instant::now();
            let _ = normalised_detail(
                &blurred,
                0,
                0,
                blurred.width().min(64),
                blurred.height().min(64),
            );
            tile_t += t.elapsed();

            let t = std::time::Instant::now();
            let _ = encode_preview(&img);
            preview += t.elapsed();
        }

        let n = files.len() as u32;
        println!("\n{} fichiers, {:.1} Mpx au total", n, pixels as f64 / 1e6);
        for (name, d) in [
            ("décodage", decode),
            ("to_luma8", luma),
            ("to_rgb8", rgb),
            ("empreinte", hash),
            ("netteté", sharp),
            ("rendu JPEG", preview),
            ("  dont réduction", resize_t),
            ("  dont flou 3x3", blur_t),
            ("  dont 1 tuile", tile_t),
        ] {
            println!(
                "{name:<12} {:>8.1} ms au total  {:>6.2} ms par fichier",
                d.as_secs_f64() * 1e3,
                d.as_secs_f64() * 1e3 / n as f64
            );
        }
    }

    /// The suggestion is only trustworthy if it can be explained, and the explanation
    /// is read against the photograph that came second, never against the winner alone.
    #[test]
    fn the_reason_names_the_criterion_that_decided() {
        let view = compute_view(
            &[
                raw_record("/card/DSC1.ARW", Some(0), 20_000_000),
                record("/card/DSC1.JPG", None, Some(0), 4_000_000),
            ],
            8,
        );
        assert_eq!(view.groups[0].reason, Some("raw"));

        let mut small = record("/card/a.jpg", Some("s"), Some(0), 1_000);
        small.photo.width = 50;
        let big = record("/card/b.jpg", Some("s"), Some(0), 2_000);
        let view = compute_view(&[small, big], 8);
        assert_eq!(view.groups[0].reason, Some("pixels"));

        let mut soft = record("/card/c.jpg", Some("t"), Some(0), 1_000);
        soft.photo.blur = Some(10.0);
        let mut sharp = record("/card/d.jpg", Some("t"), Some(0), 1_000);
        sharp.photo.blur = Some(90.0);
        let view = compute_view(&[soft, sharp], 8);
        assert_eq!(view.groups[0].reason, Some("sharp"));
        let keeper = view.groups[0].indices[view.groups[0].suggested];
        assert_eq!(view.photos[keeper].path, "/card/d.jpg");
    }

    /// Two files alike on every count do exist. Inventing a reason there would be a
    /// small lie in the one place the interface is asking to be trusted.
    #[test]
    fn no_reason_is_given_when_nothing_separates_them() {
        let view = compute_view(
            &[
                record("/card/a.jpg", Some("same"), Some(0), 1_000),
                record("/card/b.jpg", Some("same"), Some(0), 1_000),
            ],
            8,
        );
        assert_eq!(view.groups[0].reason, None);
    }

    /// An export sitting beside its source is only a certainty when one side is raw.
    #[test]
    fn a_pair_needs_one_raw_side() {
        let records = vec![
            record("/card/IMG_0001.jpg", None, Some(0), 4_000_000),
            record("/card/IMG_0001.png", None, Some(u128::MAX), 2_000_000),
        ];
        assert!(compute_view(&records, 8).groups.is_empty());
    }

    /// Pairing must not cost the perceptual grouping: two shots of the same scene, each
    /// with its own JPEG, still belong together, and the group is then no longer a pair.
    #[test]
    fn pairs_merge_with_the_resemblance_they_also_have() {
        let records = vec![
            raw_record("/card/DSC04812.ARW", Some(0b0000), 20_000_000),
            record("/card/DSC04812.JPG", None, Some(0b0000), 4_000_000),
            raw_record("/card/DSC04813.ARW", Some(0b0001), 20_000_000),
            record("/card/DSC04813.JPG", None, Some(0b0001), 4_000_000),
        ];
        let view = compute_view(&records, 8);

        assert_eq!(view.groups.len(), 1, "one cluster, not two");
        assert_eq!(view.groups[0].indices.len(), 4);
        assert_eq!(
            view.groups[0].kind, "similar",
            "several shots are no longer the exact pairing of one"
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skimrr-test-{tag}-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trash_then_undo_restores_files() {
        let work = temp_dir("trash");
        let trash = work.join("trash");
        let nested = work.join("sub");
        std::fs::create_dir_all(&nested).unwrap();

        let a = work.join("a.jpg");
        let b = nested.join("b.jpg");
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();
        let paths = vec![
            (a.to_string_lossy().into_owned(), None),
            (b.to_string_lossy().into_owned(), None),
        ];

        let batch = trash_to(&trash, &paths).unwrap();
        assert!(!a.exists() && !b.exists(), "originals should be gone");
        assert!(trash.join(&batch).join("manifest.json").exists());

        assert_eq!(undo_from(&trash, &batch).unwrap(), 2);
        assert_eq!(std::fs::read(&a).unwrap(), b"aaa");
        assert_eq!(std::fs::read(&b).unwrap(), b"bbbb");
        assert!(
            !trash.join(&batch).exists(),
            "batch folder should be cleaned"
        );

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn failed_batch_rolls_back() {
        let work = temp_dir("rollback");
        let trash = work.join("trash");
        let a = work.join("a.jpg");
        std::fs::write(&a, b"aaa").unwrap();

        let missing = work.join("does-not-exist.jpg");
        let paths = vec![
            (a.to_string_lossy().into_owned(), None),
            (missing.to_string_lossy().into_owned(), None),
        ];

        assert!(trash_to(&trash, &paths).is_err());
        assert_eq!(
            std::fs::read(&a).unwrap(),
            b"aaa",
            "first file must be restored when a later move fails"
        );

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn trash_listing_reports_batches_and_sizes() {
        let work = temp_dir("listing");
        let trash = work.join("trash");
        let a = work.join("a.jpg");
        std::fs::write(&a, b"12345").unwrap();
        let batch = trash_to(&trash, &[(a.to_string_lossy().into_owned(), None)]).unwrap();

        let batches = read_trash(&trash);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].batch_id, batch);
        assert_eq!(batches[0].bytes, 5);
        assert_eq!(batches[0].photos[0].name, "a.jpg");
        assert!(Path::new(&batches[0].photos[0].stored_path).exists());

        std::fs::remove_dir_all(&work).ok();
    }

    /// Renditions are cached by name, so the name has to change when the file changes
    /// or when the way we render it changes, otherwise a fix like baked-in rotation
    /// never reaches anyone who already scanned that folder.
    #[test]
    fn preview_cache_key_tracks_the_file_and_the_renderer() {
        let work = temp_dir("cachekey");
        let a = work.join("photo.jpg");
        std::fs::write(&a, b"first").unwrap();

        let first = preview_name(&a, "grid");
        assert_eq!(
            first,
            preview_name(&a, "grid"),
            "stable for an unchanged file"
        );
        assert_ne!(
            first,
            preview_name(&a, "detail"),
            "each rendition is its own file"
        );

        // A different file at the same path must not reuse the old rendition.
        std::fs::write(&a, b"second, and longer").unwrap();
        assert_ne!(
            first,
            preview_name(&a, "grid"),
            "edited file needs a new rendition"
        );

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn undo_rejects_path_traversal() {
        let work = temp_dir("traversal");
        assert!(undo_from(&work, "../escape").is_err());
        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn grouping_exact_and_similar() {
        let records = vec![
            record("a.jpg", Some("s1"), Some(0b1010), 10),
            record("b.jpg", Some("s1"), Some(0b1010), 10),
            record("c.jpg", Some("s2"), Some(0b1011), 30),
            record("d.jpg", None, Some(!0u128), 5),
        ];
        // c is 1 bit away from a/b: one similar group of 3, d alone.
        let view = compute_view(&records, 4);
        assert_eq!(view.groups.len(), 1);
        assert_eq!(view.groups[0].kind, "similar");
        assert_eq!(view.groups[0].indices.len(), 3);
        // With threshold 0, only the exact pair remains grouped.
        let view = compute_view(&records, 0);
        assert_eq!(view.groups.len(), 1);
        assert_eq!(view.groups[0].kind, "exact");
        assert_eq!(view.groups[0].similarity, 100);
        // Suggested keeper is the biggest reclaim choice: sizes equal → any; reclaimable = 10.
        assert_eq!(view.reclaimable_bytes, 10);
    }

    /// A corpus covering the nuisances that break sharpness measures in the wild:
    /// contrast, noise, and a small sharp artefact inside an otherwise blurred frame.
    /// Every entry carries the answer a photographer would give.
    fn bench_corpus() -> Vec<(&'static str, GrayImage, bool)> {
        /// Photo-like content: edges, a gradient, and fine texture. The blurred variant
        /// is this one actually blurred, not a coarser pattern standing in for blur:
        /// an earlier draft generated a different texture and the corpus then tested
        /// something no camera produces.
        fn scene(sharp: bool) -> GrayImage {
            let base = GrayImage::from_fn(512, 512, |x, y| {
                let gradient = (x as f64 / 512.0) * 90.0 + 60.0;
                let edge = if (x / 64) % 2 == 0 { 40.0 } else { -40.0 };
                let texture = (((x * 7 + y * 13) % 17) as f64) * 3.0;
                Luma([(gradient + edge + texture).clamp(0.0, 255.0) as u8])
            });
            if sharp {
                base
            } else {
                image::imageops::blur(&base, 3.0)
            }
        }
        fn contrast(g: &GrayImage, factor: f64) -> GrayImage {
            GrayImage::from_fn(g.width(), g.height(), |x, y| {
                let v = g.get_pixel(x, y)[0] as f64;
                Luma([((v - 128.0) * factor + 128.0).clamp(0.0, 255.0) as u8])
            })
        }
        fn noisy(g: &GrayImage, amount: f64) -> GrayImage {
            GrayImage::from_fn(g.width(), g.height(), |x, y| {
                let seed = (x as u64)
                    .wrapping_mul(1103515245)
                    .wrapping_add((y as u64).wrapping_mul(12345));
                let n = ((seed % 1000) as f64 / 1000.0 - 0.5) * amount;
                Luma([(g.get_pixel(x, y)[0] as f64 + n).clamp(0.0, 255.0) as u8])
            })
        }

        let sharp = scene(true);
        let blur = scene(false);
        let mut stamped = blur.clone();
        // A date stamp burned into a corner: sharp, tiny, and no evidence at all that
        // the photograph itself is usable.
        for y in 470..486 {
            for x in 20..90 {
                stamped.put_pixel(x, y, Luma([if (x + y) % 2 == 0 { 255 } else { 0 }]));
            }
        }

        // A real photograph carries texture no synthetic scene reproduces: foliage,
        // grain, and areas that are genuinely featureless. Tuning only against
        // generated patterns is how a metric ends up fitting the generator.
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let photo_blur = image::imageops::blur(&photo, 4.0);
        // A graded series: σ0.6 is a frame a photographer keeps, σ1.6 is one they do
        // not. An earlier draft of this bench called σ1.6 sharp, which is simply not
        // what it looks like, and every measure was judged against that mistake.
        let barely = image::imageops::blur(&photo, 0.6);
        let softened = image::imageops::blur(&photo, 1.6);

        vec![
            ("PHOTO nette", photo.clone(), true),
            ("PHOTO nette, faible contraste", contrast(&photo, 0.3), true),
            ("PHOTO à peine adoucie", barely, true),
            ("PHOTO adoucie", softened, false),
            ("PHOTO floue", photo_blur.clone(), false),
            (
                "PHOTO floue, contraste poussé",
                contrast(&photo_blur, 2.4),
                false,
            ),
            ("nette, contraste normal", sharp.clone(), true),
            (
                "nette, faible contraste (brume)",
                contrast(&sharp, 0.25),
                true,
            ),
            ("nette, fort contraste", contrast(&sharp, 1.8), true),
            ("nette et bruitée", noisy(&sharp, 30.0), true),
            ("floue, contraste normal", blur.clone(), false),
            ("floue, contraste poussé", contrast(&blur, 2.6), false),
            ("floue et bruitée", noisy(&blur, 30.0), false),
            ("floue avec horodatage net", stamped, false),
        ]
    }

    /// Prints both measures side by side and fails if the new one cannot separate
    /// sharp from blurred across every nuisance in the corpus.
    #[test]
    fn sharpness_separates_sharp_from_blurred() {
        println!(
            "\n  {:<34} {:>11} {:>11} {:>11}",
            "cas", "actuel", "Nième brut", "livré"
        );
        // Separation is judged inside each family. The shipped threshold is a fraction
        // of the scanned library's own median, so what matters is telling sharp from
        // blurred among comparable images, not across unrelated populations.
        let mut margins: Vec<(&str, f64, f64)> = Vec::new();
        for family in ["PHOTO", "synthétique"] {
            let (mut ws, mut bb, mut lws, mut lbb) = (f64::MAX, 0.0_f64, f64::MAX, 0.0_f64);
            for (name, img, is_sharp) in bench_corpus() {
                let mine = name.starts_with("PHOTO") == (family == "PHOTO");
                if !mine {
                    continue;
                }
                let legacy = sharpness_legacy(&img);
                let raw = sharpness_with(&img, false);
                let now = sharpness(&img);
                println!("  {name:<34} {legacy:>11.1} {raw:>11.1} {now:>11.3}");
                if is_sharp {
                    ws = ws.min(now);
                    lws = lws.min(legacy);
                } else {
                    bb = bb.max(now);
                    lbb = lbb.max(legacy);
                }
            }
            margins.push((family, lws / lbb.max(1e-12), ws / bb.max(1e-12)));
        }
        println!();
        for (family, legacy, now) in &margins {
            println!("  marge {family:<12} actuel {legacy:>8.2}x    nouveau {now:>8.2}x");
        }
        for (family, _, now) in &margins {
            assert!(
                *now > 1.0,
                "in the {family} family the dullest sharp image must still outrank the \
                 most flattering blurred one (margin {now:.2})"
            );
        }
    }

    #[test]
    #[ignore = "timing, run explicitly with --ignored --release"]
    fn sharpness_cost() {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        for (name, f) in [
            ("actuel ", &sharpness_legacy as &dyn Fn(&GrayImage) -> f64),
            ("nouveau", &sharpness as &dyn Fn(&GrayImage) -> f64),
        ] {
            let t = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(f(&photo));
            }
            println!(
                "  {name} : {:>7.2} ms par image",
                t.elapsed().as_secs_f64() * 1000.0 / 20.0
            );
        }
    }

    /// Pairs and clusters that a duplicate finder must get right, with the answer a
    /// photographer would give. Built from the real fixture so the hashes see genuine
    /// texture rather than a pattern chosen to flatter them.
    fn duplicate_corpus() -> Vec<(&'static str, Vec<GrayImage>, bool)> {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let (w, h) = photo.dimensions();

        let recompressed = image::imageops::blur(&photo, 0.4);
        let resized = image::imageops::resize(&photo, w / 2, h / 2, FilterType::Lanczos3);
        let brighter = GrayImage::from_fn(w, h, |x, y| {
            Luma([photo.get_pixel(x, y)[0].saturating_add(25)])
        });
        let cropped =
            image::imageops::crop_imm(&photo, w / 10, h / 10, w * 8 / 10, h * 8 / 10).to_image();
        // A different scene: same overall brightness, structure rotated a quarter turn.
        let rotated = image::imageops::rotate90(&photo);
        // Two featureless frames, the case where a coarse hash has nothing to grip.
        let sky_a = GrayImage::from_fn(w, h, |_, y| Luma([(150 + y / 200) as u8]));
        let sky_b = GrayImage::from_fn(w, h, |_, y| Luma([(152 + y / 190) as u8]));

        vec![
            (
                "même image, recompressée",
                vec![photo.clone(), recompressed],
                true,
            ),
            (
                "même image, redimensionnée",
                vec![photo.clone(), resized],
                true,
            ),
            (
                "même image, plus claire",
                vec![photo.clone(), brighter],
                true,
            ),
            (
                "même image, recadrée à 80 %",
                vec![photo.clone(), cropped],
                true,
            ),
            (
                "scène différente (pivotée)",
                vec![photo.clone(), rotated],
                false,
            ),
            ("deux ciels unis différents", vec![sky_a, sky_b], false),
        ]
    }

    /// Scores every pair with the shipped hash and reports the Hamming distance, so the
    /// threshold can be judged against evidence instead of taste.
    #[test]
    fn duplicate_pairs_are_separable() {
        println!("\n  {:<34} {:>10}", "cas", "distance");
        let mut worst_same = 0u32;
        let mut best_different = 64u32;
        for (name, images, same) in duplicate_corpus() {
            let d = match (
                fingerprint(&to_rgb(&images[0])),
                fingerprint(&to_rgb(&images[1])),
            ) {
                (Some(a), Some(b)) => (a ^ b).count_ones(),
                // A refused hash means "cannot be judged by resemblance", which for an
                // unrelated pair is the right answer: score it as maximally distant.
                _ => 64,
            };
            println!("  {name:<34} {d:>10}");
            if same {
                worst_same = worst_same.max(d);
            } else {
                best_different = best_different.min(d);
            }
        }
        println!(
            "\n  pire cas identique : {worst_same}   meilleur cas différent : {best_different}"
        );
        assert!(
            worst_same < best_different,
            "no threshold can separate these: duplicates reach {worst_same} while \
             unrelated photos come as close as {best_different}"
        );
    }

    /// Union-find joins anything transitively: A close to B and B close to C puts A and
    /// C in one group even when they look nothing alike. On a large library this is how
    /// a handful of near-matches becomes one absurd cluster.
    #[test]
    fn grouping_does_not_chain_unrelated_photos() {
        // Three hashes on a line: each neighbour within 8 bits, the ends 16 apart.
        let a = 0x0000_0000_0000_0000u128;
        let b = 0x0000_0000_0000_00FFu128;
        let c = 0x0000_0000_0000_FFFFu128;
        assert_eq!((a ^ b).count_ones(), 8);
        assert_eq!((b ^ c).count_ones(), 8);
        assert_eq!((a ^ c).count_ones(), 16);

        let records = vec![
            record("/a.jpg", None, Some(a), 10),
            record("/b.jpg", None, Some(b), 10),
            record("/c.jpg", None, Some(c), 10),
        ];
        let view = compute_view(&records, 8);
        let biggest = view
            .groups
            .iter()
            .map(|g| g.indices.len())
            .max()
            .unwrap_or(0);
        assert!(
            biggest < 3,
            "a and c are 16 bits apart and must not share a group merely because b sits \
             between them"
        );
    }

    #[test]
    #[ignore = "measurement, run explicitly"]
    fn thumbnail_spread_of_real_and_empty_frames() {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let (w, h) = photo.dimensions();
        let cases: Vec<(&str, GrayImage)> = vec![
            ("photo réelle", photo.clone()),
            (
                "photo très sombre",
                GrayImage::from_fn(w, h, |x, y| {
                    Luma([(photo.get_pixel(x, y)[0] as f64 * 0.12) as u8])
                }),
            ),
            (
                "ciel dégradé",
                GrayImage::from_fn(w, h, |_, y| Luma([(150 + y / 200) as u8])),
            ),
            ("mur uni", GrayImage::from_fn(w, h, |_, _| Luma([200]))),
        ];
        for (name, img) in cases {
            let small = image::imageops::resize(&img, 9, 8, FilterType::Triangle);
            let vals: Vec<f64> = small.pixels().map(|p| p[0] as f64).collect();
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64;
            let spread = vals.iter().cloned().fold(f64::MIN, f64::max)
                - vals.iter().cloned().fold(f64::MAX, f64::min);
            println!("  {name:<20} variance {var:>9.2}   amplitude {spread:>6.1}");
        }
    }

    /// The correction exists to rescue dull frames, never to punish vivid ones. An
    /// uncapped division by contrast inverted these two, and users reported it plainly:
    /// their good photographs were being called blurry.
    #[test]
    fn a_vivid_photo_outranks_the_same_photo_gone_dull() {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let dulled = GrayImage::from_fn(photo.width(), photo.height(), |x, y| {
            let v = photo.get_pixel(x, y)[0] as f64;
            Luma([((v - 128.0) * 0.3 + 128.0).clamp(0.0, 255.0) as u8])
        });

        let vivid = sharpness(&photo);
        let flat = sharpness(&dulled);
        assert!(
            vivid > flat,
            "the vivid frame must rank above its washed-out twin: {vivid:.3} vs {flat:.3}"
        );
        // Both are the same sharp photograph, so they must stay close together rather
        // than land on opposite sides of any sensible threshold.
        assert!(
            flat > vivid * 0.5,
            "and they must stay comparable: {flat:.3} vs {vivid:.3}"
        );
    }

    /// The failure that forced the fingerprint wider: a warm night scene and a green
    /// daylight one measured 11 bits apart under 64-bit grey hashing, closer than two
    /// genuine duplicates of the night scene. These two fixtures share their luma
    /// structure exactly, so anything that separates them is colour doing the work,
    /// which is precisely what the old fingerprint threw away.
    #[test]
    fn colour_carries_what_grey_cannot() {
        let pattern = |warm: bool| {
            image::RgbImage::from_fn(120, 90, |x, y| {
                let lit = ((x / 12) % 2 == 0 && y > 30) as u8;
                if warm {
                    image::Rgb([60 + lit * 150, 18 + lit * 60, 12 + lit * 20])
                } else {
                    image::Rgb([120 + lit * 30, 170 + lit * 60, 110 + lit * 20])
                }
            })
        };
        let desaturate = |img: &image::RgbImage| {
            image::RgbImage::from_fn(img.width(), img.height(), |x, y| {
                let p = img.get_pixel(x, y).0;
                let v = (0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64) as u8;
                image::Rgb([v, v, v])
            })
        };

        let warm = pattern(true);
        let green = pattern(false);
        let in_colour = (fingerprint(&warm).unwrap() ^ fingerprint(&green).unwrap()).count_ones();
        let in_grey = (fingerprint(&desaturate(&warm)).unwrap()
            ^ fingerprint(&desaturate(&green)).unwrap())
        .count_ones();

        assert!(
            in_colour >= in_grey * 3,
            "colour must do most of the separating here: {in_colour} in colour \
             against {in_grey} once desaturated"
        );
    }

    /// Distances now run to 128, and the similarity readout subtracts from that width
    /// on an unsigned integer. A stale 64 there wrapped around and printed 67108860 %
    /// in the group header, which is how this was found.
    #[test]
    fn similarity_never_wraps_around() {
        let far = record("/a.jpg", None, Some(0u128), 10);
        let other = record("/b.jpg", None, Some(u128::MAX), 10);
        let view = compute_view(&[far, other], FINGERPRINT_BITS);
        for group in &view.groups {
            assert!(
                group.similarity <= 100,
                "similarity must stay a percentage, got {}",
                group.similarity
            );
        }
    }

    /// Brings the embedding model up from Rust: loads the weights, embeds two frames of
    /// one burst and one unrelated photograph, and checks that the burst pair sits much
    /// closer together. The same three files measured 2.27x apart under the Python
    /// prototype, so anything near that means the port is faithful.
    #[test]
    #[ignore = "model bring-up"]
    fn embeddings_separate_a_burst_from_the_rest() {
        use candle_core::{Device, Tensor};

        let device = Device::Cpu;
        let model = candle_onnx::read_file("models/mobilenetv2.onnx").expect("model loads");
        let input_name = model.graph.as_ref().unwrap().input[0].name.clone();

        let embed = |name: &str| -> Vec<f32> {
            let img = image::open(format!("/tmp/tokyo-jpg/{name}.jpg"))
                .expect("fixture")
                .resize_exact(224, 224, image::imageops::FilterType::Triangle)
                .to_rgb8();
            let mean = [0.485f32, 0.456, 0.406];
            let std = [0.229f32, 0.224, 0.225];
            let mut data = vec![0f32; 3 * 224 * 224];
            for y in 0..224usize {
                for x in 0..224usize {
                    let p = img.get_pixel(x as u32, y as u32).0;
                    for c in 0..3 {
                        data[c * 224 * 224 + y * 224 + x] =
                            (p[c] as f32 / 255.0 - mean[c]) / std[c];
                    }
                }
            }
            let input = Tensor::from_vec(data, (1, 3, 224, 224), &device).unwrap();
            let mut inputs = std::collections::HashMap::new();
            inputs.insert(input_name.clone(), input);
            let out = candle_onnx::simple_eval(&model, inputs).expect("inference");
            let v = out
                .values()
                .next()
                .unwrap()
                .flatten_all()
                .unwrap()
                .to_vec1::<f32>()
                .unwrap();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            v.into_iter().map(|x| x / norm).collect()
        };

        let cosine = |a: &[f32], b: &[f32]| 1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let mut names: Vec<String> = std::fs::read_dir("/tmp/tokyo-jpg")
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().replace(".jpg", ""))
            .filter(|n| n.starts_with("DSC"))
            .collect();
        names.sort();
        let seed = embed("DSC00384");
        let mut rows: Vec<(f32, String)> = names
            .iter()
            .filter(|n| n.as_str() != "DSC00384")
            .map(|n| (cosine(&seed, &embed(n)), n.clone()))
            .collect();
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let is_true = |n: &str| n == "DSC00374" || n == "DSC00375";
        println!("  {} photos comparées", rows.len());
        for (d, n) in rows.iter().take(5) {
            println!(
                "    {d:.4}  {n}{}",
                if is_true(n) { "  <- doublon" } else { "" }
            );
        }
        let worst_true = rows
            .iter()
            .filter(|(_, n)| is_true(n))
            .map(|(d, _)| *d)
            .fold(0.0, f32::max);
        let first_other = rows
            .iter()
            .filter(|(_, n)| !is_true(n))
            .map(|(d, _)| *d)
            .fold(f32::MAX, f32::min);
        println!("  marge : {:.2}x", first_other / worst_true);
        assert!(
            first_other > worst_true,
            "both burst frames must rank first"
        );
    }

    /// Measures the shipped sharpness score against blur we applied ourselves, which is
    /// the only ground truth available: the user's own folder holds no failed frames.
    ///
    /// Two questions, and they are not the same one. Paired: does a photograph always
    /// outscore its own blurred copy? Cross-photo: can ONE threshold separate every
    /// sharp frame from every blurred one? The second is the job the app actually does,
    /// and the one a detail-density measure is bad at.
    #[test]
    #[ignore = "measurement"]
    fn sharpness_against_applied_blur() {
        let mut files: Vec<PathBuf> = std::fs::read_dir("/tmp/blur-bench")
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jpg").unwrap_or(false))
            .collect();
        files.sort();
        if files.is_empty() {
            return;
        }

        let levels = [("léger", 1.0f32), ("net-flou", 2.0), ("franc", 4.0)];
        let mut sharp_scores = Vec::new();
        let mut blurred: Vec<Vec<f64>> = vec![Vec::new(); levels.len()];
        let mut paired_ok = vec![0usize; levels.len()];

        for path in &files {
            let Ok(img) = image::open(path) else { continue };
            let grey = img.to_luma8();
            let base = sharpness(&grey);
            sharp_scores.push(base);
            for (i, (_, sigma)) in levels.iter().enumerate() {
                let soft = sharpness(&image::imageops::blur(&grey, *sigma));
                blurred[i].push(soft);
                if base > soft {
                    paired_ok[i] += 1;
                }
            }
        }

        let n = sharp_scores.len();
        println!("\n  {n} photographies");
        println!("\n  APPARIÉ : la photo bat-elle sa propre version floutée ?");
        for (i, (name, sigma)) in levels.iter().enumerate() {
            println!("    flou {name:<9} (σ{sigma})  {}/{n}", paired_ok[i]);
        }

        /* Reported as an AUC, the chance a sharp frame taken at random outscores a
        blurred one taken at random. An earlier version of this bench printed the
        single most detailed blurred frame against the single dullest sharp one,
        which is a worst-case statistic: it read as a catastrophe when the measure
        was in fact separating the two populations 99% of the time. */
        println!("\n  CROISÉ : chance qu'une nette au hasard batte une floue au hasard");
        for (i, (name, sigma)) in levels.iter().enumerate() {
            let wins: usize = sharp_scores
                .iter()
                .map(|s| blurred[i].iter().filter(|b| s > b).count())
                .sum();
            let auc = wins as f64 / (sharp_scores.len() * blurred[i].len()) as f64;
            println!("    flou {name:<9} (σ{sigma})  {:.1} %", auc * 100.0);
        }
    }

    /// The failure a whole-frame score cannot see: a crisp background rescuing a
    /// photograph whose subject is soft, which is how most portraits fail. Measured
    /// over 60 real frames the whole-frame reading only fell to 80% of the original;
    /// the centre reading brings that to 31%.
    #[test]
    fn a_blurred_subject_is_not_saved_by_a_sharp_background() {
        let photo = image::load_from_memory(include_bytes!("../tests/fixtures/scene-sharp.png"))
            .expect("fixture decodes")
            .to_luma8();
        let (w, h) = photo.dimensions();

        // Blur the middle only, leaving the edges untouched.
        let (cw, ch) = (w * 45 / 100, h * 45 / 100);
        let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
        let patch = image::imageops::blur(
            &image::imageops::crop_imm(&photo, x0, y0, cw, ch).to_image(),
            3.0,
        );
        let mut subject_soft = photo.clone();
        image::imageops::overlay(&mut subject_soft, &patch, x0 as i64, y0 as i64);

        let sharp = sharpness(&photo);
        let soft = sharpness(&subject_soft);
        /* 0.76 on this fixture with the shipped slack; the whole-frame reading left it
        at 0.98, effectively blind to the fault. The bound is set just above what is
        measured so a regression toward blindness fails here. */
        assert!(
            soft < sharp * 0.85,
            "a soft subject must cost real score even with sharp edges: \
             {soft:.3} against {sharp:.3}"
        );

        // And the reverse must still be safe: a deliberately melted background around a
        // sharp subject is a good photograph, not a failure.
        let bokeh_bg = image::imageops::blur(&photo, 3.0);
        let mut bokeh = bokeh_bg.clone();
        let centre = image::imageops::crop_imm(&photo, x0, y0, cw, ch).to_image();
        image::imageops::overlay(&mut bokeh, &centre, x0 as i64, y0 as i64);
        let kept = sharpness(&bokeh);
        assert!(
            kept > sharpness(&bokeh_bg) * 3.0,
            "a sharp subject must lift a melted background: {kept:.3}"
        );
    }

    #[test]
    fn managed_libraries_are_recognised() {
        for name in [
            "Photos Library.photoslibrary",
            "Old iPhoto Library.photolibrary",
            "Aperture.aplibrary",
            "Preview.app",
            "Catalog Previews.lrdata",
        ] {
            assert!(
                is_opaque_package(Path::new("/Users/x/Pictures").join(name).as_path()),
                "{name} must never be walked into"
            );
        }
    }

    #[test]
    fn the_walk_does_not_enter_a_package() {
        let root = temp_dir("walk-package");
        std::fs::write(root.join("holiday.jpg"), b"x").unwrap();
        let inside = root.join("Some App.app").join("Contents").join("Resources");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(inside.join("icon.png"), b"x").unwrap();
        let library = root.join("Photos Library.photoslibrary").join("originals");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("IMG_0001.jpg"), b"x").unwrap();

        // The same chain run_scan uses.
        let found: Vec<String> = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !(e.file_type().is_dir() && is_opaque_package(e.path())))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_image(e.path()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            found,
            vec!["holiday.jpg".to_string()],
            "only the real photo may be offered; package contents are not the user's photos"
        );
    }

    #[test]
    fn ordinary_folders_are_left_alone() {
        for name in ["Photos", "Tokyo 2024", "app", "library", "my.photos"] {
            assert!(
                !is_opaque_package(Path::new("/Users/x").join(name).as_path()),
                "{name} is a normal folder and must still be scanned"
            );
        }
    }

    fn sample_record(path: &str) -> Record {
        Record {
            photo: Photo {
                path: path.into(),
                name: "a.jpg".into(),
                size: 10,
                width: 4,
                height: 3,
                taken: 1,
                blur: Some(900.0),
                measurements: badshot::Measurements::default(),
                bad_shot: badshot::BadShot::default(),
                preview: path.into(),
                thumb: None,
                library: false,
                missing: false,
                preview_dims: None,
                lat: None,
                lon: None,
                format: "JPG".into(),
                device: None,
                kind: None,
            },
            sha: Some("abc".into()),
            phash: Some(42),
        }
    }

    fn cache_with(version: u32) -> ScanCache {
        let mut files = HashMap::new();
        files.insert(
            "/p/a.jpg".to_string(),
            CachedFile {
                mtime: 7,
                size: 10,
                record: sample_record("/p/a.jpg"),
            },
        );
        ScanCache { version, files }
    }

    #[test]
    fn a_cache_written_now_is_read_back() {
        let dir = temp_dir("cache-ok");
        let file = dir.join("c.json");
        std::fs::write(
            &file,
            serde_json::to_vec(&cache_with(SCAN_CACHE_VERSION)).unwrap(),
        )
        .unwrap();

        let back = load_scan_cache(Some(&file));
        let hit = back
            .files
            .get("/p/a.jpg")
            .expect("entry survives the round trip");
        assert_eq!(hit.mtime, 7);
        assert_eq!(hit.record.phash, Some(42));
    }

    #[test]
    fn a_cache_from_another_version_is_discarded() {
        let dir = temp_dir("cache-version");
        let file = dir.join("c.json");
        std::fs::write(
            &file,
            serde_json::to_vec(&cache_with(SCAN_CACHE_VERSION + 1)).unwrap(),
        )
        .unwrap();

        assert!(
            load_scan_cache(Some(&file)).files.is_empty(),
            "analysis may have changed meaning; old entries must not be trusted"
        );
    }

    #[test]
    fn a_damaged_cache_is_ignored_rather_than_fatal() {
        let dir = temp_dir("cache-damaged");
        let file = dir.join("c.json");
        std::fs::write(&file, b"{ not json").unwrap();
        assert!(load_scan_cache(Some(&file)).files.is_empty());
        assert!(load_scan_cache(None).files.is_empty());
    }

    #[test]
    fn a_stamp_notices_an_edited_file() {
        let dir = temp_dir("cache-stamp");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"one").unwrap();
        let first = file_stamp(&file).expect("a written file has a stamp");
        std::fs::write(&file, b"one and a half, which is longer").unwrap();
        let second = file_stamp(&file).expect("still there");
        assert_ne!(
            first, second,
            "a changed file must not reuse cached analysis"
        );
    }
}

#[cfg(test)]
mod heic_tests {
    use super::*;
    use image::GenericImageView;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// The HEIC path must produce the same perceptual fingerprint and a comparable
    /// sharpness score as the JPEG encoding of the very same picture, otherwise
    /// iPhone photos would never group with their exports.
    /// EXIF keeps a position as unsigned degrees/minutes/seconds in one tag and the
    /// hemisphere letter in another, so both halves have to be combined correctly. The
    /// fixture's coordinates are known exactly, which is what makes an arithmetic slip
    /// or a dropped sign visible here rather than as a country lighting up wrongly.
    #[test]
    fn reads_gps_position_from_exif() {
        let (lat, lon) =
            exif_facts(&fixture("geotagged-paris.jpg")).coords.expect("the fixture carries GPS tags");
        assert!((lat - 48.8566).abs() < 1e-4, "latitude read as {lat}");
        assert!((lon - 2.3522).abs() < 1e-4, "longitude read as {lon}");
    }

    /// Most photographs carry no position at all, and that has to read as absence. The
    /// failure mode worth guarding is returning (0, 0) instead: that is a real point in
    /// the Gulf of Guinea, and it would draw a visit that never happened.
    #[test]
    fn a_photo_without_gps_tags_has_no_position() {
        assert_eq!(exif_facts(&fixture("portrait.jpg")).coords, None);
    }

    /// A portrait frame is stored landscape with a tag saying "turn me". Skimrr must
    /// report and measure the upright picture, not the sensor's view of it.
    #[test]
    fn portrait_photo_is_analysed_upright() {
        let path = fixture("portrait.jpg");
        assert_eq!(exif_orientation(&path), 6, "fixture should carry the tag");

        let stored = image::open(&path).unwrap();
        assert_eq!(stored.dimensions(), (800, 600), "stored sensor-side up");

        let analysis = analyze_file(&path);
        assert_eq!(
            analysis.dims,
            Some((600, 800)),
            "the reported picture must be taller than it is wide"
        );

        let photo = photo_meta(&path, &analysis, path.to_string_lossy().into_owned());
        assert!(photo.height > photo.width, "portrait must read as portrait");
    }

    /// The upright picture is what gets fingerprinted, so a rotated copy of the same
    /// shot groups with the original instead of looking like a different photo.
    #[test]
    fn rotation_is_applied_before_fingerprinting() {
        let upright = image::open(fixture("portrait.jpg")).unwrap().rotate90();
        let analysed = apply_orientation(image::open(fixture("portrait.jpg")).unwrap(), 6);
        assert_eq!(analysed.dimensions(), upright.dimensions());

        let dist = (hash_of(&analysed.to_luma8()) ^ hash_of(&upright.to_luma8())).count_ones();
        assert_eq!(dist, 0, "same picture must yield the same fingerprint");

        // And an unrotated read really would disagree, which is the bug being guarded.
        let raw_read = image::open(fixture("portrait.jpg")).unwrap();
        let drift = (hash_of(&raw_read.to_luma8()) ^ hash_of(&upright.to_luma8())).count_ones();
        assert!(
            drift > 8,
            "sanity: ignoring the tag changes the fingerprint"
        );
    }

    /// Every EXIF orientation must move the marked corner where the standard says.
    #[test]
    fn all_eight_orientations_transform_as_specified() {
        // 4×2 image, only the top-left pixel is white.
        let mut src = image::RgbImage::new(4, 2);
        src.put_pixel(0, 0, image::Rgb([255, 255, 255]));
        let img = image::DynamicImage::ImageRgb8(src);

        // (orientation, expected size, expected position of the white pixel)
        let cases = [
            (1, (4, 2), (0, 0)),
            (2, (4, 2), (3, 0)),
            (3, (4, 2), (3, 1)),
            (4, (4, 2), (0, 1)),
            (5, (2, 4), (0, 0)),
            (6, (2, 4), (1, 0)),
            (7, (2, 4), (1, 3)),
            (8, (2, 4), (0, 3)),
        ];
        for (orientation, size, (mx, my)) in cases {
            let out = apply_orientation(img.clone(), orientation).to_rgb8();
            assert_eq!(out.dimensions(), size, "size for orientation {orientation}");
            assert_eq!(
                out.get_pixel(mx, my)[0],
                255,
                "marker position for orientation {orientation}"
            );
            assert_eq!(
                oriented_dims((4, 2), orientation),
                size,
                "reported dims for orientation {orientation}"
            );
        }
    }

    /// Sony ARW: the file is skipped entirely unless the extension is known, and
    /// its picture only becomes readable through the JPEG rendition it embeds.
    #[test]
    fn arw_is_scanned_and_analysed() {
        let path = fixture("sample.arw");
        assert!(is_image(&path), "ARW must be picked up by the scanner");
        assert!(is_raw(&path));

        let analysis = analyze_file(&path);
        assert!(analysis.phash.is_some(), "ARW needs a perceptual hash");
        assert!(analysis.blur.is_some(), "ARW needs a sharpness score");
        assert!(
            analysis.preview_jpeg.is_some(),
            "ARW needs a cached rendition, no webview can display the raw itself"
        );

        // Vendor IFDs carry the capture date even though the container is not
        // standard TIFF, so the generic EXIF reader cannot reach it.
        assert_eq!(analysis.taken, Some(1_524_923_372));

        let photo = photo_meta(&path, &analysis, "preview.jpg".into());
        assert_eq!(photo.kind.as_deref(), Some("ARW"));
    }

    /// A raw and its JPEG export are the same picture: they must group, and the raw
    /// must be the one suggested for keeping.
    #[test]
    fn raw_is_preferred_over_its_jpeg_export() {
        let raw_path = fixture("sample.arw");
        let analysis = analyze_file(&raw_path);
        let raw = Record {
            photo: photo_meta(&raw_path, &analysis, "p.jpg".into()),
            sha: Some("raw".into()),
            phash: analysis.phash,
        };
        // Same picture exported to JPEG: identical fingerprint, larger reported size.
        let mut export = raw.clone();
        export.photo.path = "export.jpg".into();
        export.photo.name = "export.jpg".into();
        export.photo.kind = None;
        export.photo.width = 6000;
        export.photo.height = 4000;
        export.sha = Some("jpeg".into());

        let view = compute_view(&[raw, export], 6);
        assert_eq!(
            view.groups.len(),
            1,
            "the raw and its export are one picture"
        );
        let group = &view.groups[0];
        let keeper = &view.photos[group.indices[group.suggested]];
        assert_eq!(
            keeper.kind.as_deref(),
            Some("ARW"),
            "the raw original must win over its JPEG export"
        );
    }

    #[test]
    fn heic_matches_its_jpeg_twin() {
        let heic = decode_heic(&fixture("sharp.heic"))
            .expect("HEIC should decode")
            .to_luma8();
        let jpeg = image::open(fixture("sharp.jpg")).unwrap().to_luma8();

        assert_eq!(heic.dimensions(), jpeg.dimensions(), "dimensions differ");

        let dist = (hash_of(&heic) ^ hash_of(&jpeg)).count_ones();
        assert!(dist <= 4, "perceptual distance {dist} too large");

        let (bh, bj) = (sharpness(&heic), sharpness(&jpeg));
        let ratio = bh / bj;
        assert!(
            (0.5..2.0).contains(&ratio),
            "sharpness scores diverge: heic {bh:.1} vs jpeg {bj:.1}"
        );
    }

    #[test]
    fn blurry_heic_scores_far_below_sharp_one() {
        let sharp = decode_heic(&fixture("sharp.heic")).unwrap().to_luma8();
        let blurry = decode_heic(&fixture("blurry.heic")).unwrap().to_luma8();
        assert!(
            sharpness(&blurry) < sharpness(&sharp) / 5.0,
            "blur detection must work on HEIC too"
        );
    }

    /// End-to-end over the real pipeline: an iPhone HEIC, its JPEG export, a byte
    /// copy of the HEIC and a blurry HEIC. The three renditions of the same picture
    /// must land in one group, and the blurry one must be scored as such.
    #[test]
    fn heic_pipeline_groups_with_its_jpeg_export() {
        let files = [
            fixture("sharp.heic"),
            fixture("sharp.jpg"),
            fixture("blurry.heic"),
        ];
        let mut records: Vec<Record> = files
            .iter()
            .map(|path| {
                let analysis = analyze_file(path);
                let photo = photo_meta(path, &analysis, path.to_string_lossy().into_owned());
                Record {
                    photo,
                    sha: Some(hash_file(path).unwrap()),
                    phash: analysis.phash,
                }
            })
            .collect();
        // A byte-identical copy of the HEIC, as a second import of the same shot.
        let mut copy = records[0].clone();
        copy.photo.path = "copy.heic".into();
        records.push(copy);

        let view = compute_view(&records, 6);
        assert_eq!(view.groups.len(), 1, "every file is the same picture");
        let group = &view.groups[0];
        assert_eq!(group.indices.len(), 4);
        assert!(
            group
                .indices
                .iter()
                .any(|&i| view.photos[i].name == "sharp.jpg"),
            "the JPEG export must group with the HEIC it came from"
        );

        // Same resolution across the group, so the blurry frame must not be the keeper.
        let keeper = &view.photos[group.indices[group.suggested]];
        assert_ne!(
            keeper.name, "blurry.heic",
            "must never suggest keeping the blurry one"
        );

        let blurry = view
            .photos
            .iter()
            .find(|p| p.name == "blurry.heic")
            .unwrap();
        let sharp = view.photos.iter().find(|p| p.name == "sharp.heic").unwrap();
        assert!(blurry.blur.unwrap() < sharp.blur.unwrap() / 5.0);
    }

    #[test]
    fn heic_goes_through_the_normal_analysis_path() {
        let analysis = analyze_file(&fixture("sharp.heic"));
        assert!(analysis.phash.is_some(), "HEIC needs a perceptual hash");
        assert!(analysis.blur.is_some(), "HEIC needs a sharpness score");
        assert_eq!(analysis.dims, Some((600, 450)));
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;

    /// Same technique as `video::tests`: a short synthetic clip built with the
    /// `ffmpeg` CLI (resolved via `PATH`), independent of the sidecar-resolution path
    /// `analyze_video` itself uses — here it's given the CLI's location directly.
    fn synthetic_clip(dir: &Path) -> Option<PathBuf> {
        let out = dir.join("clip.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=10:duration=3", "-pix_fmt",
                "yuv420p",
            ])
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(out)
    }

    #[test]
    fn a_video_is_analysed_like_a_photo_would_be() {
        let dir = std::env::temp_dir().join(format!("skimrr-analyze-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(clip) = synthetic_clip(&dir) else {
            eprintln!("skipping: no working `ffmpeg` CLI available");
            return;
        };

        let analysis = analyze_video(Some(Path::new("ffmpeg")), &clip);
        assert!(analysis.phash.is_some(), "a moving test pattern must yield a fingerprint");
        assert!(analysis.blur.is_some(), "the median frame must yield a sharpness score");
        assert_eq!(analysis.dims, Some((64, 48)));
        assert!(analysis.preview_jpeg.is_some(), "a preview must be cached, same as HEIC/raw");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_sidecar_is_treated_like_an_undecodable_file() {
        let analysis = analyze_video(None, Path::new("/no/such/file.mp4"));
        assert!(analysis.phash.is_none());
        assert!(analysis.blur.is_none());
    }

    #[test]
    fn video_extensions_are_recognised_and_dont_overlap_images() {
        for ext in VIDEO_EXTS {
            assert!(is_video(Path::new(&format!("clip.{ext}"))));
            assert!(!is_image(Path::new(&format!("clip.{ext}"))), "{ext} must not double as an image extension");
        }
    }
}

#[doc(hidden)]
pub fn debug_largest_jpeg(data: &[u8]) -> Option<usize> {
    largest_embedded_jpeg(data).map(|b| b.len())
}

