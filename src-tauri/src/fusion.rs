//! Reading an Apple Photos library from the outside: the asset rows of its
//! `Photos.sqlite` index, and the JPEG derivatives it caches on disk for them.
//!
//! Read-only by design. Nothing here writes to the library, and nothing needs the
//! originals to be downloaded — which matters, because a library with iCloud
//! "Optimize Mac Storage" on holds mostly derivatives locally.

use std::path::{Path, PathBuf};

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
    /// Decimal degrees, absent for an asset Photos has no position for.
    pub lat: Option<f64>,
    pub lon: Option<f64>,
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
/// What Photos writes into both coordinate columns for an asset it has no position
/// for. Verified on a real library: 878 of 4269 assets carry exactly this, and not one
/// row is NULL — so the absent case has to be recognised by value, not by nullability.
const NO_POSITION: f64 = -180.0;

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
             a.ZDURATION, a.ZTRASHEDSTATE, a.ZHIDDEN, x.ZORIGINALFILESIZE, \
             a.ZLATITUDE, a.ZLONGITUDE \
             FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES x \
             ON a.ZADDITIONALATTRIBUTES = x.Z_PK",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let core_data_date: Option<f64> = row.get(2)?;
            let size: Option<i64> = row.get(8)?;
            let lat: f64 = row.get(9)?;
            let lon: f64 = row.get(10)?;
            let located =
                lat != NO_POSITION && lon != NO_POSITION && lat.abs() <= 90.0 && lon.abs() <= 180.0;
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
                lat: located.then_some(lat),
                lon: located.then_some(lon),
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
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
                ZTRASHEDSTATE INTEGER, ZHIDDEN INTEGER,
                ZLATITUDE REAL, ZLONGITUDE REAL
            );
            CREATE TABLE ZADDITIONALASSETATTRIBUTES (
                Z_PK INTEGER, ZORIGINALFILESIZE INTEGER
            );
            INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES
                (1, 4200000), (2, 9900000), (3, 4200000);
            INSERT INTO ZASSET VALUES
                (1, 1, 'uuid-1', 'IMG_0001.HEIC', 800000000.0, 3024, 4032, 0.0, 0, 0, 48.8566, 2.3522),
                (2, 2, 'uuid-2', 'IMG_0002.MOV',  800000100.5, 1920, 1080, 12.3, 0, 0, -180.0, -180.0),
                (3, 3, 'uuid-3', 'IMG_0003.HEIC', 800000200.0, 3024, 4032, 0.0, 1, 0, 35.6762, 139.6503),
                (4, NULL, 'uuid-4', 'IMG_0004.HEIC', NULL,     3024, 4032, 0.0, 0, 1, -180.0, -180.0);",
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

        assert_eq!(photo.lat, Some(48.8566));
        assert_eq!(photo.lon, Some(2.3522));

        let video = index.iter().find(|a| a.uuid == "uuid-2").unwrap();
        assert!(video.is_video, "a positive ZDURATION marks a video");
        // Photos writes -180 into both columns rather than NULL when it has no
        // position. Read literally that is a real point in the Pacific, so the
        // sentinel has to be recognised by value.
        assert_eq!(video.lat, None, "the -180 sentinel must read as absent, not as a place");
        assert_eq!(video.lon, None);

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
