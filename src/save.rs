//! Saving a turned image: the pixels turned, the EXIF kept with orientation 1 and the XMP kept,
//! in the same format (a lossy WebP gets them back in RIFF chunks).

use crate::colour::to_srgb;
use crate::decode::load_icc;
use crate::{exif, has_ext};
use image::ImageDecoder;
use std::path::Path;

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
    // Through a symlink, the file it points to: the new file is renamed over that, not the link.
    let path = &std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let png = has_ext(path, &["png"]);
    if !png && !has_ext(path, &["webp"]) {
        return Err("only JPEG, PNG and WebP can store the rotation".into());
    }
    if animated(path)? {
        return Err("animated images are not turned".into());
    }
    let kind = match png {
        true => Kind::Png,
        false => Kind::WebP(webp_lossless(path) == Some(false)),
    };
    let (exif, xmp, stored_quarter) = upright_metadata(path, rotate)?;
    let (img, icc) = load_icc(path, rotate)?;
    let img = turned(img, turns, mirrored);
    let quarter = (turns % 2 == 1) != stored_quarter;
    let out = encode(path, img, icc, exif, xmp, kind, quarter, webp_quality)?;
    crate::config::replace_existing(path, out).map_err(|e| e.to_string())
}

/// How a copy of an image is made (Edit → Crop, File → Save As): as the view shows it (turned
/// by `turns` and `mirrored`, and by EXIF with `rotate`), a part of it, smaller, in a format.
pub struct CopyAs {
    pub rotate: bool,
    pub turns: u8,
    pub mirrored: bool,
    /// The part `[x, y, width, height]` of the image as shown.
    pub crop: Option<[u32; 4]>,
    /// The longest side at most, in pixels; never enlarged.
    pub long_side: Option<u32>,
    /// `None`: as the original, which is JPEG, PNG or WebP (lossy as it was), else PNG.
    pub format: Option<Format>,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    /// Without the EXIF block (camera, date, place), the XMP and the sidecar.
    pub strip: bool,
}

/// A format a copy is saved in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Jpeg,
    Png,
    WebP,
    WebPLossy,
}

impl Format {
    /// The extension of its files.
    pub fn ext(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::WebP | Format::WebPLossy => "webp",
        }
    }

    /// The format `path` would be copied in "as the original".
    pub fn of(path: &Path) -> Format {
        if crate::files::is_jpeg(path) {
            Format::Jpeg
        } else if has_ext(path, &["webp"]) && webp_lossless(path) == Some(false) {
            Format::WebPLossy
        } else if has_ext(path, &["webp"]) {
            Format::WebP
        } else {
            Format::Png
        }
    }
}

/// The copy of `path` that `how` asks for, encoded, and its format. With its profile, and
/// unless stripped its EXIF (upright) and XMP: JPEG and a lossy WebP at their quality (the
/// WebP through ffmpeg), PNG and lossless WebP without loss.
pub fn copy_bytes(path: &Path, how: &CopyAs) -> Result<(Vec<u8>, Format), String> {
    if animated(path)? {
        return Err("animated images are not saved as a copy".into());
    }
    let format = how.format.unwrap_or_else(|| Format::of(path));
    let kind = match format {
        Format::Jpeg => Kind::Jpeg(how.jpeg_quality),
        Format::Png => Kind::Png,
        Format::WebP => Kind::WebP(false),
        Format::WebPLossy => Kind::WebP(true),
    };
    let (exif, xmp, _) = match how.strip {
        true => (None, None, false),
        false => upright_metadata(path, how.rotate)?,
    };
    let (img, icc) = load_icc(path, how.rotate)?;
    let mut img = turned(img, how.turns, how.mirrored);
    if let Some([x, y, width, height]) = how.crop {
        if width == 0 || height == 0 || x + width > img.width() || y + height > img.height() {
            return Err("the part is outside the image".into());
        }
        img = img.crop_imm(x, y, width, height);
    }
    if let Some(long) = how.long_side {
        img = smaller(img, long.max(1))?;
    }
    // A PNG's other chunks hold as they were: a DPI is no different for a part. Not stripped,
    // its text chunks may say where and by whom.
    let from = if how.strip { Path::new("") } else { path };
    let out = encode(from, img, icc, exif, xmp, kind, false, how.webp_quality)?;
    Ok((out, format))
}

/// `img` with its longest side `long` at most, by Lanczos3: sharp, for a copy kept.
fn smaller(img: image::DynamicImage, long: u32) -> Result<image::DynamicImage, String> {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= long {
        return Ok(img);
    }
    let scale = long as f64 / w.max(h) as f64;
    let size = |side: u32| ((side as f64 * scale).round() as u32).max(1);
    let mut small = image::DynamicImage::new(size(w), size(h), img.color());
    let opts = fast_image_resize::ResizeOptions::new().resize_alg(
        fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3),
    );
    fast_image_resize::Resizer::new()
        .resize(&img, &mut small, &opts)
        .map_err(|e| e.to_string())?;
    Ok(small)
}

/// Save the part `how.crop` of the image as a new file beside it, "NAME-cropped.EXT" or
/// "NAME-cropped-2.EXT" …, never over another; its sidecar is copied too. Returns the new file.
pub fn save_cropped(path: &Path, how: &CopyAs) -> Result<std::path::PathBuf, String> {
    let (out, format) = copy_bytes(path, how)?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    // The original's own spelling (.jpeg, .JPG) where the format is the same.
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) if how.format.is_none() && Format::of(path) == format && format != Format::Png => {
            e.to_owned()
        }
        Some(e) if format == Format::Png && e.eq_ignore_ascii_case("png") => e.to_owned(),
        _ => format.ext().to_owned(),
    };
    for n in 1.. {
        let name = match n {
            1 => format!("{stem}-cropped.{ext}"),
            _ => format!("{stem}-cropped-{n}.{ext}"),
        };
        let new = path.with_file_name(name);
        match crate::config::create_file(&new, &out) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
            Ok(()) => {
                if !how.strip {
                    let _ = crate::meta::follow(path, &new, true);
                }
                return Ok(new);
            }
        }
    }
    unreachable!()
}

/// File → Save As: the copy `how` asks for, saved as `to`, which the file dialog let replace
/// a file there. Its extension made the format's, and then never over a file the dialog did
/// not ask about; never the image itself. Returns the file saved.
pub fn save_as(path: &Path, how: &CopyAs, to: &Path) -> Result<std::path::PathBuf, String> {
    let format = how.format.unwrap_or_else(|| Format::of(path));
    let ext = to.extension().and_then(|e| e.to_str()).unwrap_or("");
    let matches = match format {
        Format::Jpeg => ["jpg", "jpeg"].iter().any(|x| x.eq_ignore_ascii_case(ext)),
        f => f.ext().eq_ignore_ascii_case(ext),
    };
    let to = match matches {
        true => to.to_owned(),
        false => {
            let mut name = to.file_name().unwrap_or_default().to_owned();
            name.push(".");
            name.push(format.ext());
            to.with_file_name(name)
        }
    };
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same(path, &to) {
        return Err("choose another name: the image itself is not changed".into());
    }
    let (out, _) = copy_bytes(path, how)?;
    let written = match matches {
        true => crate::config::replace_file(&to, &out),
        false => crate::config::create_file(&to, &out),
    };
    written.map_err(|e| format!("{}: {e}", to.display()))?;
    if !how.strip {
        let _ = crate::meta::follow(path, &to, true);
    }
    Ok(to)
}

/// How an image is written: as a PNG, a WebP (lossy or not) or a JPEG of a quality.
#[derive(Clone, Copy)]
enum Kind {
    Png,
    WebP(bool),
    Jpeg(u8),
}

/// Whether `path` is animated: only the first frame would be written back. Asked by what the
/// file is, as it is read by its content: an animated GIF named a.png is still animated, a
/// JPEG named so is not.
fn animated(path: &Path) -> Result<bool, String> {
    let format = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .format();
    let file = || std::fs::File::open(path).map(std::io::BufReader::new);
    Ok(match format {
        Some(image::ImageFormat::Gif) => true,
        Some(image::ImageFormat::Png) => (file().ok())
            .and_then(|f| image::codecs::png::PngDecoder::new(f).ok())
            .is_some_and(|d| d.is_apng().unwrap_or(false)),
        Some(image::ImageFormat::WebP) => (file().ok())
            .and_then(|f| image::codecs::webp::WebPDecoder::new(f).ok())
            .is_some_and(|d| d.has_animation()),
        _ => false,
    })
}

/// The EXIF block (camera, date, place) and XMP of `path`, for pixels written as the view
/// showed them: their orientation set upright, also with Auto Rotate off, where the view did
/// not turn them by it, else other programs would turn them again. And whether the stored
/// orientation turned the image a quarter, with Auto Rotate.
type Metadata = (Option<Vec<u8>>, Option<Vec<u8>>, bool);
fn upright_metadata(path: &Path, rotate: bool) -> Result<Metadata, String> {
    let (mut exif, mut xmp, stored) = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .map(|mut d| {
            let exif = d.exif_metadata().ok().flatten();
            let stored = d.orientation().ok();
            (exif, d.xmp_metadata().ok().flatten(), stored)
        })
        .unwrap_or_default();
    if let Some(exif) = &mut exif {
        exif::set_upright(exif)?;
    }
    if let Some(xmp) = &mut xmp {
        exif::set_xmp_orientation(xmp, 1);
    }
    let quarter = rotate && stored.is_some_and(crate::decode::quarter_turn);
    Ok((exif, xmp, quarter))
}

/// `img` turned as the view: mirrored first, then turned clockwise `turns` quarters.
fn turned(img: image::DynamicImage, turns: u8, mirrored: bool) -> image::DynamicImage {
    let img = if mirrored { img.fliph() } else { img };
    match turns % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    }
}

/// `img` encoded as `kind` with the profile, EXIF and XMP; a PNG keeps the other chunks of
/// `path` (`quarter`: turned a quarter since). Encoded in memory: an encoder that owns its
/// file flushes it on drop, where a full disk would go unnoticed and a cut-off file replace
/// the photo.
#[allow(clippy::too_many_arguments)]
fn encode(
    path: &Path,
    img: image::DynamicImage,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
    kind: Kind,
    quarter: bool,
    webp_quality: u8,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let (width, height) = (img.width(), img.height());
    if let Kind::WebP(true) = kind {
        // ffmpeg hands the WebP over in memory, in sRGB as it drops the profile.
        let img = to_srgb(img, icc.as_deref());
        let mut webp = lossy_webp(&img, webp_quality)?;
        if let Some(exif) = &exif {
            webp = webp_with_chunk(&webp, b"EXIF", exif, width, height)?;
        }
        if let Some(xmp) = &xmp {
            webp = webp_with_chunk(&webp, b"XMP ", xmp, width, height)?;
        }
        return Ok(webp);
    }
    let mut out = Vec::new();
    let written = match kind {
        Kind::Png => {
            let mut enc = image::codecs::png::PngEncoder::new(&mut out);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            match exif.map(|e| enc.set_exif_metadata(e)) {
                Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
                _ => img.write_with_encoder(enc),
            }
        }
        Kind::Jpeg(quality) => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            // JPEG has no alpha, nor more than 8 bits.
            let img = image::DynamicImage::from(img.into_rgb8());
            match exif.map(|e| enc.set_exif_metadata(e)) {
                Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
                _ => img.write_with_encoder(enc),
            }
        }
        Kind::WebP(_) => {
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
        }
    };
    written.map_err(|e| e.to_string())?;
    // The rest of what the PNG said of itself: its text (a comment, an AI tool's prompt), its
    // DPI and its gamma; else a 300 dpi scan prints at 72 after a turn. Only from a PNG.
    if let Kind::Png = kind
        && has_ext(path, &["png"])
        && let Ok(old) = std::fs::read(path)
    {
        out = png_with_chunks(&out, &old, quarter);
    }
    if let Some(xmp) = &xmp {
        out = match kind {
            Kind::Png => png_with_xmp(&out, xmp)?,
            Kind::Jpeg(_) => jpeg_with_xmp(&out, xmp)?,
            Kind::WebP(_) => webp_with_chunk(&out, b"XMP ", xmp, width, height)?,
        };
    }
    Ok(out)
}

/// `jpeg` with `xmp` in an APP1 segment of Adobe's name, right after the start (SOI).
fn jpeg_with_xmp(jpeg: &[u8], xmp: &[u8]) -> Result<Vec<u8>, String> {
    const NAME: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let len = 2 + NAME.len() + xmp.len();
    if !jpeg.starts_with(&[0xFF, 0xD8]) || len > 0xFFFF {
        return Err("not a JPEG file, or its XMP too long".into());
    }
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend((len as u16).to_be_bytes());
    out.extend(NAME);
    out.extend(xmp);
    out.extend(&jpeg[2..]);
    Ok(out)
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

/// The PNG `new` with the chunks of `old` that still hold for its pixels put after its header:
/// text, physical size, time and colour space. Not the profile, EXIF or XMP, which are written
/// anew, nor what depends on how the pixels are stored (palette, transparency, bit depth).
fn png_with_chunks(new: &[u8], old: &[u8], quarter: bool) -> Vec<u8> {
    const KEPT: [&[u8; 4]; 8] = [
        b"tEXt", b"zTXt", b"iTXt", b"pHYs", b"tIME", b"gAMA", b"cHRM", b"sRGB",
    ];
    let (end, mut kept) = (33, Vec::new());
    // Turned a quarter: pHYs, the pixels per unit across and down, swapped too (a fax-like scan
    // of 204×98 dpi would print stretched else), also for a square picture.
    let swapped = quarter;
    let mut at = 8;
    while let Some(len) = old.get(at..at + 4) {
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let Some(chunk) = old.get(at..at.saturating_add(12).saturating_add(len)) else {
            break;
        };
        let name = &chunk[4..8];
        let xmp = name == b"iTXt" && chunk[8..].starts_with(b"XML:com.adobe.xmp\0");
        // ImageMagick's copies of the EXIF and XMP, with the old orientation, which exiv2
        // (Geeqie, digiKam) reads: the turned file would be turned again there.
        let old_profile = [
            &b"Raw profile type exif"[..],
            b"Raw profile type APP1",
            b"Raw profile type xmp",
        ]
        .iter()
        .any(|key| matches!(name, b"tEXt" | b"zTXt" | b"iTXt") && chunk[8..].starts_with(key));
        if name == b"pHYs" && swapped && len == 9 {
            let body = [&b"pHYs"[..], &chunk[12..16], &chunk[8..12], &chunk[16..17]].concat();
            kept.extend(9u32.to_be_bytes());
            kept.extend(&body);
            kept.extend(crc32(&body).to_be_bytes());
        } else if KEPT.iter().any(|k| *k == name) && !xmp && !old_profile {
            kept.extend_from_slice(chunk);
        }
        // Text may come after the pixels too: read up to the end.
        if name == b"IEND" {
            break;
        }
        at += 12 + len;
    }
    if new.get(12..16) != Some(&b"IHDR"[..]) || new.len() < end {
        return new.to_vec();
    }
    [&new[..end], &kept, &new[end..]].concat()
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

/// `img` as a lossy WebP of `quality`, encoded by ffmpeg (the `image` crate writes only
/// lossless ones), in memory: its errors read beside it, and stopped after `external::LIMIT`.
pub fn lossy_webp(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut child = crate::external::stoppable_command("ffmpeg")
        .process_group(0)
        .args([
            "-v", "error", "-f", "png_pipe", "-i", "-", "-c:v", "libwebp",
        ])
        .args([
            "-quality",
            &quality.to_string(),
            "-frames:v",
            "1",
            "-f",
            "webp",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("a lossy WebP needs ffmpeg to be saved: {e}"))?;
    // Written in a thread of its own, while its output is read: neither pipe fills and stops it.
    let mut stdin = child.stdin.take();
    let feed = std::thread::spawn(move || drop(stdin.as_mut().map(|i| i.write_all(&png))));
    let max = crate::decode::MAX_DECODE;
    let (webp, err, ok) = crate::external::output_of(&mut child, max, crate::external::LIMIT);
    let _ = feed.join();
    match webp {
        Some(webp) if ok && !webp.is_empty() => Ok(webp),
        _ if err.trim().is_empty() => Err("ffmpeg made no WebP".into()),
        _ => Err(err.trim().to_owned()),
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
    /// A damaged PNG or WebP given a chunk: refused or carried on as far as it goes, no panic.
    #[test]
    fn survives_broken_files_given_a_chunk() {
        let mut png = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        for bytes in crate::broken(&png) {
            let _ = super::png_with_xmp(&bytes, b"<x:xmpmeta/>");
        }
        let mut webp = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut webp),
                image::ImageFormat::WebP,
            )
            .unwrap();
        for bytes in crate::broken(&webp) {
            let _ = super::webp_with_chunk(&bytes, b"EXIF", b"Exif", 4, 2);
        }
    }

    /// A PNG turned keeps its text and DPI, and nothing tied to how the old pixels were stored;
    /// broken ones are survived.
    #[test]
    fn keeps_png_chunks() {
        use image::ImageEncoder;
        let chunk = |name: &[u8], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            let body = [name, data].concat();
            c.extend(&body);
            c.extend(super::crc32(&body).to_be_bytes());
            c
        };
        let mut new = Vec::new();
        image::codecs::png::PngEncoder::new(&mut new)
            .write_image(&[0; 2 * 2 * 3], 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut old = new[..33].to_vec();
        old.extend(chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]));
        old.extend(chunk(b"tRNS", &[0, 0, 0, 0, 0, 0]));
        old.extend(chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<x/>"));
        old.extend(&new[33..new.len() - 12]);
        old.extend(chunk(b"tEXt", b"Comment\0hello"));
        old.extend(chunk(b"zTXt", b"Raw profile type exif\0\0x"));
        old.extend(chunk(b"tEXt", b"Raw profile type xmp\0x"));
        old.extend(&new[new.len() - 12..]);
        let out = super::png_with_chunks(&new, &old, false);
        let has = |name: &[u8]| out.windows(4).any(|w| w == name);
        assert!(has(b"pHYs") && has(b"tEXt"));
        assert!(!out.windows(16).any(|w| w == b"Raw profile type"));
        assert!(!has(b"tRNS") && !has(b"iTXt") && !has(b"zTXt"));
        let dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&out)).unwrap();
        assert_eq!(image::ImageDecoder::dimensions(&dec), (2, 2));
        // Turned a quarter (a 2×3 picture now): pHYs across and down swapped, its CRC right.
        let mut tall = Vec::new();
        image::codecs::png::PngEncoder::new(&mut tall)
            .write_image(&[0; 2 * 3 * 3], 2, 3, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut wide = Vec::new();
        image::codecs::png::PngEncoder::new(&mut wide)
            .write_image(&[0; 3 * 2 * 3], 3, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut old = wide[..33].to_vec();
        old.extend(chunk(b"pHYs", &[0, 0, 0, 204, 0, 0, 0, 98, 1]));
        old.extend(&wide[33..]);
        let out = super::png_with_chunks(&tall, &old, true);
        let at = out.windows(4).position(|w| w == b"pHYs").unwrap();
        assert_eq!(&out[at + 4..at + 13], &[0, 0, 0, 98, 0, 0, 0, 204, 1]);
        image::codecs::png::PngDecoder::new(std::io::Cursor::new(&out)).unwrap();
        for broken in crate::broken(&old) {
            super::png_with_chunks(&new, &broken, true);
        }
    }

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
        crate::config::replace_file(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        // A folder cannot be replaced by a file.
        assert!(crate::config::replace_file(&dir.join("busy.png"), b"new").is_err());
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

    /// A part saved as a new JPEG beside the image, as the view shows it, with its XMP; a
    /// second never over the first, and the image itself untouched.
    #[test]
    fn saves_a_cropped_copy() {
        use image::ImageDecoder;
        let dir = std::env::temp_dir().join(format!("glimt-crop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let (red, blue) = (image::Rgb([255, 0, 0]), image::Rgb([0, 0, 255]));
        let mut jpeg = Vec::new();
        image::RgbImage::from_fn(16, 8, |x, _| if x < 8 { red } else { blue })
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let xmp = b"<x:xmpmeta>keywords</x:xmpmeta>";
        std::fs::write(&file, super::jpeg_with_xmp(&jpeg, xmp).unwrap()).unwrap();
        let before = std::fs::read(&file).unwrap();
        let colour = |path: &std::path::Path| {
            let img = image::open(path).unwrap().into_rgb8();
            let p = img.get_pixel(img.width() / 2, img.height() / 2).0;
            (img.dimensions(), p[0] > 200 && p[2] < 60)
        };
        let how = |turns, part| super::CopyAs {
            rotate: true,
            turns,
            mirrored: false,
            crop: Some(part),
            long_side: None,
            format: None,
            jpeg_quality: 92,
            webp_quality: 90,
            strip: false,
        };
        // The right half: blue.
        let new = super::save_cropped(&file, &how(0, [8, 0, 8, 8])).unwrap();
        assert_eq!(new, dir.join("a-cropped.jpg"));
        assert_eq!(colour(&new), ((8, 8), false));
        let mut dec = image::ImageReader::open(&new)
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.xmp_metadata().unwrap().as_deref(), Some(&xmp[..]));
        // Turned a quarter clockwise, the left (red) half is on top.
        let new = super::save_cropped(&file, &how(1, [0, 0, 8, 8])).unwrap();
        assert_eq!(new, dir.join("a-cropped-2.jpg"));
        assert_eq!(colour(&new), ((8, 8), true));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(super::save_cropped(&file, &how(0, [8, 0, 9, 8])).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Save As: smaller and in another format, its extension made the format's, the metadata
    /// kept or left out, and never the image itself.
    #[test]
    fn saves_as_another_format_and_size() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-save-as-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x01\0\0\0\0\0\0";
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::from_pixel(400, 200, image::Rgb([200, 30, 30]))
            .write_with_encoder(enc)
            .unwrap();
        let how = |format, strip| super::CopyAs {
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: None,
            long_side: Some(100),
            format,
            jpeg_quality: 90,
            webp_quality: 90,
            strip,
        };
        // Named without an extension: the format's is added.
        let to = super::save_as(
            &file,
            &how(Some(super::Format::Jpeg), false),
            &dir.join("b"),
        );
        let to = to.unwrap();
        assert_eq!(to, dir.join("b.jpg"));
        let mut dec = image::ImageReader::open(&to)
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (100, 50));
        assert!(dec.exif_metadata().unwrap().is_some());
        let to = super::save_as(
            &file,
            &how(Some(super::Format::WebP), true),
            &dir.join("c.webp"),
        );
        let mut dec = image::ImageReader::open(to.unwrap())
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (100, 50));
        assert!(dec.exif_metadata().unwrap().is_none());
        // Never the image itself, also asked for by name.
        assert!(super::save_as(&file, &how(None, false), &file).is_err());
        assert_eq!(image::image_dimensions(&file).unwrap(), (400, 200));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// "a.png" and "a.webp" saved turned at the same time: each keeps its own pixels and format.
    #[test]
    fn saves_same_names_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, webp) = (dir.join("a.png"), dir.join("a.webp"));
        assert_ne!(
            crate::config::tmp_path(&png),
            crate::config::tmp_path(&webp)
        );
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
