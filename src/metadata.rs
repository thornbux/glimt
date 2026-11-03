//! What is said about an image besides its pixels: EXIF, XMP sidecars with keywords, comments and
//! alt text, marks and stars, and the import of a Lightroom catalog.

pub(crate) mod exif;
pub(crate) mod lightroom;
#[cfg(feature = "marks")]
pub(crate) mod marks;
pub(crate) mod xmp;
