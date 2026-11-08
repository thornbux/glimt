//! Thumbnails for the file list, shared with other programs through the freedesktop thumbnail
//! cache (`~/.cache/thumbnails/normal`, or `large` for big thumbnails). Existing thumbnails are
//! always read; new ones are written there only when "Save thumbnails" is on.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The freedesktop sizes: "normal" at most 128×128 pixels, "large" 256×256.
const SIZES: [(u32, &str); 2] = [(128, "normal"), (256, "large")];

/// Markers of files no thumbnail could be made of, as the spec says: tried again by a new
/// version, or when the file changes.
const FAILED_DIR: &str = concat!("fail/glimt-", env!("CARGO_PKG_VERSION"));

/// The thumbnail of `path`: from the cache if it is newer than the file, else made from the image.
pub fn thumbnail(
    path: &Path,
    rotate: bool,
    save: bool,
    large: bool,
) -> Result<image::RgbaImage, String> {
    thumbnail_in(cache_dir(), path, rotate, save, large)
}

fn thumbnail_in(
    cache: Option<PathBuf>,
    path: &Path,
    rotate: bool,
    save: bool,
    large: bool,
) -> Result<image::RgbaImage, String> {
    if !crate::images::decode::is_regular_file(path) {
        return Err("not a regular file".into());
    }
    let (size, folder) = SIZES[large as usize];
    // The time is in whole seconds, as the spec has it, so the size tells apart two photos of a
    // burst that took each other's name in the same second (Rename Many).
    let stamp = std::fs::metadata(path).ok().and_then(|m| {
        let time = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some((time.as_secs(), m.len()))
    });
    let uri = format!("file://{}", uri_path(path));
    let file = cache.as_ref().map(|c| cache_file(c, folder, &uri));
    let failed = cache.as_ref().map(|c| cache_file(c, FAILED_DIR, &uri));
    if let (Some(file), Some(failed), Some(stamp)) = (&file, &failed, stamp) {
        // The shared cache holds thumbnails turned upright, so with Auto Rotate off it is not
        // read: the list would show turned what the image shows as stored. Whether a file can
        // be read at all does not depend on it.
        // shortcut: every thumbnail made again with Auto Rotate off; reading the cache for images
        // without an EXIF turn would need each format's orientation, if that is ever slow.
        if rotate && let Some(img) = read_cached(file, stamp) {
            return Ok(img);
        }
        if read_cached(failed, stamp).is_some() {
            return Err("no thumbnail could be made before".into());
        }
    }
    // A JPEG at 1/8 while decoding: under 1 MB for a 12 MP photo instead of 38. A decoder that
    // panics on the file fails here, inside, so the failure is marked like any other and the
    // file is not decoded again at every visit.
    let made = crate::images::decode::catch_panic_as_error(|| {
        let (img, icc, _) = crate::images::decode::decode_shrunk(path, [size, size], rotate)?;
        Ok(crate::images::colour::to_srgb(img, icc.as_deref()).into_rgba8())
    });
    // The spec forbids thumbnails of the thumbnails themselves. Others show them as made, so
    // one not turned upright by EXIF (Auto Rotate off) is not shared; the failure mark, Glimt's
    // own, is.
    if save
        && let (Some(cache), Some(file), Some(failed), Some(stamp)) =
            (&cache, &file, &failed, stamp)
        && !path.starts_with(cache)
    {
        match &made {
            Ok(img) if rotate => drop(write_thumbnail(file, img, &uri, stamp)),
            Ok(_) => {}
            Err(e) if should_mark_failed(path, e) => drop(write_thumbnail(
                failed,
                &image::RgbaImage::new(1, 1),
                &uri,
                stamp,
            )),
            Err(_) => {}
        }
    }
    made
}

/// `<cache>/normal/<md5 of the URI>.png`
fn cache_file(cache: &Path, folder: &str, uri: &str) -> PathBuf {
    let name: String = md5(uri.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    cache.join(folder).join(name + ".png")
}

/// Returns the cached thumbnail in `file` if it was made from the file as it is now, by its
/// modification time in seconds and its size, `stamp`. The thumbnail's Thumb::MTime must be
/// that time. Its Thumb::Size, which the spec leaves optional, must be that size when it is
/// there, so a thumbnail another program wrote without it is still used.
fn read_cached(file: &Path, (mtime, size): (u64, u64)) -> Option<image::RgbaImage> {
    let bytes = crate::images::decode::read_limited(file).ok()?;
    let reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .ok()?;
    let texts = &reader.info().uncompressed_latin1_text;
    let value = |key: &str| {
        (texts.iter().find(|t| t.keyword == key)).map(|t| t.text.trim().parse::<u64>().ok())
    };
    let fresh = value("Thumb::MTime") == Some(Some(mtime))
        && value("Thumb::Size").is_none_or(|s| s == Some(size));
    fresh.then_some(())?;
    crate::images::decode::from_memory(&bytes)
        .ok()
        .map(|i| i.into_rgba8())
}

/// Writes a thumbnail as the spec says: PNG with Thumb::URI, Thumb::MTime and Thumb::Size of
/// `stamp` (the time and size), mode 600, and renamed into place so other programs never read a
/// half-written file.
#[allow(clippy::disallowed_methods)] // the cache's own file, replaced on purpose
fn write_thumbnail(
    file: &Path,
    img: &image::RgbaImage,
    uri: &str,
    (mtime, size): (u64, u64),
) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // This Glimt and the thread in the name: several workers and windows may write at once.
    let tmp = file.with_extension(format!(
        "{}.{:?}.tmp",
        crate::files::file_io::instance_tag(),
        std::thread::current().id()
    ));
    // Encoded in memory first: the encoder writes its last chunk when it is dropped, where a
    // full disk would go unnoticed and a cut-off PNG land in the cache other programs share.
    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let encoded = (|| {
        enc.add_text_chunk("Thumb::URI".into(), uri.into())?;
        enc.add_text_chunk("Thumb::MTime".into(), mtime.to_string())?;
        enc.add_text_chunk("Thumb::Size".into(), size.to_string())?;
        enc.add_text_chunk("Software".into(), "Glimt".into())?;
        let mut writer = enc.write_header()?;
        writer.write_image_data(img.as_raw())?;
        writer.finish()
    })();
    encoded.map_err(std::io::Error::other)?;
    // As every file Glimt writes: made new beside it, never through a link planted at its name;
    // private, as the shared cache's spec asks.
    crate::files::file_io::write_beside(
        &tmp,
        file,
        |out| {
            use std::io::Write;
            out.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            out.write_all(&png)
        },
        // The cache's own file, replaced on purpose.
        |tmp| std::fs::rename(tmp, file),
    )
}

/// The shared cache. A Flatpak points XDG_CACHE_HOME to a folder of its own; the shared one is
/// the host's.
fn cache_dir() -> Option<std::path::PathBuf> {
    crate::files::config::host_xdg_dir("XDG_CACHE_HOME", ".cache", "thumbnails")
}

/// Removes the cached thumbnails of files that are gone, and returns (removed, looked at). A
/// thumbnail is removed only when the file's folder is still there, so thumbnails of a disk that is
/// not plugged in are kept.
pub fn remove_orphaned_thumbnails() -> (usize, usize) {
    cache_dir().map_or((0, 0), |cache| remove_orphaned_thumbnails_in(&cache))
}

/// `remove_orphaned_thumbnails` in the thumbnail cache `cache`. Older Glimts' folders of failure
/// markers are of no use to this one, so each of them goes whole and counts as one removed.
fn remove_orphaned_thumbnails_in(cache: &Path) -> (usize, usize) {
    let (mut removed, mut seen) = (0, 0);
    let own = Path::new(FAILED_DIR).file_name();
    for entry in (std::fs::read_dir(cache.join("fail")).into_iter().flatten()).flatten() {
        let name = entry.file_name();
        // remove_dir_all removes a link there, never what it points to.
        #[allow(clippy::disallowed_methods)] // an old Glimt's folder in the cache
        if name.as_encoded_bytes().starts_with(b"glimt-")
            && Some(name.as_os_str()) != own
            && std::fs::remove_dir_all(entry.path()).is_ok()
        {
            (seen, removed) = (seen + 1, removed + 1);
        }
    }
    for folder in SIZES.map(|s| s.1).into_iter().chain([FAILED_DIR]) {
        for entry in std::fs::read_dir(cache.join(folder))
            .into_iter()
            .flatten()
            .flatten()
        {
            seen += 1;
            let Some(path) = thumb_source(&entry.path()) else {
                continue;
            };
            #[allow(clippy::disallowed_methods)] // a thumbnail whose image is gone
            if !path.exists()
                && path.parent().is_some_and(Path::is_dir)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    (removed, seen)
}

/// The file a cached thumbnail was made from, from its Thumb::URI (file:// only).
fn thumb_source(thumb: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let file = std::io::BufReader::new(crate::images::decode::open_regular(thumb).ok()?);
    let reader = png::Decoder::new(file).read_info().ok()?;
    let info = reader.info();
    let uri = info
        .uncompressed_latin1_text
        .iter()
        .find(|t| t.keyword == "Thumb::URI")?;
    let encoded = uri.text.strip_prefix("file://")?.as_bytes();
    let bytes = crate::model::collections::percent_decoded(encoded);
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Percent-encode a path as GLib's g_filename_to_uri does, so the cache names match other
/// programs'.
pub(crate) fn uri_path(path: &Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => (b as char).to_string(),
            b'-' | b'_' | b'.' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' => {
                (b as char).to_string()
            }
            b'+' | b',' | b';' | b'=' | b':' | b'@' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// MD5 (RFC 1321), only for the cache file names the freedesktop spec prescribes.
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 4 * 4] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend((data.len() as u64 * 8).to_le_bytes());
    let mut h = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    for chunk in msg.chunks(64) {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            (a, d, c) = (d, c, b);
            b = b.wrapping_add(f.rotate_left(S[i / 16 * 4 + i % 4]));
        }
        for (h, v) in h.iter_mut().zip([a, b, c, d]) {
            *h = h.wrapping_add(v);
        }
    }
    let mut out = [0; 16];
    for (o, v) in out.chunks_mut(4).zip(h) {
        o.copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// Returns whether a file that gave `error` is marked as having no thumbnail. Not when ffmpeg or an
/// added format is not there: it may be installed later; nor when the file could not be read
/// (a share or a stick away for a moment): the system's errors say "(os error N)", a file that
/// is no picture does not. A program that read from such a share says so in its own words, or
/// was ended by the time limit while the share woke up, and is not marked either. Others are
/// marked, or tried again on every visit (2,000 TypeScript sources named .ts, which are video
/// endings too, would start ffmpeg 4,000 times).
fn should_mark_failed(path: &Path, error: &str) -> bool {
    let away = [
        crate::files::external::TIMED_OUT,
        "Input/output error",
        "Transport endpoint is not connected",
        "Host is down",
    ];
    !(needs_program(path) && program_missing(error))
        && !error.contains("(os error")
        && !away.iter().any(|m| error.contains(m))
}

/// Whether `path` is read by another program: ffmpeg (HEIC, AVIF, video) or an added format.
fn needs_program(path: &Path) -> bool {
    crate::files::external::ffmpeg_decodes(path)
        || crate::images::added_formats::for_path(path).is_some()
}

/// Whether `error` says the program was not found (or may not run), not that the file is bad.
fn program_missing(error: &str) -> bool {
    [
        "No such file or directory",
        "command not found",
        "Permission denied",
    ]
    .iter()
    .any(|m| error.contains(m))
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    /// Cleaning the shared thumbnail cache removes the thumbnail of a file that is gone from a
    /// folder still there, and keeps the one of a file there and those of a folder not there
    /// (a disk not plugged in): it deletes in a cache file managers share.
    #[test]
    fn cleans_only_thumbnails_of_files_gone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-clean-{}", std::process::id()));
        let (cache, photos) = (dir.join("cache"), dir.join("photos"));
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("kept.png"), "x").unwrap();
        let img = image::RgbaImage::new(2, 2);
        let thumb = |name: &str, of: &std::path::Path| {
            let uri = format!("file://{}", super::uri_path(of));
            super::write_thumbnail(&cache.join("normal").join(name), &img, &uri, (1, 1)).unwrap();
        };
        thumb("kept.png", &photos.join("kept.png"));
        thumb("gone.png", &photos.join("gone.png"));
        thumb("unplugged.png", &dir.join("not-plugged-in/x.png"));
        // An older Glimt's failure markers go whole, and this one's and other programs' stay.
        let fail = cache.join("fail");
        for folder in ["glimt-0.1.0", "gnome-thumbnail-factory"] {
            std::fs::create_dir_all(fail.join(folder)).unwrap();
            std::fs::write(fail.join(folder).join("x.png"), "x").unwrap();
        }
        std::fs::create_dir_all(cache.join(super::FAILED_DIR)).unwrap();
        assert_eq!(super::remove_orphaned_thumbnails_in(&cache), (2, 4));
        assert!(
            !fail.join("glimt-0.1.0").exists() && fail.join("gnome-thumbnail-factory").exists()
        );
        assert!(cache.join(super::FAILED_DIR).exists());
        let left: Vec<_> = (std::fs::read_dir(cache.join("normal")).unwrap())
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(left.contains(&"kept.png".into()) && left.contains(&"unplugged.png".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// ffmpeg not installed is no reason to give up on a file; ffmpeg failing on it is.
    #[test]
    fn tells_a_missing_program_from_a_bad_file() {
        let missing = "video, HEIC and AVIF need ffmpeg: No such file or directory (os error 2)";
        assert!(super::program_missing(missing));
        assert!(!super::program_missing(
            "Invalid data found when processing input"
        ));
        // A photo whose share went away for a moment is tried again; a broken one is not.
        let jpg = std::path::Path::new("/a.jpg");
        let away = std::io::Error::from_raw_os_error(5).to_string();
        assert!(!super::should_mark_failed(jpg, &away));
        // So is a video whose program said the share failed, or ran out of time while it woke.
        let mp4 = std::path::Path::new("/a.mp4");
        assert!(!super::should_mark_failed(
            mp4,
            "/a.mp4: Input/output error"
        ));
        assert!(!super::should_mark_failed(
            mp4,
            crate::files::external::TIMED_OUT
        ));
        assert!(super::should_mark_failed(
            jpg,
            "Format error decoding Jpeg: invalid marker"
        ));
    }

    /// Thumbnails broken in thousands of ways, as the shared cache may hold them: their source
    /// read without a panic.
    #[test]
    fn survives_broken_thumbnails() {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 2, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.add_text_chunk("Thumb::URI".into(), "file:///home/u/a%20b.jpg".into())
            .unwrap();
        enc.write_header()
            .unwrap()
            .write_image_data(&[0; 16])
            .unwrap();
        let file =
            std::env::temp_dir().join(format!("glimt-test-thumb-{}.png", std::process::id()));
        std::fs::write(&file, &png).unwrap();
        assert_eq!(super::thumb_source(&file), Some("/home/u/a b.jpg".into()));
        for bytes in crate::support::test_support::broken(&png) {
            std::fs::write(&file, bytes).unwrap();
            super::thumb_source(&file);
        }
        std::fs::remove_file(file).unwrap();
    }

    fn hex(data: &[u8]) -> String {
        super::md5(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn md5_matches_rfc_1321() {
        assert_eq!(hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        // 56 and 64 bytes: the padding spills into an extra block.
        assert_eq!(hex(&[b'a'; 56]), "3b0c8ac703f828b04c6c197006d17218");
        assert_eq!(hex(&[b'a'; 64]), "014842d480b571495a4a0363793f7367");
    }

    /// A link planted where the thumbnail's temporary file goes empties nothing it points to.
    #[test]
    fn a_planted_link_at_the_temporary_name_is_not_followed() {
        let dir = std::env::temp_dir().join(format!("glimt-thumbs-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, victim) = (dir.join("abc.png"), dir.join("victim"));
        std::fs::write(&victim, "keep").unwrap();
        let tmp = file.with_extension(format!(
            "{}.{:?}.tmp",
            crate::files::file_io::instance_tag(),
            std::thread::current().id()
        ));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        super::write_thumbnail(&file, &image::RgbaImage::new(2, 2), "file:///a.jpg", (7, 1))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(file.is_file() && !tmp.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_and_reads_cache() {
        let dir = std::env::temp_dir().join(format!("glimt-thumbs-{}", std::process::id()));
        let file = super::cache_file(&dir, "normal", "file:///a%20b.jpg");
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 2, 3, 255]));
        super::write_thumbnail(&file, &img, "file:///a%20b.jpg", (42, 9)).unwrap();
        assert_eq!(super::read_cached(&file, (42, 9)).unwrap(), img);
        assert!(super::read_cached(&file, (43, 9)).is_none());
        assert!(super::read_cached(&file, (42, 10)).is_none());
        // Another program's thumbnail without Thumb::Size is used by its time alone.
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 1, 1);
        enc.set_color(png::ColorType::Rgba);
        enc.add_text_chunk("Thumb::MTime".into(), "42".into())
            .unwrap();
        enc.write_header()
            .unwrap()
            .write_image_data(&[0; 4])
            .unwrap();
        std::fs::write(&file, &png).unwrap();
        assert!(super::read_cached(&file, (42, 9)).is_some());
        assert_eq!(
            super::uri_path(std::path::Path::new("/a b/(1)+æ.jpg")),
            "/a%20b/(1)+%C3%A6.jpg"
        );
        // The cached file names its source, also with spaces and non-ASCII letters.
        let named = super::cache_file(&dir, "normal", "file:///x");
        let uri = format!(
            "file://{}",
            super::uri_path(std::path::Path::new("/a b/æ.jpg"))
        );
        super::write_thumbnail(&named, &img, &uri, (1, 1)).unwrap();
        assert_eq!(super::thumb_source(&named), Some("/a b/æ.jpg".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo put at another's name in the same second gets its own thumbnail, not the other's.
    /// The cache told files by their name and their time in whole seconds, so after Rename Many
    /// renumbered a burst, the list showed a frame that was gone.
    #[test]
    fn a_file_of_the_same_second_at_the_name_gets_its_own_thumbnail() {
        let dir = std::env::temp_dir().join(format!("glimt-test-burst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (cache, photo) = (dir.join("cache"), dir.join("burst.png"));
        let save = |size: u32, colour: [u8; 4]| {
            image::RgbaImage::from_pixel(size, size, image::Rgba(colour))
                .save(&photo)
                .unwrap();
            let file = std::fs::File::options().write(true).open(&photo).unwrap();
            file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
                .unwrap();
        };
        let thumb = || super::thumbnail_in(Some(cache.clone()), &photo, true, true, false);
        save(8, [255, 0, 0, 255]);
        assert_eq!(thumb().unwrap().get_pixel(0, 0).0, [255, 0, 0, 255]);
        save(9, [0, 0, 255, 255]);
        assert_eq!(thumb().unwrap().get_pixel(0, 0).0, [0, 0, 255, 255]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file no thumbnail can be made of gets a marker, and is not decoded again until it
    /// changes.
    #[test]
    fn marks_failed_files() {
        let dir = std::env::temp_dir().join(format!("glimt-thumbs-fail-{}", std::process::id()));
        let cache = dir.join("cache");
        let broken = dir.join("broken.png");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&broken, b"not a picture").unwrap();
        // With Auto Rotate off too: whether a file can be read does not depend on it.
        let make = || super::thumbnail_in(Some(cache.clone()), &broken, false, true, false);
        assert!(make().unwrap_err() != "no thumbnail could be made before");
        let uri = format!("file://{}", super::uri_path(&broken));
        assert!(super::cache_file(&cache, super::FAILED_DIR, &uri).is_file());
        assert_eq!(make().unwrap_err(), "no thumbnail could be made before");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
