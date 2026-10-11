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
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Pnm)
        .map_err(|e| e.to_string())
}
