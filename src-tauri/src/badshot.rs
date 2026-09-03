//! Why a photograph is probably a bad shot, and how sure we are.
//!
//! Four findings for now — blur, closed eyes, under- and over-exposure — each scored
//! independently so one photograph can carry several, and each gated hard enough that
//! a category stays worth opening. The bar throughout is precision over recall: a user
//! who finds five keepers in a list of a hundred stops trusting the list, whereas one
//! who never sees a borderline frame merely deletes it by hand later.
//!
//! The work is arranged as a cascade, cheapest first:
//!
//! ```text
//! decoded frame (already in memory, already downscaled)
//!   ├── exposure          histogram, microseconds
//!   ├── sharpness         tiled Laplacian, milliseconds
//!   └── faces             a 233 KB CNN, ~120 ms — only when a model is loaded
//!         └── eye state   only for faces large and frontal enough to judge
//! ```
//!
//! Nothing here decodes an image: callers hand in the frame the scan already decoded
//! for its fingerprint, so adding these findings costs no extra read.

use image::{GrayImage, RgbImage};
use serde::{Deserialize, Serialize};

/// What the scan concluded about one photograph.
///
/// Every field is a confidence in `0..=1`, and `None` means "not judged" rather than
/// "fine": exposure is always measured, blur needs a decodable frame, and closed eyes
/// need a face big enough to be worth an opinion. The distinction matters in the UI,
/// where a missing verdict must not read as a clean bill of health.
///
/// Kept deliberately flat and additive. Composition, cut-off subjects, red eye and
/// expression are all plausible later members, and each should arrive as one more
/// optional field rather than as a second structure alongside this one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BadShot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_eyes: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underexposed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overexposed: Option<f32>,
    /// How many faces were found, and how many carry the closed-eye finding. Kept so
    /// the viewer can say "1 face of 3" rather than a bare percentage, which reads very
    /// differently to someone deciding whether to delete a group photograph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faces: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faces_closed: Option<u16>,
}

/// A finding is reported at or above this confidence. Below it the photograph is left
/// out entirely rather than shown faintly: a category the user has to second-guess is
/// worse than a category that occasionally misses something.
pub const REPORT: f32 = 0.5;

impl BadShot {
    /// Whether anything at all was found worth reporting.
    pub fn any(&self) -> bool {
        [self.blur, self.closed_eyes, self.underexposed, self.overexposed]
            .iter()
            .any(|c| c.is_some_and(|v| v >= REPORT))
    }
}

/// A luminance histogram, and the few statistics worth drawing from it.
///
/// Built once and shared by both exposure findings, because a photograph that is
/// clipped at one end is very often stretched at the other and the two questions read
/// the same 256 numbers.
#[derive(Debug, Clone)]
pub struct Histogram {
    bins: [u32; 256],
    total: u32,
}

impl Histogram {
    pub fn of(gray: &GrayImage) -> Self {
        let mut bins = [0u32; 256];
        for p in gray.pixels() {
            bins[p.0[0] as usize] += 1;
        }
        Histogram {
            bins,
            total: gray.width() * gray.height(),
        }
    }

    /// Share of the frame at or below `level`.
    fn below(&self, level: u8) -> f32 {
        let n: u32 = self.bins[..=level as usize].iter().sum();
        n as f32 / self.total.max(1) as f32
    }

    /// Share of the frame at or above `level`.
    fn above(&self, level: u8) -> f32 {
        let n: u32 = self.bins[level as usize..].iter().sum();
        n as f32 / self.total.max(1) as f32
    }

    /// The luminance below which `q` of the frame falls.
    fn quantile(&self, q: f32) -> u8 {
        let target = (self.total as f32 * q) as u32;
        let mut seen = 0u32;
        for (level, count) in self.bins.iter().enumerate() {
            seen += count;
            if seen >= target {
                return level as u8;
            }
        }
        255
    }

    /// The span holding the middle 90% of the frame — a contrast reading that a few
    /// specular highlights or one black corner cannot move.
    fn spread(&self) -> u8 {
        self.quantile(0.95).saturating_sub(self.quantile(0.05))
    }
}

/// How badly the frame is under- or over-exposed, each in `0..=1`.
///
/// Deliberately not "x% of pixels are black". A night scene, a portrait on a dark
/// ground, a silhouette and a photograph of a lit window are all mostly black and all
/// perfectly good; what separates them from a genuine failure is whether anything in
/// the frame is *correctly* exposed. So the test is two-sided: a frame is only called
/// under-exposed when it is both crushed at the bottom **and** has nothing holding a
/// usable midtone, and it is excused entirely when its contrast is healthy.
///
/// The same reasoning mirrored for the top end, where the failure is worse: clipped
/// highlights carry no recoverable detail at all, so the bar there is a little lower.
pub fn exposure(hist: &Histogram) -> (Option<f32>, Option<f32>) {
    /// Below this a pixel holds no usable detail on any ordinary display.
    const CRUSHED: u8 = 16;
    /// Above this a pixel is blown; 255 exactly is clipped outright.
    const BLOWN: u8 = 244;
    /// A frame with this much of its area in the midtones has a subject that is
    /// correctly exposed, whatever the rest of the histogram does.
    const MIDTONE_RESCUE: f32 = 0.12;
    /// Contrast wide enough that the photograph is using its range deliberately.
    const HEALTHY_SPREAD: u8 = 90;

    let midtones = hist.above(60) - hist.above(190);
    let spread = hist.spread();

    // A deliberate low-key frame keeps midtones and contrast; a failed one has neither.
    let under = {
        let crushed = hist.below(CRUSHED);
        let rescued = midtones >= MIDTONE_RESCUE || spread >= HEALTHY_SPREAD;
        // Ramp from a half of the frame crushed to nearly all of it, so the score says
        // how far gone it is rather than merely tripping a threshold.
        let severity = ((crushed - 0.5) / 0.4).clamp(0.0, 1.0);
        (!rescued && severity > 0.0).then_some(severity)
    };

    let over = {
        let blown = hist.above(BLOWN);
        let clipped = hist.above(254);
        let rescued = midtones >= MIDTONE_RESCUE && clipped < 0.1;
        // Blown highlights are unrecoverable, so this starts biting earlier than the
        // shadow case: a quarter of the frame gone white is already a failure.
        let severity = ((blown - 0.25) / 0.35).clamp(0.0, 1.0);
        (!rescued && severity > 0.0).then_some(severity)
    };

    (under, over)
}

/// A rectangle in pixels of the frame it was found in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn area(&self) -> f32 {
        (self.w * self.h).max(0.0)
    }

    fn intersection(&self, other: &Rect) -> f32 {
        let x = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let y = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        (x.max(0.0)) * (y.max(0.0))
    }

    fn iou(&self, other: &Rect) -> f32 {
        let i = self.intersection(other);
        let u = self.area() + other.area() - i;
        if u <= 0.0 { 0.0 } else { i / u }
    }
}

/// One detected face: where it is, how sure the detector is, and the five landmarks it
/// returns — right eye, left eye, nose, right mouth corner, left mouth corner, in that
/// order. The eyes are what makes a separate landmark model unnecessary.
#[derive(Debug, Clone)]
pub struct Face {
    pub rect: Rect,
    pub score: f32,
    pub landmarks: [(f32, f32); 5],
}

impl Face {
    pub fn right_eye(&self) -> (f32, f32) {
        self.landmarks[0]
    }

    pub fn left_eye(&self) -> (f32, f32) {
        self.landmarks[1]
    }

    /// How square-on the face is, from the two eyes and the nose. A profile puts the
    /// nose far off the midpoint between the eyes relative to their separation; a
    /// frontal face keeps it near. Returns 1 for square-on, falling to 0.
    ///
    /// Used as a gate rather than a measurement: an eye seen at a steep angle is
    /// foreshortened to a slit whether it is open or shut, and calling that closed is
    /// the single easiest way to fill this list with good photographs.
    pub fn frontality(&self) -> f32 {
        let (rx, ry) = self.right_eye();
        let (lx, ly) = self.left_eye();
        let (nx, ny) = self.landmarks[2];
        let eye_span = ((lx - rx).powi(2) + (ly - ry).powi(2)).sqrt();
        if eye_span <= f32::EPSILON {
            return 0.0;
        }
        let (mx, my) = ((rx + lx) / 2.0, (ry + ly) / 2.0);
        let offset = ((nx - mx).powi(2) + (ny - my).powi(2)).sqrt();
        // A frontal face's nose sits under the eye midpoint by roughly half the eye
        // separation; beyond one and a half of it the head is turned far enough that
        // the near eye is no longer readable.
        (1.0 - ((offset / eye_span) - 0.5).max(0.0) / 1.0).clamp(0.0, 1.0)
    }
}

/// Removes overlapping detections, keeping the most confident of each cluster.
///
/// The graph stops at raw anchors — `NonMaxSuppression` is not an operator the runtime
/// implements — so the suppression happens here. That is no loss: at these detection
/// counts it costs microseconds, and keeping it in Rust makes the threshold visible
/// and testable rather than baked into a model file.
pub fn non_max_suppression(mut faces: Vec<Face>, iou_limit: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<Face> = Vec::new();
    for face in faces {
        if kept.iter().all(|k| k.rect.iou(&face.rect) < iou_limit) {
            kept.push(face);
        }
    }
    kept
}

/// Converts a frame to the greyscale the sharpness and histogram passes both read.
pub fn luma(rgb: &RgbImage) -> GrayImage {
    image::imageops::grayscale(rgb)
}

// ---------------------------------------------------------------------------------
// Face detection
// ---------------------------------------------------------------------------------

/// The side of the square the detector is fed. Fixed by the exported graph: this build
/// of YuNet declares 640×640 and the runtime checks the declared shape, so there is no
/// smaller size to trade accuracy for speed at.
const DETECT_SIDE: u32 = 640;

/// Strides of the three feature maps, and therefore the three anchor grids.
const STRIDES: [u32; 3] = [8, 16, 32];

/// Below this a detection is noise. YuNet's own demo uses 0.9; this is stricter because
/// a spurious face here does not merely draw a box, it invites an eye verdict on a
/// patch of wallpaper.
const MIN_FACE_SCORE: f32 = 0.9;

/// Two boxes overlapping by more than this are the same face.
const NMS_IOU: f32 = 0.3;

/// Where the frame lands inside the square the detector is fed, so detections can be
/// mapped back. Aspect ratio is preserved and the remainder padded rather than the
/// frame being squashed to a square: a stretched face is a face the detector was not
/// trained on.
struct Letterbox {
    scale: f32,
    dx: f32,
    dy: f32,
}

impl Letterbox {
    fn fit(w: u32, h: u32) -> Self {
        let scale = (DETECT_SIDE as f32 / w as f32).min(DETECT_SIDE as f32 / h as f32);
        Letterbox {
            scale,
            dx: (DETECT_SIDE as f32 - w as f32 * scale) / 2.0,
            dy: (DETECT_SIDE as f32 - h as f32 * scale) / 2.0,
        }
    }

    fn back(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.dx) / self.scale, (y - self.dy) / self.scale)
    }
}

/// Builds the detector's input plane: the frame scaled to fit, centred, padded, and
/// laid out channel-first in BGR.
///
/// BGR and raw 0–255, deliberately: this graph was trained and is used through OpenCV,
/// whose blob has no mean subtraction and no scaling. Feeding it normalised RGB does
/// not fail — it simply finds nothing, which is the kind of mistake that looks like a
/// working feature with no faces in the library.
fn detector_input(rgb: &RgbImage) -> (Vec<f32>, Letterbox) {
    let (w, h) = rgb.dimensions();
    let fit = Letterbox::fit(w, h);
    let scaled = image::imageops::resize(
        rgb,
        ((w as f32 * fit.scale).round() as u32).max(1),
        ((h as f32 * fit.scale).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    );

    let side = DETECT_SIDE as usize;
    let mut data = vec![0f32; 3 * side * side];
    let (ox, oy) = (fit.dx.round() as u32, fit.dy.round() as u32);
    for y in 0..scaled.height() {
        for x in 0..scaled.width() {
            let px = scaled.get_pixel(x, y).0;
            let (tx, ty) = ((x + ox) as usize, (y + oy) as usize);
            if tx >= side || ty >= side {
                continue;
            }
            for (plane, channel) in [2usize, 1, 0].into_iter().enumerate() {
                data[plane * side * side + ty * side + tx] = px[channel] as f32;
            }
        }
    }
    (data, fit)
}

/// Turns the model's raw anchor outputs into faces in the original frame's pixels.
///
/// Each feature map cell is one anchor. The box arrives as an offset from its cell
/// centre and a log-scale size, and the landmarks as offsets from the same cell, all in
/// stride units — which is why every value is multiplied back by its own stride.
fn decode(
    outputs: &std::collections::HashMap<String, candle_core::Tensor>,
    fit: &Letterbox,
) -> Option<Vec<Face>> {
    let mut faces = Vec::new();
    for stride in STRIDES {
        let grid = (DETECT_SIDE / stride) as usize;
        let cls = outputs.get(&format!("cls_{stride}"))?.flatten_all().ok()?.to_vec1::<f32>().ok()?;
        let obj = outputs.get(&format!("obj_{stride}"))?.flatten_all().ok()?.to_vec1::<f32>().ok()?;
        let box_ = outputs.get(&format!("bbox_{stride}"))?.flatten_all().ok()?.to_vec1::<f32>().ok()?;
        let kps = outputs.get(&format!("kps_{stride}"))?.flatten_all().ok()?.to_vec1::<f32>().ok()?;

        for row in 0..grid {
            for col in 0..grid {
                let i = row * grid + col;
                // Two heads agree or they do not; the geometric mean of the pair is
                // what YuNet's own post-processing uses, and it is far less trusting
                // than either head alone.
                let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
                if score < MIN_FACE_SCORE {
                    continue;
                }
                let (cx, cy) = (
                    (col as f32 + box_[i * 4]) * stride as f32,
                    (row as f32 + box_[i * 4 + 1]) * stride as f32,
                );
                let (bw, bh) = (
                    box_[i * 4 + 2].exp() * stride as f32,
                    box_[i * 4 + 3].exp() * stride as f32,
                );
                let (x0, y0) = fit.back(cx - bw / 2.0, cy - bh / 2.0);
                let (x1, y1) = fit.back(cx + bw / 2.0, cy + bh / 2.0);

                let mut landmarks = [(0.0f32, 0.0f32); 5];
                for (k, slot) in landmarks.iter_mut().enumerate() {
                    *slot = fit.back(
                        (col as f32 + kps[i * 10 + k * 2]) * stride as f32,
                        (row as f32 + kps[i * 10 + k * 2 + 1]) * stride as f32,
                    );
                }

                faces.push(Face {
                    rect: Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 },
                    score,
                    landmarks,
                });
            }
        }
    }
    Some(non_max_suppression(faces, NMS_IOU))
}

/// Runs the face detector over one frame.
///
/// Returns `None` when the model could not be evaluated at all, which the caller must
/// keep distinct from `Some(vec![])`: no model is not the same as no faces, and only
/// the second means a photograph can be judged on its whole frame alone.
pub fn detect_faces(
    model: &candle_onnx::onnx::ModelProto,
    rgb: &RgbImage,
) -> Option<Vec<Face>> {
    use candle_core::{Device, Tensor};

    let (data, fit) = detector_input(rgb);
    let side = DETECT_SIDE as usize;
    let input = Tensor::from_vec(data, (1, 3, side, side), &Device::Cpu).ok()?;
    let name = model.graph.as_ref()?.input.first()?.name.clone();
    let mut inputs = std::collections::HashMap::new();
    inputs.insert(name, input);
    let outputs = candle_onnx::simple_eval(model, inputs).ok()?;
    decode(&outputs, &fit)
}

// ---------------------------------------------------------------------------------
// Measurements: what is cached per photograph, before any threshold is known
// ---------------------------------------------------------------------------------

/// The readings a single photograph yields, independent of any folder it sits in.
///
/// Split from `BadShot` on purpose. These are measurements and never change; the
/// verdict built from them depends on the blur threshold, which is a property of the
/// folder and moves under the user's slider. Caching the first and recomputing the
/// second is what lets the slider stay instant without re-reading a single file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Measurements {
    /// Sharpness of the sharpest face found, in the same units as the whole-frame
    /// score. `None` when no face was found or no detector was loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_sharpness: Option<f64>,
    /// Share of the frame's tiles that are markedly softer than its own best. High on
    /// a uniformly soft frame, low on a sharp subject against a blurred background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_share: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faces: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faces_closed: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_eyes: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underexposed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overexposed: Option<f32>,
}

/// How much of the frame is soft relative to its own sharpest region.
///
/// This is what separates a failed photograph from a deliberate one when no face is
/// available to anchor the judgement. A portrait at f/1.4 has a small sharp island in a
/// soft sea; a missed focus has no island at all. Measuring each tile against the
/// frame's own best rather than against an absolute makes the reading survive haze,
/// low light and flat subjects, which an absolute number does not.
pub fn soft_share(gray: &GrayImage) -> Option<f32> {
    /// A tile this far below the frame's best is soft. Generous: the point is to find
    /// frames with *no* sharp region, not to grade the falloff.
    const SOFT_RATIO: f64 = 0.35;
    const TILE: u32 = 64;

    let (w, h) = gray.dimensions();
    if w < TILE || h < TILE {
        return None;
    }
    let mut tiles = Vec::new();
    let mut y = 0;
    while y + TILE <= h {
        let mut x = 0;
        while x + TILE <= w {
            let crop = image::imageops::crop_imm(gray, x, y, TILE, TILE).to_image();
            tiles.push(crate::sharpness(&crop));
            x += TILE;
        }
        y += TILE;
    }
    if tiles.len() < 4 {
        return None;
    }
    let best = tiles.iter().cloned().fold(0.0f64, f64::max);
    if best <= f64::EPSILON {
        return Some(1.0);
    }
    let soft = tiles.iter().filter(|t| **t < best * SOFT_RATIO).count();
    Some(soft as f32 / tiles.len() as f32)
}

/// Sharpness of the largest face, measured with the same tiled reading the whole frame
/// gets so the two numbers are comparable.
///
/// The largest rather than the average: in a group photograph the subject is the person
/// nearest the camera, and a soft face three rows back is depth of field working as
/// intended, not a failure.
pub fn face_sharpness(gray: &GrayImage, faces: &[Face]) -> Option<f64> {
    let (w, h) = gray.dimensions();
    let biggest = faces
        .iter()
        .max_by(|a, b| a.rect.area().partial_cmp(&b.rect.area()).unwrap_or(std::cmp::Ordering::Equal))?;
    let x = biggest.rect.x.max(0.0) as u32;
    let y = biggest.rect.y.max(0.0) as u32;
    let cw = (biggest.rect.w as u32).min(w.saturating_sub(x));
    let ch = (biggest.rect.h as u32).min(h.saturating_sub(y));
    // Too small to tile is too small to judge; the whole-frame reading stands alone.
    if cw < 64 || ch < 64 {
        return None;
    }
    Some(crate::sharpness(&image::imageops::crop_imm(gray, x, y, cw, ch).to_image()))
}

/// Whether the photograph is blurred, given the folder's own threshold.
///
/// The whole-frame score alone was the previous answer and it is kept as the entry
/// condition — nothing is called blurred that the existing reading considers sharp.
/// What is new is the pair of escapes, both aimed at the same failure: a photograph
/// whose subject is sharp and whose background is not.
///
/// ```text
/// global soft + face sharp   -> keep,  shallow depth of field
/// global soft + face soft    -> blur,  the subject itself missed
/// global soft + no face + an island of sharpness -> keep, probably deliberate
/// global soft + no face + uniformly soft         -> blur
/// ```
pub fn blur_verdict(global: Option<f64>, cuts: Cuts, m: &Measurements) -> Option<f32> {
    let global = global?;
    if cuts.blur <= 0.0 || global >= cuts.blur {
        return None;
    }
    // How soft it is, against the folder's middle rather than against the cut it just
    // crossed — see `Cuts` for why that distinction is the whole point.
    let severity = Cuts::severity(cuts.blur, cuts.blur_median, global);

    if let Some(face) = m.face_sharpness {
        /* Judged against faces, never against the frame. Measured on real photographs,
           a face scores far below the scene around it — 0.37 to 0.58 where the whole
           frame scored 2.0 to 3.2 — because skin is smooth and carries little of the
           high-frequency detail this reading counts, while foliage and fabric carry a
           great deal. Comparing the two against one threshold marked every portrait
           blurred, which is the exact failure this feature exists to avoid. So faces
           get their own cut, drawn from the folder's own faces. */
        if cuts.face <= 0.0 || face >= cuts.face {
            return None;
        }
        let face_severity = Cuts::severity(cuts.face, cuts.face_median, face);
        return Some(severity.max(face_severity).min(1.0));
    }

    match m.soft_share {
        // An island of sharpness somewhere in an otherwise soft frame: bokeh, a macro,
        // a long lens. Not ours to call a failure.
        Some(share) if share < 0.7 => None,
        // Uniformly soft, and no face to appeal to.
        Some(share) => Some((severity * share).clamp(0.0, 1.0)),
        // No zone reading at all (a very small frame): fall back to the old behaviour.
        None => Some(severity),
    }
}

/// Assembles the verdict a photograph gets inside a particular folder.
/// Where each judgement cuts, and what it measures severity against.
///
/// Two different numbers on purpose, and conflating them was a real defect. The cut is a
/// percentile of the folder, so by construction it sits just above the folder's minimum:
/// measuring "how far below the cut" *against the cut itself* yields a few percent even
/// for the softest frame there is. Measured on a real folder — 31 photographs, softest
/// 0.24, cut 0.26, median 1.20 — the severity came out at 0.077 while `REPORT` asks for
/// 0.5. The category could not fire, on any folder, ever.
///
/// Severity is now measured against the median, a scale a genuinely soft frame can
/// actually travel: the same photograph scores 0.80. The cut still decides *whether* to
/// look; the median decides *how bad* it is.
#[derive(Debug, Clone, Copy)]
pub struct Cuts {
    /// Below this, a frame is a candidate. A low percentile of the folder's own readings.
    pub blur: f64,
    /// What severity is measured against. The folder's median.
    pub blur_median: f64,
    /// The same pair again, drawn from the folder's faces rather than its frames.
    pub face: f64,
    pub face_median: f64,
}

impl Cuts {
    /// Severity relative to a scale, falling back to the cut when no scale is known.
    fn severity(cut: f64, scale: f64, value: f64) -> f32 {
        let scale = if scale > value { scale } else { cut };
        if scale <= 0.0 {
            return 0.0;
        }
        (((scale - value) / scale).clamp(0.0, 1.0)) as f32
    }
}

pub fn verdict(global_sharpness: Option<f64>, cuts: Cuts, m: &Measurements) -> BadShot {
    BadShot {
        blur: blur_verdict(global_sharpness, cuts, m),
        closed_eyes: m.closed_eyes,
        underexposed: m.underexposed,
        overexposed: m.overexposed,
        faces: m.faces,
        faces_closed: m.faces_closed,
    }
}

// ---------------------------------------------------------------------------------
// Eye state
// ---------------------------------------------------------------------------------

/// Judges one eye crop, returning the probability that it is shut.
///
/// A trait rather than a function because this is the one place in the pipeline where a
/// small trained model clearly beats anything hand-written, and none that fits was
/// available: what is published for eye state is hobby-grade, unlicensed, or exported
/// as a vision transformer far heavier than this whole application's model budget. So
/// the seam is here, ready, and the classical estimator below stands in it meanwhile.
///
/// A model implementing this needs only to take a greyscale crop and return a
/// probability; nothing above it needs to change.
pub trait EyeJudge: Sync {
    fn closed_probability(&self, eye: &GrayImage) -> Option<f32>;
}

/// Eye openness from the shape of the dark region inside the crop.
///
/// An open eye shows the iris and pupil as a tall dark mass with brighter sclera to
/// either side; a shut one shows lashes as a thin horizontal line with skin above and
/// below. So the discriminator is the *vertical* extent of the darkest pixels relative
/// to the crop, which survives skin tone, lighting and eye colour better than any
/// absolute brightness would.
///
/// Honest about what this is: unvalidated against labelled data, because none is
/// present here. It is therefore gated to fire only on unambiguous cases, and its
/// output is capped well below certainty — the intent is that it never contradicts a
/// good photograph, not that it catches every blink.
pub struct DarkExtent;

impl EyeJudge for DarkExtent {
    fn closed_probability(&self, eye: &GrayImage) -> Option<f32> {
        let (w, h) = eye.dimensions();
        if w < 12 || h < 8 {
            return None;
        }
        /* What counts as dark, set between the crop's own extremes rather than at a
           fixed level or a fixed quantile. A quantile was the first attempt and is
           wrong: it assumes how much of the crop is dark, which is precisely the
           unknown — an open eye's iris covers a fifth of the crop and a shut eye's
           lashes a twentieth, so any fixed rank lands on skin for one of the two and
           calls the whole crop dark. The 2nd and 98th percentiles stand in for the
           extremes, being robust to a stray hot pixel without assuming any split. */
        let mut levels: Vec<u8> = eye.pixels().map(|p| p.0[0]).collect();
        levels.sort_unstable();
        let lo = levels[levels.len() / 50];
        let hi = levels[levels.len() * 49 / 50];
        // A crop with no contrast holds no eye worth reading — deep shadow, blown skin.
        if hi.saturating_sub(lo) < 25 {
            return None;
        }
        let cut = lo + ((hi - lo) as f32 * 0.35) as u8;

        let mut rows_with_dark = 0u32;
        for y in 0..h {
            let dark_in_row = (0..w).filter(|&x| eye.get_pixel(x, y).0[0] <= cut).count();
            // One or two stray pixels are noise; a real feature spans part of the row.
            if dark_in_row as f32 >= w as f32 * 0.12 {
                rows_with_dark += 1;
            }
        }
        let extent = rows_with_dark as f32 / h as f32;

        // Below a third of the crop's height the dark mass is a line, not an iris.
        // Above a half it is plainly an open eye. Between, no opinion is offered.
        let closed = ((0.38 - extent) / 0.12).clamp(0.0, 1.0);
        // Capped: this estimator is not good enough to assert certainty, and the cap is
        // what keeps a borderline reading out of the reported category entirely.
        Some(closed * 0.8)
    }
}

/// Whether enough faces in the frame have their eyes shut to call the photograph a
/// bad shot, and how many.
///
/// Every gate here exists to prevent one specific false positive, and each is checked
/// before any pixel of an eye is read:
///
/// - a face too small carries eyes a handful of pixels across, where the measurement
///   is meaningless;
/// - a face turned away shows a foreshortened eye that reads as shut whatever it is;
/// - a soft face gives a soft eye, and a blurred eyelid is indistinguishable from a
///   closed one.
pub fn closed_eyes(
    gray: &GrayImage,
    faces: &[Face],
    judge: &dyn EyeJudge,
) -> (Option<f32>, u16) {
    /// Eyes must be at least this far apart for their crops to hold real detail.
    const MIN_EYE_SPAN: f32 = 36.0;
    /// Below this the head is turned too far to read the near eye.
    const MIN_FRONTALITY: f32 = 0.75;
    /// Both eyes must agree this strongly before the face counts as shut.
    const BOTH_EYES: f32 = 0.55;

    let (w, h) = gray.dimensions();
    let mut worst: Option<f32> = None;
    let mut affected = 0u16;

    for face in faces {
        let (rx, ry) = face.right_eye();
        let (lx, ly) = face.left_eye();
        let span = ((lx - rx).powi(2) + (ly - ry).powi(2)).sqrt();
        if span < MIN_EYE_SPAN || face.frontality() < MIN_FRONTALITY {
            continue;
        }

        // A crop proportional to the eye separation, so it holds the same anatomy at
        // every face size: a little wider than the eye itself, and half as tall.
        let half_w = (span * 0.32).max(6.0);
        let half_h = (span * 0.20).max(4.0);
        let mut probabilities = Vec::new();
        for (ex, ey) in [(rx, ry), (lx, ly)] {
            let x0 = (ex - half_w).max(0.0) as u32;
            let y0 = (ey - half_h).max(0.0) as u32;
            let cw = ((half_w * 2.0) as u32).min(w.saturating_sub(x0));
            let ch = ((half_h * 2.0) as u32).min(h.saturating_sub(y0));
            if cw < 12 || ch < 8 {
                continue;
            }
            let crop = image::imageops::crop_imm(gray, x0, y0, cw, ch).to_image();
            if let Some(p) = judge.closed_probability(&crop) {
                probabilities.push(p);
            }
        }

        // Both eyes or nothing. One eye reading shut is far more often a highlight, a
        // strand of hair or a wink than a spoiled photograph.
        if probabilities.len() == 2 {
            let both = probabilities.iter().cloned().fold(f32::MAX, f32::min);
            if both >= BOTH_EYES {
                affected += 1;
                worst = Some(worst.map_or(both, |w: f32| w.max(both)));
            }
        }
    }

    (worst, affected)
}

// ---------------------------------------------------------------------------------
// The cascade
// ---------------------------------------------------------------------------------

/// Everything measurable about one already-decoded frame.
///
/// Ordered by cost, and each stage gates the next. Exposure reads a histogram and is
/// effectively free. The zone reading tiles the frame and costs milliseconds. Face
/// detection costs a hundred times that, so it runs last and only when a detector was
/// actually loaded — and the eye judgement runs only for the faces it returns.
///
/// The frame is borrowed, never opened: the scan has already decoded it to fingerprint
/// the photograph, and re-reading the file to answer a second question would double the
/// cost of the slowest part of a scan for no gain.
pub fn measure(
    rgb: &RgbImage,
    detector: Option<&candle_onnx::onnx::ModelProto>,
) -> Measurements {
    let gray = luma(rgb);
    let (underexposed, overexposed) = exposure(&Histogram::of(&gray));

    let mut m = Measurements {
        soft_share: soft_share(&gray),
        underexposed,
        overexposed,
        ..Default::default()
    };

    // A frame with no usable tones has no face to find and no eye to read; running a
    // CNN over it would only spend a tenth of a second confirming that.
    let hopeless = underexposed.is_some_and(|v| v > 0.9) || overexposed.is_some_and(|v| v > 0.9);
    let Some(model) = detector.filter(|_| !hopeless) else {
        return m;
    };
    let Some(faces) = detect_faces(model, rgb) else {
        return m;
    };

    m.faces = Some(faces.len() as u16);
    m.face_sharpness = face_sharpness(&gray, &faces);
    if !faces.is_empty() {
        let (closed, affected) = closed_eyes(&gray, &faces, &DarkExtent);
        m.closed_eyes = closed;
        m.faces_closed = Some(affected);
    }
    m
}

#[cfg(test)]
mod probe {
    use super::*;

    #[test]
    #[ignore = "probe"]
    fn survey() {
        let model = candle_onnx::read_file(std::env::var("PROBE_MODEL").unwrap()).unwrap();
        let dir = std::env::var("PROBE_DIR").unwrap();
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| {
                let e = e.to_string_lossy().to_lowercase();
                e == "jpg" || e == "jpeg" || e == "png"
            })).collect();
        paths.sort();
        for path in paths.iter().take(40) {
            let Ok(img) = image::open(path) else { continue };
            let rgb = img.to_rgb8();
            let t = std::time::Instant::now();
            let m = measure(&rgb, Some(&model));
            let r2 = |v: Option<f32>| v.map(|x| (x * 100.0).round() / 100.0);
            eprintln!(
                "PROBE {:26} {:>4}ms glob={:>6.2} visages={:?} netVis={:?} molles={:?} sous={:?} sur={:?} yeux={:?}",
                path.file_name().unwrap().to_string_lossy().chars().take(26).collect::<String>(),
                t.elapsed().as_millis(),
                crate::sharpness(&luma(&rgb)),
                m.faces,
                m.face_sharpness.map(|v| (v * 100.0).round() / 100.0),
                r2(m.soft_share), r2(m.underexposed), r2(m.overexposed), r2(m.closed_eyes),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb};

    /// A frame of one tone, for the histogram cases.
    fn flat(level: u8) -> GrayImage {
        GrayImage::from_pixel(64, 64, Luma([level]))
    }

    /// A frame that is mostly `dark` with `share` of it at `lit` — the shape of every
    /// photograph that is legitimately dark: a night street with lit windows, a stage,
    /// a face against a black ground.
    fn mostly_dark(dark: u8, lit: u8, share: f32) -> GrayImage {
        let mut g = GrayImage::from_pixel(100, 100, Luma([dark]));
        let rows = (100.0 * share) as u32;
        for y in 0..rows {
            for x in 0..100 {
                g.put_pixel(x, y, Luma([lit]));
            }
        }
        g
    }

    fn exposure_of(g: &GrayImage) -> (Option<f32>, Option<f32>) {
        exposure(&Histogram::of(g))
    }

    #[test]
    fn an_ordinary_frame_is_neither_under_nor_over_exposed() {
        let (under, over) = exposure_of(&flat(128));
        assert_eq!((under, over), (None, None));
    }

    #[test]
    fn a_frame_crushed_to_black_is_underexposed() {
        let (under, over) = exposure_of(&flat(4));
        assert!(under.is_some_and(|v| v > 0.9), "got {under:?}");
        assert_eq!(over, None);
    }

    /// The case the naive "x% of pixels are dark" rule gets wrong, and the reason this
    /// reading is two-sided: a night photograph is mostly black on purpose and is not a
    /// bad shot. What saves it is that something in it is correctly exposed.
    #[test]
    fn a_night_photo_with_a_lit_subject_is_not_underexposed() {
        let (under, _) = exposure_of(&mostly_dark(6, 140, 0.15));
        assert_eq!(under, None, "a lit subject rescues a dark frame");
    }

    /// And the same frame with nothing lit in it at all is a failure.
    #[test]
    fn a_dark_frame_with_nothing_exposed_is_underexposed() {
        let (under, _) = exposure_of(&mostly_dark(6, 20, 0.15));
        assert!(under.is_some(), "nothing in the frame is correctly exposed");
    }

    #[test]
    fn a_blown_frame_is_overexposed() {
        let (under, over) = exposure_of(&flat(255));
        assert_eq!(under, None);
        assert!(over.is_some_and(|v| v > 0.9), "got {over:?}");
    }

    /// A bright high-key photograph — snow, a white studio — keeps midtones and is not
    /// clipped everywhere, so it is left alone.
    #[test]
    fn a_bright_but_intact_frame_is_not_overexposed() {
        let (_, over) = exposure_of(&mostly_dark(210, 120, 0.3));
        assert_eq!(over, None);
    }

    fn measurements(face: Option<f64>, soft: Option<f32>) -> Measurements {
        Measurements {
            face_sharpness: face,
            soft_share: soft,
            ..Default::default()
        }
    }

    /// Cuts where the scale equals the cut — the shape these tests were written
    /// against, kept so their assertions still mean what they meant.
    fn cuts(blur: f64, face: f64) -> Cuts {
        Cuts { blur, blur_median: blur, face, face_median: face }
    }

    /// The defect these `Cuts` exist to fix, reproduced from the real numbers.
    ///
    /// A curated folder of 31 photographs: softest 0.24, fifth percentile 0.26, median
    /// 1.20. Two of them were deliberately soft. Under the old arithmetic the verdict
    /// came back at 0.077 against a reporting bar of 0.5, so the Bad Shot tab said
    /// "nothing here looks like a bad shot" — and would have said it on every folder,
    /// because a percentile cut always sits just above its own minimum.
    #[test]
    fn a_soft_frame_clears_the_reporting_bar_on_a_real_distribution() {
        let m = Measurements { soft_share: Some(0.95), ..Default::default() };
        let real = Cuts { blur: 0.26, blur_median: 1.20, face: 0.0, face_median: 0.0 };

        let v = blur_verdict(Some(0.24), real, &m);
        assert!(
            v.is_some_and(|c| c >= REPORT),
            "the softest photograph of the folder must be reported, got {v:?}"
        );

        // And the old arithmetic, kept here as the thing that must never come back.
        let conflated = Cuts { blur: 0.26, blur_median: 0.26, face: 0.0, face_median: 0.0 };
        assert!(
            blur_verdict(Some(0.24), conflated, &m).is_some_and(|c| c < REPORT),
            "measuring severity against the cut is what made the category silent"
        );

        // A frame just above the cut is still not a candidate: the gate has not moved.
        assert_eq!(blur_verdict(Some(0.27), real, &m), None);
    }

    #[test]
    fn a_sharp_frame_is_never_called_blurred() {
        let m = measurements(None, Some(0.9));
        assert_eq!(blur_verdict(Some(5.0), cuts(1.0, 1.0), &m), None);
    }

    /// The headline case: a portrait at a wide aperture is soft nearly everywhere and
    /// entirely correct. Its face is sharp, and that is what settles it.
    #[test]
    fn a_sharp_face_rescues_a_soft_frame() {
        let m = measurements(Some(0.9), Some(0.95));
        assert_eq!(
            blur_verdict(Some(0.4), cuts(1.0, 0.8), &m),
            None,
            "the subject is in focus, the background is meant to be soft"
        );
    }

    /// The mirror image, and the one worth catching: the frame is no worse than the
    /// one above, but the face itself missed focus.
    #[test]
    fn a_soft_face_in_a_soft_frame_is_a_bad_shot() {
        let m = measurements(Some(0.2), Some(0.95));
        let v = blur_verdict(Some(0.4), cuts(1.0, 0.8), &m);
        assert!(v.is_some_and(|c| c >= REPORT), "got {v:?}");
    }

    /// With no face to appeal to, an island of sharpness still means the softness was
    /// chosen — a macro, a long lens, a subject picked out of a crowd.
    #[test]
    fn an_island_of_sharpness_rescues_a_faceless_frame() {
        let m = measurements(None, Some(0.4));
        assert_eq!(blur_verdict(Some(0.4), cuts(1.0, 1.0), &m), None);
    }

    #[test]
    fn a_uniformly_soft_faceless_frame_is_a_bad_shot() {
        let m = measurements(None, Some(0.98));
        let v = blur_verdict(Some(0.2), cuts(1.0, 1.0), &m);
        assert!(v.is_some_and(|c| c >= REPORT), "got {v:?}");
    }

    /// Faces are judged against faces. A face scores far below the scene around it —
    /// measured at 0.4-0.6 against frames scoring 2-3 — because skin carries little
    /// fine detail. Sharing one threshold marked every portrait blurred.
    #[test]
    fn a_face_is_judged_against_the_face_threshold_not_the_frame_one() {
        let m = measurements(Some(0.5), Some(0.95));
        assert_eq!(
            blur_verdict(Some(0.9), cuts(3.0, 0.4), &m),
            None,
            "0.5 is a sharp face even though it is far under the frame's cut"
        );
        assert!(
            blur_verdict(Some(0.9), cuts(3.0, 0.9), &m).is_some(),
            "the same reading is soft once the folder's faces are sharper"
        );
    }

    #[test]
    fn overlapping_detections_collapse_to_one_face() {
        let at = |x: f32, score: f32| Face {
            rect: Rect { x, y: 0.0, w: 100.0, h: 100.0 },
            score,
            landmarks: [(0.0, 0.0); 5],
        };
        let kept = non_max_suppression(vec![at(0.0, 0.8), at(10.0, 0.95), at(400.0, 0.9)], 0.3);
        assert_eq!(kept.len(), 2, "the two that overlap are one face");
        assert!((kept[0].score - 0.95).abs() < 1e-6, "the most confident survives");
    }

    fn face_at(eye_span: f32, nose_offset: f32) -> Face {
        Face {
            rect: Rect { x: 0.0, y: 0.0, w: eye_span * 2.0, h: eye_span * 2.5 },
            score: 0.95,
            landmarks: [
                (100.0, 100.0),
                (100.0 + eye_span, 100.0),
                (100.0 + eye_span / 2.0, 100.0 + nose_offset),
                (0.0, 0.0),
                (0.0, 0.0),
            ],
        }
    }

    #[test]
    fn a_face_turned_away_reads_as_less_frontal() {
        assert!(face_at(60.0, 30.0).frontality() > 0.9, "square on");
        assert!(face_at(60.0, 120.0).frontality() < 0.5, "turned away");
    }

    /// Every gate on the eye reading exists to stop one false positive. A tiny face is
    /// the commonest: eyes a few pixels across read as shut whatever they are doing.
    #[test]
    fn a_tiny_face_is_never_judged_on_its_eyes() {
        let gray = GrayImage::from_pixel(400, 400, Luma([120]));
        let (closed, affected) = closed_eyes(&gray, &[face_at(12.0, 6.0)], &DarkExtent);
        assert_eq!((closed, affected), (None, 0));
    }

    #[test]
    fn a_face_in_profile_is_never_judged_on_its_eyes() {
        let gray = GrayImage::from_pixel(400, 400, Luma([120]));
        let (closed, affected) = closed_eyes(&gray, &[face_at(80.0, 200.0)], &DarkExtent);
        assert_eq!((closed, affected), (None, 0));
    }

    /// One eye is never enough. A highlight, a strand of hair or a wink reads as one
    /// shut eye far more often than a spoiled photograph does.
    #[test]
    fn one_eye_alone_does_not_condemn_a_face() {
        struct OnlyLeft;
        impl EyeJudge for OnlyLeft {
            fn closed_probability(&self, eye: &GrayImage) -> Option<f32> {
                // Answer for one crop and abstain on the other.
                (eye.width() > 0).then_some(0.95).filter(|_| false)
            }
        }
        let gray = GrayImage::from_pixel(400, 400, Luma([120]));
        let (closed, _) = closed_eyes(&gray, &[face_at(80.0, 40.0)], &OnlyLeft);
        assert_eq!(closed, None);
    }

    /// A group photograph where one person blinked: the finding names how many faces
    /// it applies to, because "1 of 3" and "3 of 3" are different decisions.
    #[test]
    fn a_group_reports_how_many_faces_are_affected() {
        struct Always(f32);
        impl EyeJudge for Always {
            fn closed_probability(&self, _: &GrayImage) -> Option<f32> {
                Some(self.0)
            }
        }
        let gray = GrayImage::from_pixel(600, 600, Luma([120]));
        let faces = vec![face_at(80.0, 40.0), face_at(80.0, 40.0)];
        let (closed, affected) = closed_eyes(&gray, &faces, &Always(0.9));
        assert_eq!(affected, 2);
        assert!(closed.is_some_and(|c| c >= REPORT));
    }

    /// An open eye shows a tall dark iris; a shut one a thin line of lashes. The
    /// estimator reads the vertical extent, so these two crops must not agree.
    #[test]
    fn the_eye_estimator_separates_a_tall_iris_from_a_thin_line() {
        let mut open = GrayImage::from_pixel(40, 24, Luma([200]));
        for y in 4..20 {
            for x in 14..26 {
                open.put_pixel(x, y, Luma([20]));
            }
        }
        let mut shut = GrayImage::from_pixel(40, 24, Luma([200]));
        for x in 4..36 {
            shut.put_pixel(x, 12, Luma([20]));
            shut.put_pixel(x, 13, Luma([20]));
        }
        let judge = DarkExtent;
        let (o, c) = (
            judge.closed_probability(&open).unwrap(),
            judge.closed_probability(&shut).unwrap(),
        );
        assert!(c > o, "shut {c} should read as more closed than open {o}");
        assert!(o < REPORT, "an open eye must never reach the reporting bar");
    }

    #[test]
    fn a_photo_can_carry_several_findings_at_once() {
        let m = Measurements {
            soft_share: Some(0.98),
            closed_eyes: Some(0.8),
            underexposed: Some(0.7),
            faces: Some(1),
            faces_closed: Some(1),
            ..Default::default()
        };
        let v = verdict(Some(0.2), cuts(1.0, 1.0), &m);
        assert!(v.blur.is_some() && v.closed_eyes.is_some() && v.underexposed.is_some());
        assert_eq!(v.overexposed, None, "what was not found stays absent");
        assert!(v.any());
    }

    #[test]
    fn a_clean_photo_reports_nothing() {
        let v = verdict(Some(5.0), cuts(1.0, 1.0), &Measurements::default());
        assert!(!v.any());
        assert_eq!(v, BadShot { blur: None, ..BadShot::default() });
    }

    /// A finding under the reporting bar is not a finding. The category has to stay
    /// worth opening, and a list the user has to second-guess is worse than a shorter
    /// one that is right.
    #[test]
    fn a_borderline_finding_stays_out_of_the_categories() {
        let v = BadShot { blur: Some(REPORT - 0.01), ..Default::default() };
        assert!(!v.any());
    }

    /// The zone reading is what tells a deliberately soft frame from a failed one, so
    /// it has to actually separate them.
    #[test]
    fn zones_separate_a_sharp_island_from_a_uniformly_soft_frame() {
        let mut sharp_island = RgbImage::from_pixel(256, 256, Rgb([128, 128, 128]));
        for y in 64..192 {
            for x in 64..192 {
                let v = if (x / 2 + y / 2) % 2 == 0 { 240 } else { 10 };
                sharp_island.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let flat_frame = RgbImage::from_pixel(256, 256, Rgb([128, 128, 128]));
        let island = soft_share(&luma(&sharp_island)).unwrap();
        let uniform = soft_share(&luma(&flat_frame)).unwrap();
        assert!(island < uniform, "island {island} vs uniform {uniform}");
        assert!(uniform > 0.9, "a frame with no detail anywhere is soft throughout");
    }
}
