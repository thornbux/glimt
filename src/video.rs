//! Optional video support (`cargo build --features video`): a frame from the video is shown
//! as a preview, decoded by the `ffmpeg` program (`ffmpeg_frame`), so Glimt itself links no
//! video codecs.

use std::path::Path;

pub const EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "mpg", "mpeg", "wmv", "flv", "3gp", "ts", "mts",
    "m2ts",
];

/// A frame one second in (the very first one is often black), or the first frame for
/// shorter videos. ffmpeg applies the rotation stored in the video.
pub fn preview(path: &Path) -> Result<image::DynamicImage, String> {
    crate::ffmpeg_frame(path, "1").or_else(|_| crate::ffmpeg_frame(path, "0"))
}
