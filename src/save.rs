//! Saving a turned image: the pixels turned, the EXIF kept with orientation 1 and the XMP kept,
//! in the same format (a lossy WebP gets them back in RIFF chunks).

use crate::colour::to_srgb;
use crate::decode::load_icc;
use crate::external::command;
use crate::{exif, has_ext};
use image::ImageDecoder;
use std::path::{Path, PathBuf};

/// Where a new version of `path` is written before it is renamed over it: its whole name, this
/// Glimt's instance ID and ".glimt-tmp", so that "a.png" and "a.webp", saved at the same time,
/// or one file saved by two windows at once, never share one.
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.glimt-tmp", crate::remote::instance_id()));
    path.with_file_name(name)
}

/// Replace the image `path` with `bytes` through `tmp_path`: written and on disk (`sync_all`)
/// before the rename, with the old file's permissions. Any failure takes the temporary file away
/// and leaves the old file as it was, also on a full disk.
pub fn replace_image(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = tmp_path(path);
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|_| f.sync_all()))
        .and_then(|_| match std::fs::metadata(path) {
            Ok(meta) => std::fs::set_permissions(&tmp, meta.permissions()),
            Err(_) => Ok(()),
        })
        .and_then(|_| std::fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Save the view's rotation in a PNG or WebP, which have no orientation field to set: the
/// pixels are turned and the file written again with its ICC profile, EXIF block and XMP
/// (keywords, comment, alt text other programs wrote into it). PNG and
/// lossless WebP lose nothing; a lossy WebP is encoded again by ffmpeg at `webp_quality`, in
/// sRGB as ffmpeg drops the profile, and gets its EXIF block back afterwards. The new file
/// replaces the old in one rename.
pub fn save_rotated(
    path: &Path,
    rotate: bool,
    turns: u8,
    mirrored: bool,
    webp_quality: u8,
) -> Result<(), String> {
    use image::ImageEncoder;
    // Through a symlink, the file it points to: the new file is renamed over that, not the link.
    let path = &std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let png = has_ext(path, &["png"]);
    if !png && !has_ext(path, &["webp"]) {
        return Err("only JPEG, PNG and WebP can store the rotation".into());
    }
    // Only the first frame would be written back. Asked by what the file is, as it is read below
    // by its content: an animated GIF named a.png is still animated, a JPEG named so is not.
    let format = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .format();
    let file = || std::fs::File::open(path).map(std::io::BufReader::new);
    let animated = match format {
        Some(image::ImageFormat::Gif) => true,
        Some(image::ImageFormat::Png) => (file().ok())
            .and_then(|f| image::codecs::png::PngDecoder::new(f).ok())
            .is_some_and(|d| d.is_apng().unwrap_or(false)),
        Some(image::ImageFormat::WebP) => (file().ok())
            .and_then(|f| image::codecs::webp::WebPDecoder::new(f).ok())
            .is_some_and(|d| d.has_animation()),
        _ => false,
    };
    if animated {
        return Err("animated images are not turned".into());
    }
    // The EXIF block (camera, date, place) goes into the new file. The pixels are written as the
    // view showed them, so its orientation is set upright, also with Auto Rotate off, where the
    // view did not turn them by it: else other programs would turn them again.
    let (mut exif, xmp) = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .map(|mut d| {
            let exif = d.exif_metadata().ok().flatten();
            (exif, d.xmp_metadata().ok().flatten())
        })
        .unwrap_or_default();
    let lossy = !png && webp_lossless(path) == Some(false);
    if let Some(exif) = &mut exif {
        exif::set_upright(exif)?;
    }
    let (mut img, icc) = load_icc(path, rotate)?;
    if mirrored {
        img = img.fliph();
    }
    img = match turns % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    };
    if lossy {
        // ffmpeg writes its file; read back, with the EXIF block added, it replaces the image.
        let tmp = tmp_path(path);
        let img = to_srgb(img, icc.as_deref());
        let made = lossy_webp(&img, &tmp, webp_quality)
            .and_then(|_| std::fs::read(&tmp).map_err(|e| e.to_string()))
            .and_then(|webp| match &exif {
                Some(exif) => webp_with_chunk(&webp, b"EXIF", exif, img.width(), img.height()),
                None => Ok(webp),
            })
            .and_then(|webp| match &xmp {
                Some(xmp) => webp_with_chunk(&webp, b"XMP ", xmp, img.width(), img.height()),
                None => Ok(webp),
            });
        return match made {
            Ok(webp) => replace_image(path, &webp),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        };
    }
    // Encoded in memory first: an encoder that owns its file flushes it on drop, where a full
    // disk would go unnoticed and a cut-off file replace the photo.
    let mut out = Vec::new();
    let (width, height) = (img.width(), img.height());
    let written = if png {
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        if let Some(icc) = icc {
            let _ = enc.set_icc_profile(icc);
        }
        match exif.map(|e| enc.set_exif_metadata(e)) {
            Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
            _ => img.write_with_encoder(enc),
        }
    } else {
        let mut enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
        if let Some(icc) = icc {
            let _ = enc.set_icc_profile(icc);
        }
        // The WebP encoder takes 8-bit RGB or RGBA.
        let img = if img.color().has_alpha() {
            image::DynamicImage::from(img.into_rgba8())
        } else {
            image::DynamicImage::from(img.into_rgb8())
        };
        match exif.map(|e| enc.set_exif_metadata(e)) {
            Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
            _ => img.write_with_encoder(enc),
        }
    };
    written.map_err(|e| e.to_string())?;
    if let Some(xmp) = &xmp {
        out = match png {
            true => png_with_xmp(&out, xmp)?,
            false => webp_with_chunk(&out, b"XMP ", xmp, width, height)?,
        };
    }
    replace_image(path, &out)
}

/// `png` with `xmp` in an iTXt chunk named as Adobe's XMP spec says, right after the header
/// (IHDR), where readers that stop at the pixels find it too.
fn png_with_xmp(png: &[u8], xmp: &[u8]) -> Result<Vec<u8>, String> {
    // The 8-byte signature, then IHDR: its length 13, its name, 13 bytes and a CRC.
    let end = 33;
    if png.get(12..16) != Some(&b"IHDR"[..]) || png.len() < end {
        return Err("not a PNG file".into());
    }
    let mut chunk = b"iTXt".to_vec();
    // Keyword, no compression, no language, no translated keyword, then the text.
    chunk.extend(b"XML:com.adobe.xmp\0\0\0\0\0");
    chunk.extend(xmp);
    let mut out = png[..end].to_vec();
    out.extend(((chunk.len() - 4) as u32).to_be_bytes());
    out.extend(&chunk);
    out.extend(crc32(&chunk).to_be_bytes());
    out.extend(&png[end..]);
    Ok(out)
}

/// The CRC that PNG chunks end with (ISO 3309), bit by bit: run once per saved image.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Whether a WebP file is lossless (a VP8L image) or lossy (VP8); `None` if neither is found.
pub fn webp_lossless(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0; 12];
    file.read_exact(&mut head).ok()?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WEBP" {
        return None;
    }
    // The chunks after the header: an extended file has VP8X, ICCP, ALPH … before the image.
    let mut chunk = [0; 8];
    while file.read_exact(&mut chunk).is_ok() {
        match &chunk[..4] {
            b"VP8L" => return Some(true),
            b"VP8 " => return Some(false),
            _ => {
                let size = u32::from_le_bytes(chunk[4..].try_into().ok()?) as i64;
                file.seek(SeekFrom::Current(size + size % 2)).ok()?;
            }
        }
    }
    None
}

/// Write `img` as a lossy WebP at `quality` (0–100) through ffmpeg, which has libwebp.
pub fn lossy_webp(img: &image::DynamicImage, to: &Path, quality: u8) -> Result<(), String> {
    use std::io::Write;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut child = command("ffmpeg")
        .args([
            "-v", "error", "-y", "-f", "png_pipe", "-i", "-", "-c:v", "libwebp",
        ])
        .args([
            "-quality",
            &quality.to_string(),
            "-frames:v",
            "1",
            "-f",
            "webp",
        ])
        .arg(to)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("a lossy WebP needs ffmpeg to be saved: {e}"))?;
    let _ = child.stdin.take().map(|mut i| i.write_all(&png));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(to);
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// `webp` with an EXIF or XMP chunk (`name`), at the end where the format puts them, EXIF
/// before XMP, and its flag set in the extended format's VP8X header, which is made for a
/// simple file.
pub fn webp_with_chunk(
    webp: &[u8],
    name: &[u8; 4],
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let flag = if name == b"EXIF" { 0x08 } else { 0x04 };
    if !webp.starts_with(b"RIFF") || webp.get(8..12) != Some(&b"WEBP"[..]) {
        return Err("not a WebP file".into());
    }
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let chunks = &webp[12..];
    // A lossless image tells its alpha in its header (bit 28 after the signature byte); the
    // extended header must say it too, or viewers show it opaque.
    let alpha = chunks.starts_with(b"VP8L") && chunks.get(12).is_some_and(|b| b & 0x10 != 0);
    let flag = if alpha { flag | 0x10 } else { flag };
    if chunks.starts_with(b"VP8X") {
        out.extend(chunks);
        *out.get_mut(20).ok_or("cut off")? |= flag;
    } else {
        out.extend(b"VP8X\x0a\0\0\0");
        out.extend([flag, 0, 0, 0]);
        out.extend(&width.saturating_sub(1).to_le_bytes()[..3]);
        out.extend(&height.saturating_sub(1).to_le_bytes()[..3]);
        out.extend(chunks);
    }
    out.extend(name);
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    // Chunks have an even length.
    if data.len() % 2 == 1 {
        out.push(0);
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// WebP files broken in thousands of ways: told lossless, lossy or neither without a panic
    /// or a loop.
    #[test]
    fn survives_broken_webp() {
        use image::ImageEncoder;
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0; 4 * 2 * 3], 4, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let file =
            std::env::temp_dir().join(format!("glimt-test-webp-{}.webp", std::process::id()));
        for bytes in crate::broken(&webp) {
            std::fs::write(&file, bytes).unwrap();
            super::webp_lossless(&file);
        }
        std::fs::remove_file(file).unwrap();
    }

    /// The new bytes replace the image with its permissions; a failed replace leaves no
    /// temporary file and the old one as it was.
    #[test]
    fn replaces_images_whole() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-replace-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("busy.png")).unwrap();
        std::fs::write(dir.join("busy.png/inside"), "x").unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        super::replace_image(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        // A folder cannot be replaced by a file.
        assert!(super::replace_image(&dir.join("busy.png"), b"new").is_err());
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// image writes lossless WebP (VP8L); anything else is not taken for one.
    #[test]
    fn tells_lossless_webp() {
        let file = std::env::temp_dir().join(format!("glimt-{}.webp", std::process::id()));
        image::RgbImage::new(4, 4).save(&file).unwrap();
        assert_eq!(super::webp_lossless(&file), Some(true));
        std::fs::write(&file, b"RIFF\x04\0\0\0WEBP").unwrap();
        assert_eq!(super::webp_lossless(&file), None);
        std::fs::remove_file(file).unwrap();
    }

    /// A PNG turned upright by its EXIF orientation and then a quarter turn keeps its EXIF,
    /// with an upright orientation, and its XMP.
    #[test]
    fn saves_rotated_png_with_exif() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-rotated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // IFD0 with Orientation 6 (turn 90° clockwise) and Make "Canon".
        let exif = b"MM\0*\0\0\0\x08\0\x02\x01\x0f\0\x02\0\0\0\x06\0\0\0\x26\
            \x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0Canon\0";
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::new(4, 2).write_with_encoder(enc).unwrap();
        // And XMP, as digiKam writes keywords into a PNG.
        let xmp = b"<x:xmpmeta>Oslo</x:xmpmeta>";
        let with_xmp = super::png_with_xmp(&std::fs::read(&file).unwrap(), xmp).unwrap();
        std::fs::write(&file, with_xmp).unwrap();
        super::save_rotated(&file, true, 1, false, 90).unwrap();
        let mut dec = image::ImageReader::open(&file)
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (4, 2));
        assert_eq!(
            dec.orientation().unwrap(),
            image::metadata::Orientation::NoTransforms
        );
        assert_eq!(dec.xmp_metadata().unwrap().as_deref(), Some(&xmp[..]));
        let rows = crate::exif::read(&file);
        assert!(rows.contains(&("Camera", "Canon".to_string())), "{rows:?}");
        // An animated PNG (an acTL chunk after IHDR) is refused and left as it was.
        let apng = dir.join("b.png");
        image::RgbImage::new(4, 2).save(&apng).unwrap();
        let mut bytes = std::fs::read(&apng).unwrap();
        let actl = [b"acTL".as_slice(), &[0, 0, 0, 1, 0, 0, 0, 0]].concat();
        let mut chunk = 8u32.to_be_bytes().to_vec();
        chunk.extend(&actl);
        chunk.extend(super::crc32(&actl).to_be_bytes());
        bytes.splice(33..33, chunk);
        std::fs::write(&apng, &bytes).unwrap();
        assert!(super::save_rotated(&apng, false, 1, false, 90).is_err());
        assert_eq!(std::fs::read(&apng).unwrap(), bytes);
        // A JPEG with a .png name is turned, and saved as a PNG.
        let misnamed = dir.join("c.png");
        image::RgbImage::new(4, 2)
            .save_with_format(&misnamed, image::ImageFormat::Jpeg)
            .unwrap();
        super::save_rotated(&misnamed, false, 1, false, 90).unwrap();
        assert_eq!(image::image_dimensions(&misnamed).unwrap(), (2, 4));
        // An animated GIF with a .png name is refused, as by its own name.
        let gif = dir.join("d.png");
        let frame = || image::Frame::new(image::RgbaImage::new(4, 2));
        image::codecs::gif::GifEncoder::new(std::fs::File::create(&gif).unwrap())
            .encode_frames([frame(), frame()])
            .unwrap();
        let before = std::fs::read(&gif).unwrap();
        assert!(super::save_rotated(&gif, false, 1, false, 90).is_err());
        assert_eq!(std::fs::read(&gif).unwrap(), before);
        // Through a symlink: the file it points to is turned, and the link stays a link.
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        super::save_rotated(&link, false, 1, false, 90).unwrap();
        assert!(link.is_symlink());
        assert_eq!(image::image_dimensions(&file).unwrap(), (2, 4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Saved as the view showed it, also with a mirroring orientation: turned by it with Auto
    /// Rotate on, as it is without, and in both cases with an upright orientation after.
    #[test]
    fn saves_rotated_as_seen() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let (red, blue) = (image::Rgb([255, 0, 0]), image::Rgb([0, 0, 255]));
        // Orientation 2: mirrored left to right.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0";
        for (auto_rotate, top) in [(false, red), (true, blue)] {
            let mut enc =
                image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
            enc.set_exif_metadata(exif.to_vec()).unwrap();
            image::RgbImage::from_fn(2, 1, |x, _| if x == 0 { red } else { blue })
                .write_with_encoder(enc)
                .unwrap();
            // A quarter turn clockwise: the left pixel of what the view shows goes on top.
            super::save_rotated(&file, auto_rotate, 1, false, 90).unwrap();
            let mut dec = image::ImageReader::open(&file)
                .unwrap()
                .into_decoder()
                .unwrap();
            assert_eq!(
                dec.orientation().unwrap(),
                image::metadata::Orientation::NoTransforms
            );
            let img = image::DynamicImage::from_decoder(dec).unwrap().into_rgb8();
            assert_eq!((img.dimensions(), *img.get_pixel(0, 0)), ((1, 2), top));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A simple WebP gets the extended header and an EXIF chunk that the decoder reads back.
    #[test]
    fn adds_exif_and_xmp() {
        use image::{ImageDecoder, ImageEncoder};
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0; 5 * 3 * 3], 5, 3, image::ExtendedColorType::Rgb8)
            .unwrap();
        let exif = b"II*\0\x08\0\0\0\0\0\0\0\0";
        let with = super::webp_with_chunk(&webp, b"EXIF", exif, 5, 3).unwrap();
        let xmp = b"<x:xmpmeta>keywords</x:xmpmeta>";
        let with = super::webp_with_chunk(&with, b"XMP ", xmp, 5, 3).unwrap();
        let mut dec = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(with)).unwrap();
        assert_eq!(dec.dimensions(), (5, 3));
        assert_eq!(dec.exif_metadata().unwrap().as_deref(), Some(&exif[..]));
        assert_eq!(dec.xmp_metadata().unwrap().as_deref(), Some(&xmp[..]));
        assert!(!dec.color_type().has_alpha());
        // One with alpha keeps it.
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0; 5 * 3 * 4], 5, 3, image::ExtendedColorType::Rgba8)
            .unwrap();
        let with = super::webp_with_chunk(&webp, b"EXIF", exif, 5, 3).unwrap();
        assert_eq!(with[20], 0x18);
        let dec = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(with)).unwrap();
        assert!(dec.color_type().has_alpha());
        // A PNG with XMP in an iTXt chunk, read back by the png decoder, which checks its CRC.
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[0; 5 * 3 * 3], 5, 3, image::ExtendedColorType::Rgb8)
            .unwrap();
        let png = super::png_with_xmp(&png, xmp).unwrap();
        let mut dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(png)).unwrap();
        assert_eq!(dec.xmp_metadata().unwrap().as_deref(), Some(&xmp[..]));
        assert_eq!(super::crc32(b"IEND"), 0xae42_6082);
    }

    /// "a.png" and "a.webp" saved turned at the same time: each keeps its own pixels and format.
    #[test]
    fn saves_same_names_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, webp) = (dir.join("a.png"), dir.join("a.webp"));
        assert_ne!(super::tmp_path(&png), super::tmp_path(&webp));
        image::RgbImage::from_pixel(1200, 800, image::Rgb([255, 0, 0]))
            .save(&png)
            .unwrap();
        image::RgbImage::from_pixel(1200, 800, image::Rgb([0, 0, 255]))
            .save(&webp)
            .unwrap();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for path in [&png, &webp] {
                let start = &start;
                s.spawn(move || {
                    start.wait();
                    super::save_rotated(path, false, 1, false, 90).unwrap();
                });
            }
        });
        let pixel = |p: &std::path::Path| {
            let img = image::open(p).unwrap().into_rgb8();
            (img.dimensions(), *img.get_pixel(0, 0))
        };
        assert_eq!(pixel(&png), ((800, 1200), image::Rgb([255, 0, 0])));
        assert_eq!(pixel(&webp), ((800, 1200), image::Rgb([0, 0, 255])));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
