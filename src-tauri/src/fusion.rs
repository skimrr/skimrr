//! "Sources vs Destination" matching: reading a file's own capture date well enough to
//! compare it against what a destination library (Apple Photos) already has for the
//! same shot, ahead of importing or deleting anything there.

use chrono::NaiveDateTime;
use std::path::{Path, PathBuf};

/// Reads a file's own "taken" timestamp before any timezone correction: EXIF
/// `DateTimeOriginal` for image formats, ISO-BMFF `mvhd.creation_time` for MP4/MOV/M4V.
///
/// `None` for anything else, or when the field itself is absent — deliberately no
/// mtime fallback here (unlike the scan pipeline's own `taken_date`): a copied or
/// exported file's mtime reflects when it was copied, not when it was captured, and
/// this exists specifically to compare captures against a destination library's own
/// capture dates, where a wrong-but-present value is worse than a known absence.
pub fn read_naive_taken(path: &Path) -> Option<i64> {
    match crate::ext_of(path).as_deref() {
        Some("mp4" | "mov" | "m4v") => mp4_creation_time(path),
        _ => exif_taken_naive(path),
    }
}

/// Corrects a naive reading — parsed as if it already were UTC — for `offset_secs`
/// seconds of local-time drift. EXIF and MP4 timestamps are routinely written as the
/// camera's local wall-clock time with no zone attached (or, for MP4, in a field
/// specced as UTC that phones commonly fill with local time anyway), so a naive parse
/// is off by exactly the capturing device's own UTC offset at the time.
///
/// A camera set to UTC+2 that stamped "14:00" really meant 12:00 UTC — `offset_secs`
/// *ahead* of the true instant — so it is subtracted; a negative `offset_secs` (a zone
/// behind UTC) naturally adds instead.
pub fn apply_timezone_offset(naive_as_utc: i64, offset_secs: i32) -> i64 {
    naive_as_utc - offset_secs as i64
}

fn exif_taken_naive(path: &Path) -> Option<i64> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let meta = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = meta.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    parse_exif_datetime(&field.display_value().to_string())
}

/// EXIF's spec separator is `:` even inside the date (`"2024:06:15 14:30:00"`), but
/// some encoders write the more usual `-`; both are accepted.
fn parse_exif_datetime(s: &str) -> Option<i64> {
    ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S"]
        .iter()
        .find_map(|fmt| NaiveDateTime::parse_from_str(s, fmt).ok())
        .map(|dt| dt.and_utc().timestamp())
}

/// Seconds between the QuickTime/MP4 box epoch (1904-01-01 00:00:00 UTC) and the Unix
/// epoch (1970-01-01 00:00:00 UTC).
const MAC_TO_UNIX_EPOCH_SECS: i64 = 2_082_844_800;

/// Reads `creation_time` from the `moov/mvhd` box of an ISO-BMFF container — the
/// structure MP4, MOV and M4V all share. `None` for anything unparseable; AVI, MKV and
/// WebM use unrelated container formats this does not attempt to read.
fn mp4_creation_time(path: &Path) -> Option<i64> {
    let data = std::fs::read(path).ok()?;
    let moov = find_box(&data, b"moov")?;
    let mvhd = find_box(moov, b"mvhd")?;
    let version = *mvhd.first()?;
    let body = mvhd.get(4..)?; // skip the 1-byte version + 3-byte flags of the FullBox header
    let mac_time = if version == 1 {
        i64::from_be_bytes(body.get(0..8)?.try_into().ok()?)
    } else {
        u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as i64
    };
    Some(mac_time - MAC_TO_UNIX_EPOCH_SECS)
}

/// Finds the first immediate child box of `container` (or the top-level boxes of a
/// whole file, when `container` is the full buffer) whose fourcc is `want`, returning
/// its body — everything after the 8-byte `size + type` header (16 bytes for the rare
/// 64-bit "largesize" form). Malformed length fields stop the walk rather than
/// indexing past the buffer.
fn find_box<'a>(container: &'a [u8], want: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;
    while pos + 8 <= container.len() {
        let size32 = u32::from_be_bytes(container[pos..pos + 4].try_into().ok()?);
        let kind = &container[pos + 4..pos + 8];
        let (header_len, box_size) = if size32 == 1 {
            let size64 = u64::from_be_bytes(container.get(pos + 8..pos + 16)?.try_into().ok()?);
            (16usize, size64 as usize)
        } else if size32 == 0 {
            (8usize, container.len() - pos)
        } else {
            (8usize, size32 as usize)
        };
        if box_size < header_len || pos + box_size > container.len() {
            break;
        }
        if kind == want {
            return Some(&container[pos + header_len..pos + box_size]);
        }
        pos += box_size;
    }
    None
}

/// Imports files into Apple Photos via `osascript`, the only supported way to add
/// assets to the library without touching its internal storage directly: Photos owns
/// its own on-disk representation (a Core Data store plus a managed file layout), and
/// there is no way to add to it by copying files into place that would not risk
/// corrupting it — `import` is Photos' own scriptable command for exactly this.
///
/// Verified against this machine's own `Photos.app` (`sdef
/// /System/Applications/Photos.app`, Photos Suite → `import` command): `import <list
/// of file specs> [skip check duplicates true]`. `skip check duplicates true` is used
/// deliberately — duplicate detection is already this app's job, done with a real
/// perceptual hash rather than Photos' own (stricter, exact-file) check, so Photos is
/// told not to second-guess a decision already made upstream.
///
/// Returns the stdout `osascript` produced (one line per imported item, from the
/// `import` command's own result) on success, or its stderr on failure — most commonly
/// because Photos hasn't been granted permission to run, or isn't installed.
pub fn import_paths(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    run_osascript(&import_script(paths))
}

fn import_script(paths: &[PathBuf]) -> String {
    let file_list = paths
        .iter()
        .map(|p| format!("POSIX file \"{}\"", escape_applescript_string(&p.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(", ");
    format!("tell application \"Photos\"\n    import {{{file_list}}} skip check duplicates true\nend tell")
}

/// No counterpart to `import_paths` exists for removing individual items: Photos'
/// scripting dictionary defines `delete` explicitly as "Only albums and folders can be
/// deleted" (confirmed from the real `sdef` above, not assumed), and the `media item`
/// class exposes no delete/trash command or property at all. The only way to remove a
/// specific asset from the library at all is UI automation (System Events driving the
/// Photos window) — another TCC permission (Accessibility), fragile against any UI or
/// localization change, and it needs the Photos window open and frontmost throughout.
///
/// Decided against building that: once a source file is confirmed already present in
/// Photos, the redundant copy is removed from the *source* side instead, through the
/// scan pipeline's existing trash (`trash_to`/`undo_from` in `lib.rs`, already tested).
/// Photos itself is only ever written to via the clean, supported `import` path above.
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn run_osascript(script: &str) -> Result<Vec<String>, String> {
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("skimrr-osascript-{}-{}.applescript", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)));
    std::fs::write(&tmp, script).map_err(|e| e.to_string())?;
    let output = std::process::Command::new("osascript").arg(&tmp).output();
    let _ = std::fs::remove_file(&tmp);
    let output = output.map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Seconds between the Unix epoch (1970-01-01 00:00:00 UTC) and Apple's Core Data /
/// Cocoa reference date (2001-01-01 00:00:00 UTC), which every `Z*` `TIMESTAMP` column
/// in `Photos.sqlite` is measured from — confirmed against a real row on a real
/// library, not assumed: a raw `ZDATECREATED` of `809101457.744` converts to
/// 2026-08-22, a plausible capture date rather than nonsense.
const CORE_DATA_TO_UNIX_EPOCH_SECS: i64 = 978_307_200;

/// One row of `ZASSET` (joined to `ZADDITIONALASSETATTRIBUTES` for its original file
/// size), reduced to what matching against a source file needs.
#[derive(Debug, Clone, PartialEq)]
pub struct PhotosAsset {
    pub uuid: String,
    pub filename: String,
    /// Unix seconds, already converted from `ZDATECREATED`'s Core Data epoch. `None`
    /// when the column itself is NULL, which does happen for assets still importing.
    pub taken: Option<i64>,
    /// `ZADDITIONALASSETATTRIBUTES.ZORIGINALFILESIZE` — the imported file's own byte
    /// size, not a re-encoded derivative's. `None` when the join row is missing.
    pub size: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub is_video: bool,
    pub trashed: bool,
    pub hidden: bool,
}

/// Indexes every asset's matching-relevant metadata from `<library>/database/
/// Photos.sqlite`, opened strictly read-only — filename, capture date, original byte
/// size, pixel size, video/trashed/hidden flags. Deliberately metadata only, not the
/// asset's own file bytes: on a real library checked here, `originals/<hex>/` exists
/// but is empty for every bucket, trashed or not — this Mac has iCloud "Optimize Mac
/// Storage" on, so most assets' full-resolution data simply is not on disk to hash.
/// Only Photos' own on-demand download (via PhotosKit, not raw file access) can fetch
/// it, so matching has to work from metadata against a source file's own reading
/// (`read_naive_taken`, plus its filesystem size) rather than a second perceptual hash.
///
/// `Err` when the file can't be opened (wrong path, or the caller's process lacks
/// Full Disk Access — macOS gates raw reads of this specific file under that
/// permission, confirmed via `sqlite3 -readonly` returning "authorization denied"
/// without it) or the expected columns aren't there (an unexpected schema).
pub fn read_photos_index(library_path: &Path) -> Result<Vec<PhotosAsset>, String> {
    let db_path = library_path.join("database/Photos.sqlite");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("{}: {e}", db_path.display()))?;

    let mut stmt = conn
        .prepare(
            "SELECT a.ZUUID, a.ZFILENAME, a.ZDATECREATED, a.ZWIDTH, a.ZHEIGHT, \
             a.ZDURATION, a.ZTRASHEDSTATE, a.ZHIDDEN, x.ZORIGINALFILESIZE \
             FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES x \
             ON a.ZADDITIONALATTRIBUTES = x.Z_PK",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let core_data_date: Option<f64> = row.get(2)?;
            let size: Option<i64> = row.get(8)?;
            Ok(PhotosAsset {
                uuid: row.get(0)?,
                filename: row.get(1)?,
                taken: core_data_date.map(|d| d as i64 + CORE_DATA_TO_UNIX_EPOCH_SECS),
                size: size.map(|s| s.max(0) as u64),
                width: row.get::<_, i64>(3)?.max(0) as u32,
                height: row.get::<_, i64>(4)?.max(0) as u32,
                is_video: row.get::<_, f64>(5)? > 0.0,
                trashed: row.get::<_, i64>(6)? != 0,
                hidden: row.get::<_, i64>(7)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// The first non-trashed asset in `index` that plausibly is the source file already
/// imported. Filename (case-insensitive — Photos is happy to relowercase an extension
/// on import) plus an exact original-byte-size match is the real signal: a same-name,
/// same-byte-size collision between two genuinely different photos is vanishingly
/// unlikely, whereas capture-date agreement is not trustworthy enough alone to lean
/// on — whether Photos' own `ZDATECREATED` and a source file's naively-parsed EXIF/MP4
/// reading (`read_naive_taken`, no timezone applied) land within seconds of each other
/// or hours apart depending on the capturing device's own UTC offset is not something
/// this codebase has verified against real matched pairs. So date is used only as a
/// loose sanity bound (`date_tolerance_secs`, expect something generous — hours, not
/// seconds) to catch a pathological same-name-same-size coincidence, never as the
/// deciding signal on its own; a missing date/size on either side just skips that check
/// rather than blocking the match, since filename+size alone is already strong enough.
pub fn already_in_photos<'a>(
    source_filename: &str,
    source_size: Option<u64>,
    source_taken: Option<i64>,
    index: &'a [PhotosAsset],
    date_tolerance_secs: i64,
) -> Option<&'a PhotosAsset> {
    index.iter().find(|a| {
        !a.trashed
            && a.filename.eq_ignore_ascii_case(source_filename)
            && match (a.size, source_size) {
                (Some(asize), Some(ssize)) => asize == ssize,
                _ => false,
            }
            && match (a.taken, source_taken) {
                (Some(at), Some(st)) => (at - st).abs() <= date_tolerance_secs,
                _ => true,
            }
    })
}

/// Finds a locally-cached JPEG derivative for `uuid` under `<library>/resources/
/// derivatives`, without requiring the asset's full original to be downloaded.
/// Photos keeps these regardless of iCloud "Optimize Mac Storage" — confirmed on a
/// real library here: `originals/<hex>/` was empty for a non-trashed asset, but
/// `resources/derivatives/<hex>/<uuid>_..._c.jpeg` and `resources/derivatives/
/// masters/<hex>/<uuid>_..._c.jpeg` both had real, decodable JPEGs for it. That is
/// what lets Photos' own grid view render instantly without downloading anything,
/// and is exactly what a day-by-day gallery of Photos assets can piggyback on here.
///
/// Both locations can hold more than one derivative for the same UUID, and the
/// trailing numbers in their filenames are Apple's internal size-class codes, not a
/// reliable size ordering — verified: `_1_105_c` decoded to 768×1024, larger than
/// `_4_5005_c`'s 323×576, on the same library. So every candidate found is decoded
/// just far enough to read its dimensions (`imagesize`, no full JPEG decode) and the
/// one with the most pixels wins.
pub fn find_thumbnail(library_path: &Path, uuid: &str) -> Option<PathBuf> {
    let hex = uuid.chars().next()?.to_ascii_uppercase().to_string();
    let mut best: Option<(u64, PathBuf)> = None;
    for sub in ["resources/derivatives", "resources/derivatives/masters"] {
        let dir = library_path.join(sub).join(&hex);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(uuid) || !name.to_ascii_lowercase().ends_with(".jpeg") {
                continue;
            }
            let path = entry.path();
            let area = imagesize::size(&path)
                .map(|d| d.width as u64 * d.height as u64)
                .unwrap_or(0);
            if best.as_ref().is_none_or(|(a, _)| area > *a) {
                best = Some((area, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built `Photos.sqlite`-shaped database (just the columns this module
    /// reads, across the two real tables it joins) rather than a real library file:
    /// fully deterministic, portable to any machine/CI, and does not embed anyone's
    /// real photo filenames or dates.
    fn synthetic_photos_library(dir: &Path) -> PathBuf {
        let db_dir = dir.join("database");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("Photos.sqlite");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ZASSET (
                Z_PK INTEGER, ZADDITIONALATTRIBUTES INTEGER,
                ZUUID TEXT, ZFILENAME TEXT, ZDATECREATED REAL,
                ZWIDTH INTEGER, ZHEIGHT INTEGER, ZDURATION REAL,
                ZTRASHEDSTATE INTEGER, ZHIDDEN INTEGER
            );
            CREATE TABLE ZADDITIONALASSETATTRIBUTES (
                Z_PK INTEGER, ZORIGINALFILESIZE INTEGER
            );
            INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES
                (1, 4200000), (2, 9900000), (3, 4200000);
            INSERT INTO ZASSET VALUES
                (1, 1, 'uuid-1', 'IMG_0001.HEIC', 800000000.0, 3024, 4032, 0.0, 0, 0),
                (2, 2, 'uuid-2', 'IMG_0002.MOV',  800000100.5, 1920, 1080, 12.3, 0, 0),
                (3, 3, 'uuid-3', 'IMG_0003.HEIC', 800000200.0, 3024, 4032, 0.0, 1, 0),
                (4, NULL, 'uuid-4', 'IMG_0004.HEIC', NULL,     3024, 4032, 0.0, 0, 1);",
        )
        .unwrap();
        dir.to_path_buf()
    }

    #[test]
    fn reads_every_asset_with_correct_types_and_epoch_conversion() {
        let dir = std::env::temp_dir().join(format!("skimrr-photos-index-test-{}", std::process::id()));
        let lib = synthetic_photos_library(&dir);

        let index = read_photos_index(&lib).expect("synthetic library must be readable");
        assert_eq!(index.len(), 4);

        let photo = index.iter().find(|a| a.uuid == "uuid-1").unwrap();
        assert_eq!(photo.filename, "IMG_0001.HEIC");
        assert_eq!(photo.taken, Some(800_000_000 + CORE_DATA_TO_UNIX_EPOCH_SECS));
        assert_eq!(photo.size, Some(4_200_000));
        assert_eq!((photo.width, photo.height), (3024, 4032));
        assert!(!photo.is_video);
        assert!(!photo.trashed);

        let video = index.iter().find(|a| a.uuid == "uuid-2").unwrap();
        assert!(video.is_video, "a positive ZDURATION marks a video");

        let trashed = index.iter().find(|a| a.uuid == "uuid-3").unwrap();
        assert!(trashed.trashed);

        let no_date = index.iter().find(|a| a.uuid == "uuid-4").unwrap();
        assert_eq!(no_date.taken, None, "a NULL ZDATECREATED must not become a fake epoch date");
        assert_eq!(no_date.size, None, "no matching ZADDITIONALASSETATTRIBUTES row must not become a fake size");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_library_is_an_error_not_a_panic() {
        assert!(read_photos_index(Path::new("/no/such/library.photoslibrary")).is_err());
    }

    /// Not part of normal `cargo test`: reads whichever real library sits at the
    /// default macOS path for whoever runs this, which only exists (and is only
    /// readable — needs Full Disk Access) on a real Mac with a real Photos library.
    /// Never prints a filename or UUID, only aggregate counts, since this runs
    /// against someone's actual personal library.
    #[test]
    #[ignore = "reads the real, local Photos library; run explicitly with --ignored"]
    fn reads_the_real_library_on_this_machine() {
        let Ok(home) = std::env::var("HOME") else {
            eprintln!("skipping: no $HOME");
            return;
        };
        let lib = PathBuf::from(home).join("Pictures/Photos Library.photoslibrary");
        if !lib.exists() {
            eprintln!("skipping: no Photos library at {}", lib.display());
            return;
        }

        let index = read_photos_index(&lib).expect("real library must be readable with Full Disk Access granted");
        let total = index.len();
        let with_date = index.iter().filter(|a| a.taken.is_some()).count();
        let with_size = index.iter().filter(|a| a.size.is_some()).count();
        let videos = index.iter().filter(|a| a.is_video).count();
        let trashed = index.iter().filter(|a| a.trashed).count();
        println!("real library: {total} assets, {with_date} dated, {with_size} sized, {videos} videos, {trashed} trashed");

        assert!(total > 0, "a real library with any photos in it should yield at least one asset");
        assert!(with_date > 0, "at least some assets should carry a capture date");
        assert!(with_size > 0, "at least some assets should carry an original file size");
    }

    fn asset(filename: &str, size: Option<u64>, taken: Option<i64>, trashed: bool) -> PhotosAsset {
        PhotosAsset {
            uuid: "u".into(),
            filename: filename.into(),
            taken,
            size,
            width: 100,
            height: 100,
            is_video: false,
            trashed,
            hidden: false,
        }
    }

    #[test]
    fn already_in_photos_requires_filename_and_exact_size() {
        let index = vec![asset("img_0001.heic", Some(4_200_000), Some(1_000_000), false)];

        assert!(
            already_in_photos("IMG_0001.HEIC", Some(4_200_000), Some(1_000_000), &index, 3600).is_some(),
            "case-insensitive filename, exact size, date within tolerance"
        );
        assert!(
            already_in_photos("IMG_0001.HEIC", Some(4_200_001), Some(1_000_000), &index, 3600).is_none(),
            "one byte off must not match — size is the strong signal here"
        );
        assert!(
            already_in_photos("IMG_9999.HEIC", Some(4_200_000), Some(1_000_000), &index, 3600).is_none(),
            "different filename entirely"
        );
        assert!(
            already_in_photos("IMG_0001.HEIC", None, Some(1_000_000), &index, 3600).is_none(),
            "no size to compare against is not a match, not a wildcard"
        );
    }

    #[test]
    fn already_in_photos_treats_date_as_a_loose_bound_not_a_blocker() {
        let index = vec![asset("img_0001.heic", Some(4_200_000), Some(1_000_000), false)];

        assert!(
            already_in_photos("IMG_0001.HEIC", Some(4_200_000), Some(1_000_000 + 7200), &index, 3600).is_none(),
            "2h apart is outside a 1h sanity bound, even with filename+size agreeing"
        );
        assert!(
            already_in_photos("IMG_0001.HEIC", Some(4_200_000), None, &index, 3600).is_some(),
            "no date on the source side skips the date check rather than blocking the match"
        );
    }

    #[test]
    fn already_in_photos_ignores_trashed_assets() {
        let index = vec![asset("img_0001.heic", Some(4_200_000), Some(1_000_000), true)];
        assert!(
            already_in_photos("IMG_0001.HEIC", Some(4_200_000), Some(1_000_000), &index, 3600).is_none(),
            "a trashed asset isn't a reason to skip importing the source file again"
        );
    }

    #[test]
    fn escapes_backslashes_and_quotes() {
        assert_eq!(escape_applescript_string(r#"a "quoted" path"#), r#"a \"quoted\" path"#);
        assert_eq!(escape_applescript_string(r"C:\weird\path"), r"C:\\weird\\path");
    }

    #[test]
    fn import_script_embeds_every_path_as_a_posix_file() {
        let paths = vec![PathBuf::from("/a/one.jpg"), PathBuf::from("/a/two \"dup\".mov")];
        let script = import_script(&paths);
        assert!(script.contains(r#"POSIX file "/a/one.jpg""#));
        assert!(script.contains(r#"POSIX file "/a/two \"dup\".mov""#));
        assert!(script.contains("tell application \"Photos\""));
        assert!(script.contains("skip check duplicates true"));
    }

    #[test]
    fn offset_ahead_of_utc_is_subtracted() {
        // "14:00" read naively in a UTC+2 zone really meant 12:00 UTC.
        let naive_14_00_utc = 14 * 3600;
        assert_eq!(apply_timezone_offset(naive_14_00_utc, 2 * 3600), 12 * 3600);
    }

    #[test]
    fn offset_behind_utc_is_added() {
        let naive_10_00_utc = 10 * 3600;
        assert_eq!(apply_timezone_offset(naive_10_00_utc, -5 * 3600), 15 * 3600);
    }

    #[test]
    fn zero_offset_is_identity() {
        assert_eq!(apply_timezone_offset(1_700_000_000, 0), 1_700_000_000);
    }

    #[test]
    fn parses_both_exif_date_separators() {
        let expected = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        assert_eq!(parse_exif_datetime("2024:06:15 14:30:00"), Some(expected));
        assert_eq!(parse_exif_datetime("2024-06-15 14:30:00"), Some(expected));
    }

    #[test]
    fn garbage_exif_date_is_none() {
        assert_eq!(parse_exif_datetime("not a date"), None);
    }

    #[test]
    fn exif_reading_does_not_panic_on_a_real_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/portrait.jpg");
        let _ = exif_taken_naive(&path);
    }

    fn make_box(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        b.extend_from_slice(fourcc);
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn finds_creation_time_in_a_hand_built_mvhd_box() {
        let target_unix: i64 = 1_700_000_000;
        let mac_time = (target_unix + MAC_TO_UNIX_EPOCH_SECS) as u32;

        let mut mvhd_payload = vec![0u8, 0, 0, 0]; // version 0, flags 0
        mvhd_payload.extend_from_slice(&mac_time.to_be_bytes());
        let mvhd = make_box(b"mvhd", &mvhd_payload);
        let moov = make_box(b"moov", &mvhd);
        let ftyp = make_box(b"ftyp", b"isom\0\0\0\0isom");

        let mut file = ftyp;
        file.extend_from_slice(&moov);

        let dir = std::env::temp_dir().join(format!("skimrr-mvhd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("synthetic.mp4");
        std::fs::write(&path, &file).unwrap();

        assert_eq!(mp4_creation_time(&path), Some(target_unix));
        assert_eq!(read_naive_taken(&path), Some(target_unix));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_creation_time_from_a_real_ffmpeg_mp4() {
        let dir = std::env::temp_dir().join(format!("skimrr-fusion-ffmpeg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("clip.mp4");
        let built = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=10:duration=1",
                // ffmpeg's mp4 muxer leaves `creation_time` at 0 unless explicitly
                // told to set it — unlike a real phone camera, which always writes
                // its own capture time. Without this the field genuinely is 0, which
                // is exactly what the first run of this test caught.
                "-metadata", "creation_time=now",
            ])
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !built {
            eprintln!("skipping: no working `ffmpeg` CLI available");
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let taken = mp4_creation_time(&out).expect("an ffmpeg-written mp4 must carry an mvhd creation_time");
        assert!(
            (taken - now).abs() < 300,
            "creation_time should read as roughly now: got {taken}, now is {now}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 1×1 JPEG, just large enough to have real, decodable dimensions — the point
    /// of this test is the file-picking logic, not image content.
    fn write_tiny_jpeg(path: &Path, w: u32, h: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let img = image::RgbImage::new(w, h);
        img.save_with_format(path, image::ImageFormat::Jpeg).unwrap();
    }

    #[test]
    fn find_thumbnail_prefers_the_largest_decoded_candidate_across_both_locations() {
        let dir = std::env::temp_dir().join(format!("skimrr-thumbnail-test-{}", std::process::id()));
        let uuid = "0283FD35-7126-4035-ADC4-DC6BA3A8505C";

        // Smaller candidate in `derivatives/<hex>/`, larger one in
        // `derivatives/masters/<hex>/` — the naming scheme's own numbers must not be
        // trusted as a size order, only the actual decoded dimensions.
        write_tiny_jpeg(&dir.join("resources/derivatives/0").join(format!("{uuid}_4_5005_c.jpeg")), 50, 50);
        write_tiny_jpeg(&dir.join("resources/derivatives/masters/0").join(format!("{uuid}_1_105_c.jpeg")), 200, 200);
        // A different UUID in the same bucket must never be picked up.
        write_tiny_jpeg(&dir.join("resources/derivatives/0").join("11111111-0000-0000-0000-000000000000_1_1_c.jpeg"), 999, 999);

        let found = find_thumbnail(&dir, uuid).expect("a candidate exists in both locations");
        assert!(found.to_string_lossy().contains("masters"), "the larger (200×200) candidate must win: {found:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_thumbnail_is_none_when_nothing_matches() {
        let dir = std::env::temp_dir().join(format!("skimrr-thumbnail-empty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_thumbnail(&dir, "AAAAAAAA-0000-0000-0000-000000000000").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not part of normal `cargo test`: confirms real assets on this machine's real
    /// library actually resolve to a real, on-disk thumbnail file, the same way
    /// `reads_the_real_library_on_this_machine` verifies the index itself. Prints
    /// only a hit-rate count, never a filename or UUID.
    #[test]
    #[ignore = "reads the real, local Photos library; run explicitly with --ignored"]
    fn finds_thumbnails_for_most_real_assets() {
        let Ok(home) = std::env::var("HOME") else {
            eprintln!("skipping: no $HOME");
            return;
        };
        let lib = PathBuf::from(home).join("Pictures/Photos Library.photoslibrary");
        if !lib.exists() {
            eprintln!("skipping: no Photos library at {}", lib.display());
            return;
        }
        let index = read_photos_index(&lib).expect("real library must be readable");
        let sample: Vec<&PhotosAsset> = index.iter().filter(|a| !a.trashed).take(200).collect();
        let hits = sample.iter().filter(|a| find_thumbnail(&lib, &a.uuid).is_some()).count();
        println!("thumbnails found for {hits}/{} sampled assets", sample.len());
        assert!(hits > 0, "at least some real assets should have a locally cached derivative");
    }
}
