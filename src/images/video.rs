//! Optional video support (`cargo build --features video`): a frame from the video is shown
//! as a preview, decoded by the `ffmpeg` program (`ffmpeg_frame`), so Glimt itself links no
//! video codecs.

use std::path::Path;

pub const EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "mpg", "mpeg", "wmv", "flv", "3gp", "ts", "mts",
    "m2ts",
];

/// A frame one second in (the very first one is often black), or the first frame for
/// shorter videos, no larger than `max` if given, and then the video's real size. ffmpeg
/// applies the rotation stored in the video.
pub fn preview(
    path: &Path,
    max: Option<[u32; 2]>,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    use crate::files::external::{ffmpeg_frame, probe_size};
    // A frame that took too long is not asked for again at 0, which would wait as long.
    let img = match ffmpeg_frame(path, "1", max) {
        Err(e) if !e.contains(crate::files::external::TIMED_OUT) => ffmpeg_frame(path, "0", max)?,
        frame => frame?,
    };
    Ok((img, max.and_then(|_| probe_size(path))))
}
