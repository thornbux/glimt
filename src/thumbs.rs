//! Thumbnails for the file list, shared with other programs through the freedesktop thumbnail
//! cache (`~/.cache/thumbnails/normal`). Existing thumbnails are always read; new ones are
//! written there only when "Save thumbnails" is on.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The freedesktop "normal" size: at most 128×128 pixels.
pub const SIZE: u32 = 128;

/// The thumbnail of `path`: from the cache if it is newer than the file, else made from the image.
pub fn thumbnail(path: &Path, rotate: bool, save: bool) -> Result<image::RgbaImage, String> {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let uri = format!("file://{}", uri_path(path));
    let cache = crate::xdg_dir("XDG_CACHE_HOME", ".cache", "thumbnails");
    let file = cache.as_ref().map(|c| cache_file(c, &uri));
    if let (Some(file), Some(mtime)) = (&file, mtime)
        && let Some(img) = read_cached(file, mtime)
    {
        return Ok(img);
    }
    let img = crate::shrink(crate::load(path, rotate)?, [SIZE, SIZE])?.into_rgba8();
    // The spec forbids thumbnails of the thumbnails themselves.
    if save
        && let (Some(cache), Some(file), Some(mtime)) = (&cache, &file, mtime)
        && !path.starts_with(cache)
    {
        let _ = write(file, &img, &uri, mtime);
    }
    Ok(img)
}

/// `<cache>/normal/<md5 of the URI>.png`
fn cache_file(cache: &Path, uri: &str) -> PathBuf {
    let name: String = md5(uri.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    cache.join("normal").join(name + ".png")
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
    // Process and thread in the name: several workers may write at once.
    let tmp = file.with_extension(format!(
        "{}.{:?}.tmp",
        std::process::id(),
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
        let file = super::cache_file(&dir, "file:///a%20b.jpg");
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 2, 3, 255]));
        super::write(&file, &img, "file:///a%20b.jpg", 42).unwrap();
        assert_eq!(super::read_cached(&file, 42).unwrap(), img);
        assert!(super::read_cached(&file, 43).is_none());
        assert_eq!(
            super::uri_path(std::path::Path::new("/a b/(1)+æ.jpg")),
            "/a%20b/(1)+%C3%A6.jpg"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
