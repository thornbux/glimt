//! Optional video support (`cargo build --features video`): a frame from the video is shown
//! as a preview, decoded by the `ffmpeg` program, so Glimt itself links no video codecs.

use std::path::Path;

pub const EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "mpg", "mpeg", "wmv", "flv", "3gp", "ts", "mts",
    "m2ts",
];

/// A frame one second in (the very first one is often black), or the first frame for
/// shorter videos. ffmpeg applies the rotation stored in the video.
pub fn preview(path: &Path) -> Result<image::DynamicImage, String> {
    frame(path, "1").or_else(|_| frame(path, "0"))
}

fn frame(path: &Path, at: &str) -> Result<image::DynamicImage, String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss", at, "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "ppm", "-"])
        .output()
        .map_err(|e| format!("video previews need ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            "no frame".into()
        } else {
            err
        });
    }
    parse_ppm(out.stdout)
}

/// A binary PPM as ffmpeg writes it: "P6", width, height and 255, each followed by one
/// whitespace byte, then the RGB pixels.
fn parse_ppm(mut data: Vec<u8>) -> Result<image::DynamicImage, String> {
    // Five parts: the four header fields, then the pixels (which may contain whitespace bytes).
    let mut parts = data.splitn(5, u8::is_ascii_whitespace);
    if parts.next() != Some(&b"P6"[..]) {
        return Err("not a binary PPM".into());
    }
    let field = |p: Option<&[u8]>| -> Option<u32> { std::str::from_utf8(p?).ok()?.parse().ok() };
    let (w, h, max) = (
        field(parts.next()),
        field(parts.next()),
        field(parts.next()),
    );
    let header = data.len() - parts.next().map_or(0, <[u8]>::len);
    let (Some(w), Some(h), Some(255)) = (w, h, max) else {
        return Err("unsupported PPM header".into());
    };
    let img = image::RgbImage::from_raw(w, h, data.split_off(header)).ok_or("truncated PPM")?;
    Ok(image::DynamicImage::ImageRgb8(img))
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_ppm() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend([255, 0, 0, 0, 0, 255]);
        let img = super::parse_ppm(data).unwrap().into_rgb8();
        assert_eq!(img.dimensions(), (2, 1));
        assert_eq!(img.get_pixel(1, 0).0, [0, 0, 255]);
        assert!(super::parse_ppm(b"P6\n2 1\n255\n".to_vec()).is_err());
        assert!(super::parse_ppm(b"P3\n1 1\n255\n".to_vec()).is_err());
    }
}
