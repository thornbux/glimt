//! Saving a turned image: the pixels turned, the EXIF kept with orientation 1, in the same format
//! (a lossy WebP gets its EXIF back in a RIFF chunk).

use crate::colour::to_srgb;
use crate::decode::load_icc;
use crate::external::command;
use crate::{exif, has_ext};
use image::ImageDecoder;
use std::path::{Path, PathBuf};

/// Where a new version of `path` is written before it is renamed over it: its whole name and
/// ".glimt-tmp", so that "a.png" and "a.webp", which can be saved at the same time, never share one.
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".glimt-tmp");
    path.with_file_name(name)
}

/// Save the view's rotation in a PNG or WebP, which have no orientation field to set: the
/// pixels are turned and the file written again with its ICC profile and EXIF block. PNG and
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
    if !png {
        let file = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
        let dec = image::codecs::webp::WebPDecoder::new(file).map_err(|e| e.to_string())?;
        if dec.has_animation() {
            return Err("animated WebP files are not turned".into());
        }
    }
    // The EXIF block (camera, date, place) goes into the new file. The pixels are written as the
    // view showed them, so its orientation is set upright, also with Auto Rotate off, where the
    // view did not turn them by it: else other programs would turn them again.
    let mut exif = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .and_then(|mut d| d.exif_metadata().ok().flatten());
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
    let tmp = tmp_path(path);
    if lossy {
        let img = to_srgb(img, icc.as_deref());
        lossy_webp(&img, &tmp, webp_quality)?;
        // ffmpeg writes no EXIF: it is added to its file.
        if let Some(exif) = &exif {
            let added = std::fs::read(&tmp)
                .map_err(|e| e.to_string())
                .and_then(|webp| webp_with_exif(&webp, exif, img.width(), img.height()))
                .and_then(|webp| std::fs::write(&tmp, webp).map_err(|e| e.to_string()));
            if let Err(e) = added {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        return std::fs::rename(&tmp, path).map_err(|e| e.to_string());
    }
    let out = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
    let written = if png {
        let mut enc = image::codecs::png::PngEncoder::new(out);
        if let Some(icc) = icc {
            let _ = enc.set_icc_profile(icc);
        }
        match exif.map(|e| enc.set_exif_metadata(e)) {
            Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
            _ => img.write_with_encoder(enc),
        }
    } else {
        let mut enc = image::codecs::webp::WebPEncoder::new_lossless(out);
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
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
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

/// `webp` with an EXIF chunk, at the end where the format puts it, and its flag set in the
/// extended format's VP8X header, which is made for a simple file.
pub fn webp_with_exif(
    webp: &[u8],
    exif: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    if !webp.starts_with(b"RIFF") || webp.get(8..12) != Some(&b"WEBP"[..]) {
        return Err("not a WebP file".into());
    }
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let chunks = &webp[12..];
    if chunks.starts_with(b"VP8X") {
        out.extend(chunks);
        *out.get_mut(20).ok_or("cut off")? |= 0x08;
    } else {
        out.extend(b"VP8X\x0a\0\0\0\x08\0\0\0");
        out.extend(&width.saturating_sub(1).to_le_bytes()[..3]);
        out.extend(&height.saturating_sub(1).to_le_bytes()[..3]);
        out.extend(chunks);
    }
    out.extend(b"EXIF");
    out.extend((exif.len() as u32).to_le_bytes());
    out.extend(exif);
    // Chunks have an even length.
    if exif.len() % 2 == 1 {
        out.push(0);
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}
