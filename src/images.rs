//! From a file to pixels and back: decoding at the size shown, camera RAW previews, colour
//! management, video, decoder plugins, the thumbnail cache, and saving a turned image or a copy.

pub(crate) mod colour;
pub(crate) mod decode;
pub(crate) mod decoder_plugins;
pub(crate) mod raw;
pub(crate) mod save;
#[cfg(feature = "svg")]
pub(crate) mod svg;
pub(crate) mod thumbs;
#[cfg(feature = "video")]
pub(crate) mod video;
