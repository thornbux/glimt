//! From a file to pixels and back: decoding at the size shown, camera RAW previews, colour
//! management, video, added formats, the thumbnail cache, and saving a turned image or a copy.

pub(crate) mod added_formats;
pub(crate) mod colour;
pub(crate) mod decode;
pub(crate) mod raw;
pub(crate) mod save;
#[cfg(feature = "svg")]
pub(crate) mod svg;
pub(crate) mod thumbs;
#[cfg(feature = "video")]
pub(crate) mod video;
