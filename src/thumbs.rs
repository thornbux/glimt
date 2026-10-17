//! Thumbnails for the file list, shared with other programs through the freedesktop thumbnail
//! cache (`~/.cache/thumbnails/normal`, or `large` for big thumbnails). Existing thumbnails are
//! always read; new ones are written there only when "Save thumbnails" is on.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The freedesktop sizes: "normal" at most 128×128 pixels, "large" 256×256.
const SIZES: [(u32, &str); 2] = [(128, "normal"), (256, "large")];
/// Markers of files no thumbnail could be made of, as the spec says: tried again by a new
/// version, or when the file changes.
const FAILED: &str = concat!("fail/glimt-", env!("CARGO_PKG_VERSION"));

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
    if !crate::decode::is_file(path) {
        return Err("not a regular file".into());
    }
    let (size, folder) = SIZES[large as usize];
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let uri = format!("file://{}", uri_path(path));
    let file = cache.as_ref().map(|c| cache_file(c, folder, &uri));
    let failed = cache.as_ref().map(|c| cache_file(c, FAILED, &uri));
    if let (Some(file), Some(failed), Some(mtime)) = (&file, &failed, mtime) {
        if let Some(img) = read_cached(file, mtime) {
            return Ok(img);
        }
        if read_cached(failed, mtime).is_some() {
            return Err("no thumbnail could be made before".into());
        }
    }
    // A JPEG at 1/8 while decoding: under 1 MB for a 12 MP photo instead of 38.
    let made = match crate::decode::scaled_jpeg(path, [size, size], rotate) {
        Some((img, icc, _)) => Ok((img, icc)),
        None => crate::decode::load_icc(path, rotate),
    }
    .and_then(|(img, icc)| {
        let img = crate::decode::shrink(img, [size, size])?;
        Ok(crate::colour::to_srgb(img, icc.as_deref()).into_rgba8())
    });
    // The spec forbids thumbnails of the thumbnails themselves. Others show them as made, so
    // one not turned upright by EXIF (Auto Rotate off) is not shared.
    if save
        && rotate
        && let (Some(cache), Some(file), Some(failed), Some(mtime)) =
            (&cache, &file, &failed, mtime)
        && !path.starts_with(cache)
    {
        match &made {
            Ok(img) => drop(write(file, img, &uri, mtime)),
            // Not when ffmpeg or a decoder plugin failed: it may be installed later.
            Err(_)
                if !crate::external::is_heif(path)
                    && !crate::external::is_video(path)
                    && crate::decoders::for_path(path).is_none() =>
            {
                drop(write(failed, &image::RgbaImage::new(1, 1), &uri, mtime))
            }
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

/// A cached thumbnail, if it was made from the file as it is now (same Thumb::MTime).
fn read_cached(file: &Path, mtime: u64) -> Option<image::RgbaImage> {
    let bytes = std::fs::read(file).ok()?;
    let reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .ok()?;
    let fresh =
        reader.info().uncompressed_latin1_text.iter().any(|t| {
            t.keyword == "Thumb::MTime" && t.text.trim().parse::<u64>().ok() == Some(mtime)
        });
    fresh.then_some(())?;
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .ok()
        .map(|i| i.into_rgba8())
}

/// Write a thumbnail as the spec says: PNG with Thumb::URI and Thumb::MTime, mode 600, and
/// renamed into place so other programs never read a half-written file.
fn write(file: &Path, img: &image::RgbaImage, uri: &str, mtime: u64) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // This Glimt and the thread in the name: several workers and windows may write at once.
    let tmp = file.with_extension(format!(
        "{}.{:?}.tmp",
        crate::remote::instance_id(),
        std::thread::current().id()
    ));
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(out), img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let written = (|| {
        enc.add_text_chunk("Thumb::URI".into(), uri.into())?;
        enc.add_text_chunk("Thumb::MTime".into(), mtime.to_string())?;
        enc.add_text_chunk("Software".into(), "Glimt".into())?;
        enc.write_header()?.write_image_data(img.as_raw())
    })();
    match written {
        Ok(()) => std::fs::rename(&tmp, file),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(std::io::Error::other(e))
        }
    }
}

/// The shared cache. A Flatpak points XDG_CACHE_HOME to a folder of its own; the shared one is
/// the host's.
fn cache_dir() -> Option<std::path::PathBuf> {
    let var = match std::env::var_os("FLATPAK_ID") {
        Some(_) => "HOST_XDG_CACHE_HOME",
        None => "XDG_CACHE_HOME",
    };
    crate::config::xdg_dir(var, ".cache", "thumbnails")
}

/// Remove the cached thumbnails of files that are gone: (removed, looked at). A thumbnail is
/// removed only when the file's folder is still there, so thumbnails of a disk that is not
/// plugged in are kept.
pub fn clean() -> (usize, usize) {
    let Some(cache) = cache_dir() else {
        return (0, 0);
    };
    let (mut removed, mut seen) = (0, 0);
    for folder in SIZES.map(|s| s.1).into_iter().chain([FAILED]) {
        for entry in std::fs::read_dir(cache.join(folder))
            .into_iter()
            .flatten()
            .flatten()
        {
            seen += 1;
            let Some(path) = thumb_source(&entry.path()) else {
                continue;
            };
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
    let file = std::io::BufReader::new(std::fs::File::open(thumb).ok()?);
    let reader = png::Decoder::new(file).read_info().ok()?;
    let info = reader.info();
    let uri = info
        .uncompressed_latin1_text
        .iter()
        .find(|t| t.keyword == "Thumb::URI")?;
    let encoded = uri.text.strip_prefix("file://")?.as_bytes();
    let bytes = crate::collections::percent_decoded(encoded);
    // SAFETY: on Unix any bytes are a valid OsStr.
    Some(PathBuf::from(unsafe {
        std::ffi::OsStr::from_encoded_bytes_unchecked(&bytes)
    }))
}

/// Percent-encode a path as GLib's g_filename_to_uri does, so the cache names match other programs'.
fn uri_path(path: &Path) -> String {
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

#[cfg(test)]
mod tests {
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
        for bytes in crate::broken(&png) {
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

    #[test]
    fn writes_and_reads_cache() {
        let dir = std::env::temp_dir().join(format!("glimt-thumbs-{}", std::process::id()));
        let file = super::cache_file(&dir, "normal", "file:///a%20b.jpg");
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 2, 3, 255]));
        super::write(&file, &img, "file:///a%20b.jpg", 42).unwrap();
        assert_eq!(super::read_cached(&file, 42).unwrap(), img);
        assert!(super::read_cached(&file, 43).is_none());
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
        super::write(&named, &img, &uri, 1).unwrap();
        assert_eq!(super::thumb_source(&named), Some("/a b/æ.jpg".into()));
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
        let make = || super::thumbnail_in(Some(cache.clone()), &broken, true, true, false);
        assert!(make().unwrap_err() != "no thumbnail could be made before");
        let uri = format!("file://{}", super::uri_path(&broken));
        assert!(super::cache_file(&cache, super::FAILED, &uri).is_file());
        assert_eq!(make().unwrap_err(), "no thumbnail could be made before");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
