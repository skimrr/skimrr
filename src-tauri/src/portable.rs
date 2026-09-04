//! Export and import of `.skimrr` project files.
//!
//! A scan has always died with the application: what Skimrr knew about a library lived
//! in memory and in a per-file cache keyed by absolute path, neither of which survives
//! being carried anywhere. This module is the bridge between that in-memory scan and the
//! portable container in `skimrr-format` — turning absolute paths into relative ones on
//! the way out, and relative ones back into local files on the way in.
//!
//! The format crate does the security. Nothing here parses a container by hand, and
//! nothing here trusts one: by the time a `Project` reaches this file it has been
//! authenticated and its paths have been checked, and the checks are still repeated at
//! the point a path is turned into a real filename, because that is where the damage
//! would be done.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skimrr_format as fmt;
use tauri::{AppHandle, Manager, State};

use crate::{badshot, compute_view, hash_file, preview_cache_dir, Photo, Record, ScanState};

/// How much of the library travels with the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Findings only: paths, dates, fingerprints, verdicts. Kilobytes.
    Project,
    /// Plus the small renditions, so the project can be looked at on a machine that has
    /// none of the photographs.
    Thumbnails,
    /// Plus the photographs themselves. Gigabytes, and said so before it starts.
    Originals,
}

impl Mode {
    fn contents(self) -> fmt::Contents {
        fmt::Contents {
            thumbnails: matches!(self, Mode::Thumbnails | Mode::Originals),
            originals: matches!(self, Mode::Originals),
        }
    }
}

/// The application's own fields, carried opaquely so the format crate never has to know
/// what a camera model is.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Extra {
    #[serde(default)]
    name: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    format: String,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    measurements: badshot::Measurements,
}

// ---------------------------------------------------------------------- assembling

/// Turns an absolute path into `(root index, relative path)`.
///
/// Longest root wins, so nested roots do not silently attribute a file to the shallower
/// one. Separators are normalised to `/` on the way out — a project written on Windows
/// has to open on Linux, and that only works if one separator is chosen and kept.
fn relativise(path: &str, roots: &[String]) -> Option<(u32, String)> {
    let p = Path::new(path);
    let mut best: Option<(usize, usize)> = None; // (root index, root length)
    for (i, root) in roots.iter().enumerate() {
        if p.starts_with(root) && root.len() > best.map(|b| b.1).unwrap_or(0) {
            best = Some((i, root.len()));
        }
    }
    let (index, _) = best?;
    let rel = p.strip_prefix(&roots[index]).ok()?;
    let mut out = String::new();
    for part in rel.components() {
        let std::path::Component::Normal(part) = part else {
            return None;
        };
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&part.to_string_lossy());
    }
    (!out.is_empty()).then_some((index as u32, out))
}

/// Why a photograph did not make it into the container.
#[derive(Debug, Default, Serialize, Clone)]
pub struct Skipped {
    /// Assets read out of the macOS Photos library. They have no path under any scanned
    /// folder, so there is nothing to relocate on the other machine — carrying the
    /// metadata alone would produce entries nobody could ever resolve.
    library: usize,
    /// Files whose name cannot be written safely on every platform. macOS accepts a
    /// trailing space, Windows does not; rather than fail the whole export over one
    /// file, it is left out and counted.
    unsafe_name: usize,
    /// Files sitting outside every scanned root, which should not happen and is reported
    /// rather than guessed at.
    outside_roots: usize,
}

struct Assembled {
    project: fmt::Project,
    blobs: Vec<Vec<u8>>,
    skipped: Skipped,
    photos: usize,
}

fn assemble(
    records: &[Record],
    roots: &[String],
    threshold: u32,
    mode: Mode,
    name: &str,
) -> Assembled {
    let mut entries = Vec::with_capacity(records.len());
    let mut blobs = Vec::new();
    let mut skipped = Skipped::default();
    // Which exported entry each original path became, so groups can be remapped.
    let mut index_of: HashMap<&str, u32> = HashMap::new();

    for record in records {
        if record.photo.library {
            skipped.library += 1;
            continue;
        }
        let Some((root, rel)) = relativise(&record.photo.path, roots) else {
            skipped.outside_roots += 1;
            continue;
        };
        if fmt::safe_relative_path(&rel).is_err() {
            skipped.unsafe_name += 1;
            continue;
        }

        let thumbnail = mode
            .contents()
            .thumbnails
            .then(|| record.photo.thumb.as_deref().or(Some(record.photo.preview.as_str())))
            .flatten()
            .and_then(|p| std::fs::read(p).ok())
            .map(|bytes| {
                blobs.push(bytes);
                (blobs.len() - 1) as u32
            });

        let original = mode
            .contents()
            .originals
            .then(|| std::fs::read(&record.photo.path).ok())
            .flatten()
            .map(|bytes| {
                blobs.push(bytes);
                (blobs.len() - 1) as u32
            });

        let extra = Extra {
            name: record.photo.name.clone(),
            width: record.photo.width,
            height: record.photo.height,
            format: record.photo.format.clone(),
            device: record.photo.device.clone(),
            kind: record.photo.kind.clone(),
            lat: record.photo.lat,
            lon: record.photo.lon,
            measurements: record.photo.measurements.clone(),
        };

        index_of.insert(record.photo.path.as_str(), entries.len() as u32);
        entries.push(fmt::Entry {
            root,
            path: rel,
            size: record.photo.size,
            sha: record.sha.clone(),
            // Hex rather than a number: CBOR has no 128-bit integer, and a string that
            // reads the same in every language beats two halves that have to be
            // reassembled in the right order on the other side.
            phash: record.phash.map(|h| format!("{h:032x}")),
            taken: record.photo.taken,
            blur: record.photo.blur,
            bad_shot: ciborium::Value::serialized(&record.photo.bad_shot).ok(),
            extra: ciborium::Value::serialized(&extra).ok(),
            kept: true,
            thumbnail,
            original,
        });
    }

    // The clustering is recorded even though an importing Skimrr recomputes it: a reader
    // that has the container but not the clustering code — the browser one — otherwise
    // has no way to show what the project actually found.
    let view = compute_view(records, threshold);
    let mut groups = Vec::new();
    for group in &view.groups {
        let mut members = Vec::new();
        let mut suggested = 0u32;
        for (position, &i) in group.indices.iter().enumerate() {
            if let Some(&mapped) = index_of.get(view.photos[i].path.as_str()) {
                if position == group.suggested {
                    suggested = members.len() as u32;
                }
                members.push(mapped);
            }
        }
        // A group of one is not a group; it is what is left after the others were
        // skipped, and offering it would suggest a duplicate that is not there.
        if members.len() > 1 {
            groups.push(fmt::Group { members, suggested, kind: group.kind.to_string() });
        }
    }

    // Only the keeper of each group is marked kept, which is what the interface would
    // have proposed. Everything ungrouped stays kept, because nothing suggested otherwise.
    for group in &groups {
        for (position, &member) in group.members.iter().enumerate() {
            if position as u32 != group.suggested {
                entries[member as usize].kept = false;
            }
        }
    }

    let photos = entries.len();
    Assembled {
        project: fmt::Project {
            name: name.to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            settings: fmt::Settings {
                similarity_threshold: threshold,
                blur_threshold: 0.0,
                face_threshold: 0.0,
            },
            roots: roots.to_vec(),
            entries,
            groups,
        },
        blobs,
        skipped,
        photos,
    }
}

// ---------------------------------------------------------------------- export

#[derive(Debug, Serialize)]
pub struct Estimate {
    photos: usize,
    /// Roughly what the file will weigh. The manifest compresses and the blobs do not,
    /// so this is close for the two heavy modes and a slight over-estimate for the light
    /// one — which is the right direction to be wrong in.
    bytes: u64,
    skipped: Skipped,
}

#[tauri::command]
pub fn export_estimate(state: State<ScanState>, mode: Mode) -> Estimate {
    let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let mut estimate = Estimate { photos: 0, bytes: 0, skipped: Skipped::default() };

    for record in &data.records {
        if record.photo.library {
            estimate.skipped.library += 1;
            continue;
        }
        let Some((_, rel)) = relativise(&record.photo.path, &data.roots) else {
            estimate.skipped.outside_roots += 1;
            continue;
        };
        if fmt::safe_relative_path(&rel).is_err() {
            estimate.skipped.unsafe_name += 1;
            continue;
        }
        estimate.photos += 1;
        // About a third of a kilobyte of manifest per photograph once deflated.
        estimate.bytes += 340;
        if mode.contents().thumbnails {
            let thumb = record.photo.thumb.as_deref().unwrap_or(&record.photo.preview);
            estimate.bytes += std::fs::metadata(thumb).map(|m| m.len()).unwrap_or(0);
        }
        if mode.contents().originals {
            estimate.bytes += record.photo.size;
        }
    }
    estimate
}

#[derive(Debug, Serialize)]
pub struct Exported {
    path: String,
    bytes: u64,
    photos: usize,
    encrypted: bool,
    skipped: Skipped,
}

#[tauri::command]
pub async fn export_project(
    state: State<'_, ScanState>,
    dest: String,
    mode: Mode,
    threshold: u32,
    name: String,
    password: Option<String>,
) -> Result<Exported, String> {
    let (records, roots) = {
        let data = state.0.lock().unwrap_or_else(|e| e.into_inner());
        (data.records.clone(), data.roots.clone())
    };
    if records.is_empty() {
        return Err("there is nothing to export yet".into());
    }
    if roots.is_empty() {
        return Err("this project has no scanned folder to be relative to".into());
    }

    tauri::async_runtime::spawn_blocking(move || {
        let assembled = assemble(&records, &roots, threshold, mode, &name);
        if assembled.photos == 0 {
            return Err("none of these photographs can be exported".to_string());
        }

        // The password lives in this scope and nowhere else: it is never written to the
        // container, never logged, never put in an error, and the buffer is wiped before
        // the function returns whether it succeeded or not.
        let bytes = fmt::write(
            &assembled.project,
            &assembled.blobs,
            mode.contents(),
            password.as_deref(),
            fmt::Profile::Strong,
        )
        .map_err(|e| e.to_string());
        let encrypted = password.is_some();
        if let Some(password) = password {
            fmt::forget(password);
        }
        let bytes = bytes?;

        let path = with_extension(&dest);
        std::fs::write(&path, &bytes).map_err(|e| format!("could not write the file: {e}"))?;

        Ok(Exported {
            path: path.to_string_lossy().into_owned(),
            bytes: bytes.len() as u64,
            photos: assembled.photos,
            encrypted,
            skipped: assembled.skipped,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Adds the extension if the user did not, and leaves it alone if they did.
fn with_extension(dest: &str) -> PathBuf {
    let path = PathBuf::from(dest);
    match path.extension() {
        Some(e) if e.eq_ignore_ascii_case("skimrr") => path,
        _ => path.with_extension("skimrr"),
    }
}

// ---------------------------------------------------------------------- import

/// What a container says about itself before anything is decrypted.
///
/// This is what lets the interface ask for a password only when there is one to ask
/// about, and show what is inside before the user commits to opening it.
#[derive(Debug, Serialize)]
pub struct Peek {
    encrypted: bool,
    has_thumbnails: bool,
    has_originals: bool,
    photos: usize,
    bytes: u64,
}

#[tauri::command]
pub fn peek_project(path: String) -> Result<Peek, String> {
    // Only the head of the file. A project carrying its originals can be gigabytes, and
    // reading all of it to answer "does this need a password?" would freeze the moment
    // someone double-clicks one.
    let meta = std::fs::metadata(&path).map_err(|e| format!("could not open the file: {e}"))?;
    let bytes = read_head(&path, HEAD_LEN)?;
    let header = fmt::peek(&bytes).map_err(|e| e.to_string())?;
    Ok(Peek {
        encrypted: header.encrypted(),
        has_thumbnails: header.flags().contains(fmt::Flags::THUMBNAILS),
        has_originals: header.flags().contains(fmt::Flags::ORIGINALS),
        // The manifest is inside the encrypted body, so the photograph count is not
        // knowable without the key. The blob count is the honest lower bound available
        // here; the interface shows it as "about", or nothing at all when it is zero.
        photos: header.blob_count as usize,
        bytes: meta.len(),
    })
}

/// Enough to hold the longest header the format permits, plus its fixed prefix.
const HEAD_LEN: usize = 14 + 64 * 1024;

fn read_head(path: &str, len: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("could not open the file: {e}"))?;
    let mut head = Vec::new();
    file.take(len as u64)
        .read_to_end(&mut head)
        .map_err(|e| format!("could not read the file: {e}"))?;
    Ok(head)
}

/// The largest container this build will load.
///
/// `read` holds the file and its decrypted body in memory at once, so the ceiling is
/// about a working set rather than about the format — which permits far more. Streaming
/// would lift it, and is the obvious next thing if anyone ever needs it.
const MAX_CONTAINER: u64 = 8 * 1024 * 1024 * 1024;

fn read_container(path: &str) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("could not open the file: {e}"))?;
    if meta.len() > MAX_CONTAINER {
        return Err("this project file is too large to open".into());
    }
    std::fs::read(path).map_err(|e| format!("could not read the file: {e}"))
}

#[derive(Debug, Serialize)]
pub struct Imported {
    name: String,
    /// Entries in the container.
    photos: usize,
    /// Entries that were matched to a real file on this machine.
    resolved: usize,
    /// Entries that were matched by content rather than by path — the file had been
    /// moved or renamed since the project was made.
    relocated: usize,
    /// Entries with no local file at all. Their findings are shown; they cannot be acted
    /// upon.
    missing: usize,
    /// Originals written out of the container.
    restored: usize,
    /// Originals not written because a file was already there. Nothing is ever
    /// overwritten by opening a project.
    kept_existing: usize,
    encrypted: bool,
}

#[tauri::command]
/// Opens a container into the current scan state.
///
/// `search_root` is where to look for the photographs when the container does not carry
/// them; `restore_root` is where to write them when it does. Both are optional: a
/// project with neither still opens, as findings about files that are not here.
#[allow(clippy::too_many_arguments)]
pub async fn import_project(
    app: AppHandle,
    state: State<'_, ScanState>,
    path: String,
    password: Option<String>,
    search_root: Option<String>,
    restore_root: Option<String>,
) -> Result<Imported, String> {
    let cache = preview_cache_dir(&app);
    let fallback_root = app.path().app_data_dir().ok().map(|d| d.join("imported"));

    // Reading the file, deriving the key and authenticating every frame all happen off
    // the async runtime. Any one of them can take seconds on a large project, and a
    // runtime thread blocked for seconds is an interface frozen for seconds.
    let handle = tauri::async_runtime::spawn_blocking(move || {
        let bytes = read_container(&path)?;
        let opened = {
            let result = fmt::read(&bytes, password.as_deref()).map_err(|e| e.to_string());
            if let Some(password) = password {
                fmt::forget(password);
            }
            result?
        };
        let restore_root = restore_root
            .map(PathBuf::from)
            .or_else(|| fallback_root.map(|d| d.join(slug(&opened.project.name))));
        rebuild(opened, cache, search_root.map(PathBuf::from), restore_root)
    })
    .await
    .map_err(|e| e.to_string())?;

    let (records, roots, summary) = handle?;
    let mut data = state.0.lock().unwrap_or_else(|e| e.into_inner());
    data.records = records;
    data.roots = roots;
    data.trashed.clear();
    Ok(summary)
}

/// A project name reduced to something safe to use as a folder name on any platform.
///
/// Everything outside `[a-z0-9]` becomes a dash and runs of dashes collapse, so an
/// accented or non-Latin name yields a short readable stem rather than a row of
/// separators — and so that nothing a name contains can ever be read as a path.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(48).collect();
    let out = out.trim_matches('-');
    if out.is_empty() {
        "project".into()
    } else {
        out.to_string()
    }
}

type Rebuilt = Result<(Vec<Record>, Vec<String>, Imported), String>;

fn rebuild(
    opened: fmt::Opened,
    cache: Option<PathBuf>,
    search_root: Option<PathBuf>,
    restore_root: Option<PathBuf>,
) -> Rebuilt {
    let project = opened.project;
    let blobs = opened.blobs;
    let encrypted = opened.header.encrypted();
    let has_originals = opened.header.flags().contains(fmt::Flags::ORIGINALS);

    let mut summary = Imported {
        name: project.name.clone(),
        photos: project.entries.len(),
        resolved: 0,
        relocated: 0,
        missing: 0,
        restored: 0,
        kept_existing: 0,
        encrypted,
    };

    // Built lazily and only when it can help: a size index of the folder the user pointed
    // at, so a file that has been renamed can still be found by content without hashing
    // the whole folder. Only candidates of exactly the right size are ever opened.
    let by_size = search_root.as_ref().filter(|_| !has_originals).map(|root| index_by_size(root));

    let root_for_restore = restore_root.filter(|_| has_originals);
    if let Some(root) = &root_for_restore {
        std::fs::create_dir_all(root).map_err(|e| format!("could not create {root:?}: {e}"))?;
    }

    let mut records = Vec::with_capacity(project.entries.len());
    for entry in &project.entries {
        // Checked again here even though `fmt::read` has already refused anything unsafe.
        // This is the line that turns a string from a file into a real filename, and a
        // check at the point of use is the one that cannot be bypassed by a future change
        // somewhere else.
        if fmt::safe_relative_path(&entry.path).is_err() {
            continue;
        }

        let mut local: Option<PathBuf> = None;

        if let Some(root) = &root_for_restore {
            if let Some(index) = entry.original {
                let target = root.join(&entry.path);
                if target.exists() {
                    summary.kept_existing += 1;
                    local = Some(target);
                } else if let Some(bytes) = blobs.get(index as usize) {
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&target, bytes) {
                        Ok(()) => {
                            summary.restored += 1;
                            local = Some(target);
                        }
                        Err(_) => local = None,
                    }
                }
            }
        }

        if local.is_none() {
            if let Some(root) = &search_root {
                let direct = root.join(&entry.path);
                // The size has to agree. A different photograph can easily end up at the
                // same relative path — two libraries both have `2024/city/marche.jpg` —
                // and attaching this project's findings to it would report a sharpness
                // and a duplicate grouping that belong to another picture entirely.
                // Size is free; hashing every direct hit would not be.
                let same_size =
                    std::fs::metadata(&direct).map(|m| m.len() == entry.size).unwrap_or(false);
                if direct.is_file() && same_size {
                    local = Some(direct);
                } else if let Some(by_size) = &by_size {
                    local = find_by_content(entry, by_size);
                    if local.is_some() {
                        summary.relocated += 1;
                    }
                }
            }
        }

        match &local {
            Some(_) => summary.resolved += 1,
            None => summary.missing += 1,
        }

        // A thumbnail from the container is written into the ordinary preview cache, so
        // everything downstream — grids, covers, the detail view — works on an imported
        // project exactly as it does on a scanned one.
        let thumb = entry
            .thumbnail
            .and_then(|i| blobs.get(i as usize))
            .zip(cache.as_ref())
            .and_then(|(bytes, cache)| write_cached_thumb(cache, entry, bytes));

        let extra: Extra = entry
            .extra
            .as_ref()
            .and_then(|v| v.deserialized().ok())
            .unwrap_or_default();

        let display_path = local
            .clone()
            .unwrap_or_else(|| PathBuf::from(&project.roots[entry.root as usize]).join(&entry.path));
        let preview = thumb.clone().unwrap_or_else(|| display_path.to_string_lossy().into_owned());

        records.push(Record {
            sha: entry.sha.clone(),
            phash: entry.phash.as_deref().and_then(|h| u128::from_str_radix(h, 16).ok()),
            photo: Photo {
                path: display_path.to_string_lossy().into_owned(),
                name: if extra.name.is_empty() {
                    entry.path.rsplit('/').next().unwrap_or(&entry.path).to_string()
                } else {
                    extra.name
                },
                size: entry.size,
                width: extra.width,
                height: extra.height,
                taken: entry.taken,
                blur: entry.blur,
                measurements: extra.measurements,
                bad_shot: entry
                    .bad_shot
                    .as_ref()
                    .and_then(|v| v.deserialized().ok())
                    .unwrap_or_default(),
                preview,
                thumb,
                library: false,
                missing: local.is_none(),
                preview_dims: None,
                lat: extra.lat,
                lon: extra.lon,
                format: extra.format,
                device: extra.device,
                kind: extra.kind,
            },
        });
    }

    // The roots an imported project is relative to are the local ones, not the ones the
    // sender had: re-exporting must produce paths relative to where the files are now.
    let roots = match (&root_for_restore, &search_root) {
        (Some(root), _) | (None, Some(root)) => vec![root.to_string_lossy().into_owned()],
        _ => project.roots.clone(),
    };

    Ok((records, roots, summary))
}

/// Files under a folder, grouped by exact byte length.
///
/// Size is a free filter that eliminates almost everything; only the handful of files
/// that could possibly be a given photograph are then hashed. Walking a large folder
/// costs a directory traversal, not a read of every file.
fn index_by_size(root: &Path) -> HashMap<u64, Vec<PathBuf>> {
    let mut index: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            index.entry(meta.len()).or_default().push(entry.into_path());
        }
    }
    index
}

fn find_by_content(entry: &fmt::Entry, by_size: &HashMap<u64, Vec<PathBuf>>) -> Option<PathBuf> {
    let sha = entry.sha.as_ref()?;
    let candidates = by_size.get(&entry.size)?;
    candidates.iter().find(|path| hash_file(path).is_ok_and(|h| &h == sha)).cloned()
}

fn write_cached_thumb(cache: &Path, entry: &fmt::Entry, bytes: &[u8]) -> Option<String> {
    // Named after what the file *is*, so importing the same project twice does not
    // accumulate copies, and two projects sharing a photograph share one cached thumb.
    let key = entry.sha.clone().unwrap_or_else(|| {
        format!("{:016x}-{}", entry.size, entry.path.len())
    });
    let path = cache.join(format!("imported-{key}.bin"));
    if !path.exists() {
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativises_against_the_deepest_matching_root() {
        let roots = vec!["/Users/a/Pictures".to_string(), "/Users/a/Pictures/2024".to_string()];
        assert_eq!(
            relativise("/Users/a/Pictures/2024/beach.jpg", &roots),
            Some((1, "beach.jpg".into())),
            "a nested root must win, or the file is filed under the wrong folder"
        );
        assert_eq!(
            relativise("/Users/a/Pictures/2023/x.jpg", &roots),
            Some((0, "2023/x.jpg".into()))
        );
        assert_eq!(relativise("/elsewhere/x.jpg", &roots), None);
        // A root is not itself an entry.
        assert_eq!(relativise("/Users/a/Pictures", &roots), None);
    }

    #[test]
    fn relative_paths_never_carry_a_traversal() {
        // `Component::Normal` is the only kind accepted, so nothing that is not a plain
        // name can survive the conversion — which is what `safe_relative_path` then
        // double-checks.
        let roots = vec!["/r".to_string()];
        for path in ["/r/../escape.jpg", "/r/./a.jpg", "/r"] {
            if let Some((_, rel)) = relativise(path, &roots) {
                assert!(fmt::safe_relative_path(&rel).is_ok(), "{path} produced {rel}");
                assert!(!rel.contains(".."), "{path} produced {rel}");
            }
        }
    }

    #[test]
    fn the_extension_is_added_but_never_doubled() {
        assert_eq!(with_extension("/tmp/a"), PathBuf::from("/tmp/a.skimrr"));
        assert_eq!(with_extension("/tmp/a.skimrr"), PathBuf::from("/tmp/a.skimrr"));
        assert_eq!(with_extension("/tmp/a.SKIMRR"), PathBuf::from("/tmp/a.SKIMRR"));
        assert_eq!(with_extension("/tmp/a.jpg"), PathBuf::from("/tmp/a.skimrr"));
    }

    #[test]
    fn slugs_are_always_usable_as_a_folder_name() {
        assert_eq!(slug("Été 2024 — Corse"), "t-2024-corse");
        assert_eq!(slug("../../etc"), "etc");
        assert_eq!(slug(""), "project");
        assert_eq!(slug("///"), "project");
        assert!(slug(&"x".repeat(500)).len() <= 48);
    }
}

#[cfg(test)]
mod round_trip_tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("skimrr-portable-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Bytes that look nothing like each other, so a mix-up cannot pass unnoticed.
    fn contents(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| seed.wrapping_mul(31).wrapping_add((i * 7) as u8)).collect()
    }

    fn record(path: &Path, seed: u8, taken: i64) -> Record {
        let bytes = std::fs::read(path).unwrap();
        Record {
            sha: Some(crate::hash_file(path).unwrap()),
            phash: Some(0x0f1e_2d3c_4b5a_6978_0011_2233_4455_6677u128 ^ seed as u128),
            photo: Photo {
                path: path.to_string_lossy().into_owned(),
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                size: bytes.len() as u64,
                width: 4032,
                height: 3024,
                taken,
                blur: Some(140.5),
                measurements: badshot::Measurements::default(),
                bad_shot: badshot::BadShot::default(),
                preview: path.to_string_lossy().into_owned(),
                thumb: None,
                library: false,
                missing: false,
                preview_dims: None,
                lat: Some(41.9),
                lon: Some(8.7),
                format: "JPG".into(),
                device: Some("iPhone 13".into()),
                kind: None,
            },
        }
    }

    /// Reproduces a real export from the real scan cache, to find out what a failing
    /// one actually says. Ignored by default: it needs a cache on this machine.
    #[test]
    #[ignore = "needs a local scan cache"]
    fn export_the_screenshot_folder() {
        let cache = dirs_cache().join("scans/cache.json");
        // Typé plutôt que via `serde_json::Value` : le `phash` est un u128, et un Value
        // le rabat sur un f64 qui perd les bits de poids faible.
        #[derive(serde::Deserialize)]
        struct Cached { record: Record }
        #[derive(serde::Deserialize)]
        struct Cache { files: std::collections::BTreeMap<String, Cached> }

        let raw = std::fs::read_to_string(&cache).expect("no scan cache");
        let parsed: Cache = serde_json::from_str(&raw).unwrap();
        let records: Vec<Record> = parsed
            .files
            .into_iter()
            .filter(|(path, _)| path.contains("shot-photos"))
            .map(|(_, c)| c.record)
            .collect();
        eprintln!("records: {}", records.len());
        assert!(!records.is_empty());

        let roots = vec!["/Users/baptiste/Fichiers/Skimrr/tools/shot-photos".to_string()];
        for mode in [Mode::Project, Mode::Thumbnails] {
            let a = assemble(&records, &roots, 28, mode, "shot-photos");
            eprintln!(
                "{mode:?}: photos={} blobs={} skipped={:?} groups={}",
                a.photos,
                a.blobs.len(),
                a.skipped,
                a.project.groups.len()
            );
            match fmt::write(&a.project, &a.blobs, mode.contents(), None, fmt::Profile::Strong) {
                Ok(bytes) => {
                    eprintln!("  wrote {} bytes", bytes.len());
                    if let Ok(out) = std::env::var("SKIMRR_EXPORT_TO") {
                        if matches!(mode, Mode::Thumbnails) {
                            std::fs::write(&out, &bytes).unwrap();
                            eprintln!("  saved to {out}");
                        }
                    }
                }
                Err(e) => panic!("{mode:?} FAILED: {e}"),
            }
            // Et la même chose chiffrée, pour éprouver le mot de passe côté navigateur.
            if matches!(mode, Mode::Thumbnails) {
                if let Ok(out) = std::env::var("SKIMRR_EXPORT_LOCKED") {
                    let bytes = fmt::write(&a.project, &a.blobs, mode.contents(),
                                           Some("un mot de passe"), fmt::Profile::Strong).unwrap();
                    std::fs::write(&out, &bytes).unwrap();
                    eprintln!("  saved encrypted to {out}");
                }
            }
        }
    }

    fn dirs_cache() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap()).join("Library/Caches/com.skimrr.app")
    }

    /// The whole journey: a scan on one machine becomes a file, and the file becomes a
    /// scan on another — where the photographs sit under a different folder and two of
    /// them have been renamed since.
    #[test]
    fn a_project_survives_being_carried_to_another_machine() {
        let here = scratch("origin");
        let there = scratch("destination");
        std::fs::create_dir_all(here.join("2024/corse")).unwrap();
        std::fs::create_dir_all(there.join("backup/somewhere else")).unwrap();

        let names = ["2024/corse/plage.jpg", "2024/corse/plage-2.jpg", "2024/été.heic"];
        let mut records = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let path = here.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents(i as u8 + 1, 2000 + i * 10)).unwrap();
            records.push(record(&path, i as u8, 1_700_000_000 + i as i64));
        }

        let roots = vec![here.to_string_lossy().into_owned()];
        let assembled = assemble(&records, &roots, 10, Mode::Project, "Été 2024 — Corse");
        assert_eq!(assembled.photos, 3);
        assert_eq!(assembled.skipped.library, 0);
        assert_eq!(assembled.skipped.outside_roots, 0);
        for entry in &assembled.project.entries {
            assert!(!entry.path.starts_with('/'), "{} is not relative", entry.path);
        }

        let container = fmt::write(
            &assembled.project,
            &assembled.blobs,
            Mode::Project.contents(),
            Some("a passphrase"),
            fmt::Profile::Strong,
        )
        .unwrap();

        // On the other machine the same photographs live elsewhere, and two of them have
        // been renamed — which is exactly why entries are identified by content.
        std::fs::copy(here.join(names[0]), there.join("backup/renamed-A.jpg")).unwrap();
        std::fs::copy(here.join(names[1]), there.join("backup/somewhere else/B.jpg")).unwrap();
        std::fs::create_dir_all(there.join("2024")).unwrap();
        std::fs::copy(here.join(names[2]), there.join("2024/été.heic")).unwrap();

        let opened = fmt::read(&container, Some("a passphrase")).unwrap();
        let (rebuilt, new_roots, summary) =
            rebuild(opened, None, Some(there.clone()), None).unwrap();

        assert_eq!(summary.photos, 3);
        assert_eq!(summary.resolved, 3, "every photograph was found on the other machine");
        assert_eq!(summary.relocated, 2, "two of them only by their contents");
        assert_eq!(summary.missing, 0);
        assert!(summary.encrypted);
        assert_eq!(new_roots, vec![there.to_string_lossy().into_owned()]);

        // The findings came across intact, and every path now points at a real local file.
        assert_eq!(rebuilt.len(), 3);
        for (before, after) in records.iter().zip(&rebuilt) {
            assert_eq!(after.sha, before.sha);
            assert_eq!(after.phash, before.phash);
            assert_eq!(after.photo.taken, before.photo.taken);
            assert_eq!(after.photo.size, before.photo.size);
            assert_eq!(after.photo.blur, before.photo.blur);
            assert_eq!(after.photo.width, before.photo.width);
            assert_eq!(after.photo.device, before.photo.device);
            assert_eq!(after.photo.lat, before.photo.lat);
            assert_eq!(after.photo.name, before.photo.name);
            assert!(!after.photo.missing);
            assert!(Path::new(&after.photo.path).is_file(), "{}", after.photo.path);
            assert!(Path::new(&after.photo.path).starts_with(&there));
        }

        let _ = std::fs::remove_dir_all(&here);
        let _ = std::fs::remove_dir_all(&there);
    }

    /// The same file opened on a machine that has none of the photographs. The findings
    /// are still worth reading, and every entry says plainly that its file is not here.
    #[test]
    fn a_project_opens_where_the_photographs_are_absent() {
        let here = scratch("absent-origin");
        std::fs::create_dir_all(&here).unwrap();
        let path = here.join("a.jpg");
        std::fs::write(&path, contents(9, 1500)).unwrap();
        let records = vec![record(&path, 9, 1_700_000_000)];

        let roots = vec![here.to_string_lossy().into_owned()];
        let assembled = assemble(&records, &roots, 10, Mode::Project, "orphan");
        let container = fmt::write(
            &assembled.project,
            &assembled.blobs,
            Mode::Project.contents(),
            None,
            fmt::Profile::Strong,
        )
        .unwrap();

        let opened = fmt::read(&container, None).unwrap();
        let (rebuilt, _, summary) = rebuild(opened, None, None, None).unwrap();
        assert_eq!(summary.resolved, 0);
        assert_eq!(summary.missing, 1);
        assert!(rebuilt[0].photo.missing, "an entry with no local file must say so");
        assert_eq!(rebuilt[0].photo.blur, Some(140.5), "the findings still came across");

        let _ = std::fs::remove_dir_all(&here);
    }

    /// Carrying the photographs themselves, and the promise that opening a project never
    /// overwrites anything already on disk.
    #[test]
    fn originals_travel_and_never_overwrite_what_is_already_there() {
        let here = scratch("originals-origin");
        let there = scratch("originals-destination");
        std::fs::create_dir_all(here.join("trip")).unwrap();

        let mut records = Vec::new();
        for (i, name) in ["trip/one.jpg", "trip/two.jpg"].iter().enumerate() {
            let path = here.join(name);
            std::fs::write(&path, contents(i as u8 + 40, 3000 + i)).unwrap();
            records.push(record(&path, i as u8 + 40, 1_700_000_000));
        }

        let roots = vec![here.to_string_lossy().into_owned()];
        let assembled = assemble(&records, &roots, 10, Mode::Originals, "with originals");
        assert_eq!(assembled.blobs.len(), 4, "a thumbnail and an original for each");

        let container = fmt::write(
            &assembled.project,
            &assembled.blobs,
            Mode::Originals.contents(),
            None,
            fmt::Profile::Strong,
        )
        .unwrap();

        // A file is already sitting where one of the originals would go, holding
        // something else entirely.
        std::fs::create_dir_all(there.join("trip")).unwrap();
        std::fs::write(there.join("trip/one.jpg"), b"do not lose me").unwrap();

        let opened = fmt::read(&container, None).unwrap();
        assert!(opened.header.flags().contains(fmt::Flags::ORIGINALS));
        let (rebuilt, _, summary) = rebuild(opened, None, None, Some(there.clone())).unwrap();

        assert_eq!(summary.restored, 1);
        assert_eq!(summary.kept_existing, 1, "the file that was already there was left alone");
        assert_eq!(summary.resolved, 2);
        assert_eq!(
            std::fs::read(there.join("trip/one.jpg")).unwrap(),
            b"do not lose me",
            "opening a project must never overwrite a file"
        );
        assert_eq!(
            std::fs::read(there.join("trip/two.jpg")).unwrap(),
            std::fs::read(here.join("trip/two.jpg")).unwrap(),
            "and the one it did write came across byte for byte"
        );
        assert_eq!(rebuilt.len(), 2);

        let _ = std::fs::remove_dir_all(&here);
        let _ = std::fs::remove_dir_all(&there);
    }

    /// A different photograph sitting at the same relative path must not inherit this
    /// project's findings. Two libraries both having `2024/city/marche.jpg` is ordinary,
    /// and a sharpness score attached to the wrong picture is worse than no score.
    #[test]
    fn a_path_collision_with_different_content_is_not_the_same_photograph() {
        let here = scratch("collision-origin");
        let there = scratch("collision-destination");
        std::fs::create_dir_all(here.join("city")).unwrap();
        std::fs::create_dir_all(there.join("city")).unwrap();

        let path = here.join("city/marche.jpg");
        std::fs::write(&path, contents(11, 5000)).unwrap();
        let records = vec![record(&path, 11, 1_700_000_000)];

        // Same name, same folder, different picture — and a different length, which is
        // what makes the mismatch free to notice.
        std::fs::write(there.join("city/marche.jpg"), contents(12, 8123)).unwrap();

        let roots = vec![here.to_string_lossy().into_owned()];
        let assembled = assemble(&records, &roots, 10, Mode::Project, "collision");
        let container =
            fmt::write(&assembled.project, &assembled.blobs, Mode::Project.contents(), None, fmt::Profile::Strong)
                .unwrap();

        let opened = fmt::read(&container, None).unwrap();
        let (rebuilt, _, summary) = rebuild(opened, None, Some(there.clone()), None).unwrap();
        assert_eq!(summary.resolved, 0, "the impostor must not be accepted");
        assert_eq!(summary.missing, 1);
        assert!(rebuilt[0].photo.missing);

        // And when the real one is there under another name, it is found by content.
        std::fs::write(there.join("elsewhere.jpg"), contents(11, 5000)).unwrap();
        let opened = fmt::read(&container, None).unwrap();
        let (_, _, summary) = rebuild(opened, None, Some(there.clone()), None).unwrap();
        assert_eq!(summary.resolved, 1);
        assert_eq!(summary.relocated, 1);

        let _ = std::fs::remove_dir_all(&here);
        let _ = std::fs::remove_dir_all(&there);
    }

    /// A photograph from the macOS Photos library cannot be relocated anywhere, so it is
    /// left out and counted rather than exported as an entry nobody could ever resolve.
    #[test]
    fn library_assets_are_left_out_and_reported() {
        let here = scratch("library");
        let path = here.join("a.jpg");
        std::fs::write(&path, contents(3, 900)).unwrap();

        let mut records = vec![record(&path, 3, 1_700_000_000)];
        let mut asset = records[0].clone();
        asset.photo.library = true;
        asset.photo.path = "/somewhere/in/a/library/IMG_0001.HEIC".into();
        records.push(asset);

        let roots = vec![here.to_string_lossy().into_owned()];
        let assembled = assemble(&records, &roots, 10, Mode::Project, "mixed");
        assert_eq!(assembled.photos, 1);
        assert_eq!(assembled.skipped.library, 1);
        assert_eq!(assembled.skipped.outside_roots, 0, "a library asset is not an outside file");

        let _ = std::fs::remove_dir_all(&here);
    }
}

// ---------------------------------------------------------------------- opened by the OS

/// A `.skimrr` the operating system has asked us to open.
///
/// Held rather than acted on: at the moment the request arrives the interface may not
/// exist yet, and opening a project is not something to do behind the user's back
/// anyway — an encrypted one needs a password, and any of them replaces what is on
/// screen. So the path waits here until the interface comes and asks for it.
#[derive(Default)]
pub struct PendingOpen(pub std::sync::Mutex<Option<String>>);

/// The `.skimrr` named on the command line, if there is one.
///
/// How Windows and Linux hand a double-clicked file to an application. Filtered rather
/// than trusted: an argument is only taken when it ends in `.skimrr` and names a file
/// that actually exists, so a stray flag can never be read as a project.
pub fn from_argv() -> Option<String> {
    std::env::args().skip(1).find(|arg| is_project_file(Path::new(arg)))
}

fn is_project_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("skimrr")) && path.is_file()
}

/// Records a path the system wants opened, and tells the interface it is there.
///
/// The event matters for the second case: on macOS a running Skimrr is handed the file
/// through `Opened` rather than through a new process, so nothing would otherwise go and
/// look for it.
pub fn remember(app: &AppHandle, path: String) {
    if let Some(state) = app.try_state::<PendingOpen>() {
        *state.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
    let _ = tauri::Emitter::emit(app, "project-opened", ());
}

/// macOS hands over files as URLs, including while the application is already running.
#[cfg(target_os = "macos")]
pub fn opened_urls(app: &AppHandle, urls: &[tauri::Url]) {
    for path in urls.iter().filter_map(|u| u.to_file_path().ok()) {
        if is_project_file(&path) {
            remember(app, path.to_string_lossy().into_owned());
            return;
        }
    }
}

/// Hands the waiting path to the interface, and forgets it.
///
/// Taken rather than read: a project offered once and declined should not be offered
/// again on the next render.
#[tauri::command]
pub fn take_pending_project(state: State<PendingOpen>) -> Option<String> {
    state.0.lock().unwrap_or_else(|e| e.into_inner()).take()
}
