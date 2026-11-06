//! Saving a turned image. The pixels are turned, the EXIF is kept with orientation 1, and the
//! XMP is kept, all in the same format. (A lossy WebP gets them back in RIFF chunks.)

use crate::images::decode::load_icc;
use crate::metadata::exif;
use crate::model::listing::has_ext;
use crate::support::lang::tr;
use image::ImageDecoder;
use std::path::Path;

/// The XMP properties a file written with new pixels drops, because they show or measure the
/// pixels as they were. A thumbnail (xmp:Thumbnails, its JPEG in xmpGImg:image) of a crop shows
/// the whole frame, and of a turn the old way up. The pixel sizes are wrong after a crop or a
/// quarter turn, and readers take the image's own. Old Photoshop wrote "xap" for "xmp".
/// shortcut: these prefixes are the ones Adobe's tools write; match by namespace if another
/// writer's thumbnails are found.
const DROPPED_XMP: [&str; 8] = [
    "xmp:Thumbnails",
    "xap:Thumbnails",
    "xmpGImg:image",
    "xapGImg:image",
    "tiff:ImageWidth",
    "tiff:ImageLength",
    "exif:PixelXDimension",
    "exif:PixelYDimension",
];

/// The XMP properties a copy (`Derived::Copy`) also drops: other programs' edits of the image it
/// was made from. Lightroom's crop (crs:Crop…) and darktable's history, which holds its crop and
/// flip, are set on the old frame, so Lightroom or darktable would crop a crop again. darktable
/// reads Glimt's sidecar names (`name.ext.xmp`). The image itself keeps them when it is turned
/// (`Derived::Oriented`, `Derived::Turned`): its sidecar may be the only copy of the user's
/// darktable edits, and both programs keep their own history besides.
/// shortcut: these prefixes are the ones Lightroom and darktable write; match by namespace if
/// another program's crop is found.
const DROPPED_FROM_COPIES: [&str; 13] = [
    "crs:HasCrop",
    "crs:CropTop",
    "crs:CropLeft",
    "crs:CropBottom",
    "crs:CropRight",
    "crs:CropAngle",
    "crs:CropConstrainToWarp",
    "crs:CropWidth",
    "crs:CropHeight",
    "crs:CropUnit",
    "darktable:history",
    "darktable:history_end",
    "darktable:masks_history",
];

/// The keys of a PNG's text chunks that a file written with new pixels drops. The first three
/// are ImageMagick's copies of the EXIF and XMP, with the old orientation, which exiv2 (Geeqie,
/// digiKam) reads, so the turned file would be turned again there. The last is Photoshop's
/// resources, which may hold a thumbnail of the whole frame. Its IPTC keywords are kept in
/// ImageMagick's "Raw profile type iptc" beside it.
const DROPPED_PNG_TEXT: [&[u8]; 4] = [
    b"Raw profile type exif",
    b"Raw profile type APP1",
    b"Raw profile type xmp",
    b"Raw profile type 8bim",
];

/// How a copy of an image is made (Edit → Crop, File → Save As). It is made as the view shows
/// it (turned by `turns` and `mirrored`, by EXIF with `rotate`, and the page `page` of a TIFF),
/// as a part of it, smaller, and in a format. `App::shown_copy` makes it from the view, for
/// both.
#[derive(Clone, PartialEq)]
pub struct CopyAs {
    /// The page of a multi-page TIFF shown. It is 0 for any other image.
    pub page: usize,
    pub rotate: bool,
    pub turns: u8,
    pub mirrored: bool,
    /// The part `[x, y, width, height]` of the image as shown.
    pub crop: Option<[u32; 4]>,
    /// The longest side at most, in pixels. The image is never enlarged.
    pub long_side: Option<u32>,
    /// The format. `None` means as the original, which is JPEG, PNG or WebP (lossy as it was),
    /// and otherwise PNG.
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
        if crate::model::listing::is_jpeg(path) {
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

/// How an image is written, as a PNG, a WebP (lossy or not) or a JPEG of a quality.
#[derive(Clone, Copy)]
enum Kind {
    Png,
    WebP(bool),
    Jpeg(u8),
}

/// The EXIF block (camera, date, place) and XMP of `path`, for pixels written as the view
/// showed them. Their orientation is set upright, also with Auto Rotate off, where the view did
/// not turn them by it, because otherwise other programs would turn them again. The flag says
/// whether the written pixels lie a quarter turn from the stored ones, so that a PNG's DPI
/// across and down is swapped (`png_with_chunks`).
type Metadata = (Option<Vec<u8>>, Option<Vec<u8>>, bool);

/// How a file Glimt writes came from the image it was made from, which decides what it keeps of
/// that image's metadata. Every writer of such a file goes through `derived_metadata`,
/// `derived_xmp` and `png_with_chunks`, the one place that says it. The keywords, comment, alt
/// text, stars, camera, date and place are kept in every case.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Derived {
    /// The same pixels with another orientation, as a JPEG's turn is saved. Only the
    /// orientation changes.
    Oriented(u8),
    /// The image's own pixels written anew as the view showed them: a PNG or WebP turned. The
    /// orientation is 1, and what showed or measured the old pixels is dropped (`DROPPED_XMP`,
    /// `DROPPED_PNG_TEXT`, and in EXIF the thumbnail, with the pixel sizes set anew). Other
    /// programs' edits of the image stay, as they do on a JPEG's turn.
    Turned,
    /// A new file made from the image: a crop or a copy (Save As). It drops what `Turned` drops,
    /// and other programs' edits of the image it came from (`DROPPED_FROM_COPIES`).
    Copy,
}

/// The next place of a text in a packet, for places asked in rising order. A search goes on
/// from where the last one ended, so all of them together read the packet once, however many
/// places are asked (`without`).
struct Next<'a> {
    hay: &'a [u8],
    what: &'a [u8],
    /// What the last search found, if one was made. A search that found nothing holds for every
    /// later place too.
    found: Option<Option<usize>>,
}

impl<'a> Next<'a> {
    fn new(hay: &'a [u8], what: &'a [u8]) -> Self {
        Next {
            hay,
            what,
            found: None,
        }
    }

    /// Returns the first place of the text at or after `from`, or `None` when there is none.
    /// `from` is never smaller than in the call before.
    fn at(&mut self, from: usize) -> Option<usize> {
        if let Some(found) = self.found
            && found.is_none_or(|f| f >= from)
        {
            return found;
        }
        let rest = self.hay.get(from..).unwrap_or_default();
        let found = (rest.windows(self.what.len()))
            .position(|w| w == self.what)
            .map(|i| from + i);
        self.found = Some(found);
        found
    }
}

/// Saves the view's rotation in a PNG or WebP, which have no orientation field to set. The
/// pixels are turned, and the file is written again with its ICC profile, EXIF block and XMP
/// (keywords, comment, and alt text other programs wrote into it). PNG and lossless WebP lose
/// nothing. A lossy WebP is encoded again by ffmpeg at `webp_quality`, and gets its ICC profile,
/// EXIF block and XMP back afterwards. The new file replaces the old in one rename.
pub fn save_rotated(
    path: &Path,
    rotate: bool,
    turns: u8,
    mirrored: bool,
    webp_quality: u8,
) -> Result<(), String> {
    let named = path;
    // Through a symlink, the new file is renamed over the file it points to, not over the link.
    let path = &std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    // A read-only file is refused before the whole image is decoded and encoded for nothing.
    crate::files::file_io::writable(path).map_err(|e| e.to_string())?;
    let png = has_ext(path, &["png"]);
    if !png && !has_ext(path, &["webp"]) {
        return Err(tr("only JPEG, PNG and WebP can store the rotation").into());
    }
    if animated(path)? {
        return Err(tr("animated images are not turned").into());
    }
    let kind = match png {
        true => Kind::Png,
        false => Kind::WebP(webp_lossless(path) == Some(false)),
    };
    let (img, icc) = load_icc(path, rotate)?;
    let img = turned(img, turns, mirrored);
    let size = (img.width(), img.height());
    let (exif, xmp, quarter) = derived_metadata(path, rotate, turns, size, Derived::Turned)?;
    let out = encode(path, img, icc, exif, xmp, kind, quarter, webp_quality)?;
    replace_with_sidecar(path, named, out, Derived::Turned)
}

/// Replaces the image at `real` with `out` and makes the sidecar of `named`, the name the list
/// shows, say what the new image does (`how`). The sidecar is changed only once the image's own
/// write can no longer fail: the file is writable and still there and `out` is on disk beside
/// it. Before, the sidecar was changed first, so a read-only image or a full disk left a
/// sidecar that said the image was turned. This is the one way an image and its sidecar are
/// written again together, for a JPEG's orientation and a PNG or WebP turned.
pub(crate) fn replace_with_sidecar(
    real: &Path,
    named: &Path,
    out: Vec<u8>,
    how: Derived,
) -> Result<(), String> {
    let sidecar = || derive_sidecar(named, how).map_err(std::io::Error::other);
    crate::files::file_io::replace_existing(real, out, sidecar).map_err(|e| e.to_string())
}

/// The copy of `path` that `how` asks for, encoded, and its format. It has its profile, and
/// unless stripped its EXIF (upright) and XMP. JPEG and a lossy WebP are at their quality (the
/// WebP through ffmpeg), and PNG and lossless WebP are without loss.
pub fn copy_bytes(path: &Path, how: &CopyAs) -> Result<(Vec<u8>, Format), String> {
    if animated(path)? {
        return Err(tr("animated images are not saved as a copy").into());
    }
    let format = how.format.unwrap_or_else(|| Format::of(path));
    let kind = match format {
        Format::Jpeg => Kind::Jpeg(how.jpeg_quality),
        Format::Png => Kind::Png,
        Format::WebP => Kind::WebP(false),
        Format::WebPLossy => Kind::WebP(true),
    };
    let (img, icc) = pixels(path, how)?;
    let size = (img.width(), img.height());
    let (exif, xmp, quarter) = match how.strip {
        true => (None, None, false),
        false => derived_metadata(path, how.rotate, how.turns, size, Derived::Copy)?,
    };
    // A PNG's other chunks hold as they were, because a DPI is no different for a part. They
    // are dropped when stripped, because the text chunks may say where and by whom.
    let from = if how.strip { Path::new("") } else { path };
    let out = encode(from, img, icc, exif, xmp, kind, quarter, how.webp_quality)?;
    Ok((out, format))
}

/// The pixels of `path` as `how` asks for them, with its ICC profile. That is the page shown,
/// turned as shown, the part, and smaller. Every copy of the shown image starts from these,
/// whether it is saved or put on the clipboard.
fn pixels(path: &Path, how: &CopyAs) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    // A later page is read as the view decodes it, because the image crate reads only the
    // first.
    let (img, icc) = match how.page {
        0 => load_icc(path, how.rotate)?,
        page => (crate::images::decode::page_of(path, page)?, None),
    };
    let mut img = turned(img, how.turns, how.mirrored);
    if let Some([x, y, width, height]) = how.crop {
        if width == 0 || height == 0 || x + width > img.width() || y + height > img.height() {
            return Err(tr("the part is outside the image").into());
        }
        img = img.crop_imm(x, y, width, height);
    }
    if let Some(long) = how.long_side {
        img = smaller(img, long.max(1))?;
    }
    Ok((img, icc))
}

/// The image as `how` shows it, in sRGB, for Select → Copy Image. That copied the image
/// unturned, and only its first page.
pub fn shown_image(path: &Path, how: &CopyAs) -> Result<image::DynamicImage, String> {
    let (img, icc) = pixels(path, how)?;
    Ok(crate::images::colour::to_srgb(img, icc.as_deref()))
}

/// `img` with its longest side `long` at most, by Lanczos3, which is sharp, for a copy kept.
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

/// The name of `path` without its extension, as its bytes, with `more` after it, as a copy's
/// name beside it. A name that is not UTF-8 is kept as it is, not made "caf\u{FFFD}".
pub(crate) fn stem_and(path: &Path, more: &str) -> std::ffi::OsString {
    let mut name = path.file_stem().unwrap_or_default().to_owned();
    name.push(more);
    name
}

/// Saves the part `how.crop` of the image as a new file beside it, "NAME-cropped.EXT" or
/// "NAME-cropped-2.EXT" …, never over another file. Its sidecar is copied too. Returns the new
/// file.
pub fn save_cropped(path: &Path, how: &CopyAs) -> Result<std::path::PathBuf, String> {
    let (out, format) = copy_bytes(path, how)?;
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
            1 => stem_and(path, &format!("-cropped.{ext}")),
            _ => stem_and(path, &format!("-cropped-{n}.{ext}")),
        };
        let new = path.with_file_name(name);
        match land(path, &new, &out, false, how.strip) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
            Ok(()) => return Ok(new),
        }
    }
    unreachable!()
}

/// Saves a copy of `path`, encoded as `out`, as `new`. It goes over a file there only when
/// `replace`, which the file dialog asked about. The copy gets `path`'s sidecar, or none when
/// `strip` or when `path` has none. A sidecar at `new`'s name belongs to the replaced file, or
/// was left without its image, and its keywords and comment are not the copy's. So it is
/// removed and the copy's is made new, never written through a link there, which a downloaded
/// folder may have pointed at any file of the user's (~/.bashrc). This is the one way a copy is
/// saved, for Crop, Save As and Save This Frame.
pub(crate) fn land(
    path: &Path,
    new: &Path,
    out: &[u8],
    replace: bool,
    strip: bool,
) -> std::io::Result<()> {
    // The lock of the copy's sidecar is held, as every writer of a sidecar holds it. A keyword
    // written to the replaced file from another window then waits, rather than landing between
    // the old sidecar's removal and the new one's creation. The image is written beside its name
    // before the lock is taken, because that is the slow part on a share, and other sidecars of
    // the lock's slot would wait for it. The lock is taken before the image takes its name and is
    // held until the copy's sidecar is in place.
    let mut held = None;
    // The sidecar is read before the copy is written, so one that cannot be read leaves no copy
    // behind. A sidecar that is a link counts as none, because what it points to (a private key)
    // is not the photo's.
    let source = match strip {
        true => None,
        false => crate::metadata::xmp::own_sidecar(path)?,
    };
    let side = crate::metadata::xmp::sidecar(new);
    let named =
        |e: std::io::Error| std::io::Error::new(e.kind(), format!("{}: {e}", side.display()));
    // A link goes, not what it points to, and `create_file` never follows one.
    let remove_side = || match std::fs::remove_file(&side) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(named(e)),
        _ => Ok(()),
    };
    match replace {
        // The replaced file's sidecar goes after the image is written beside it and before the
        // image takes the name. A failed write then leaves the file and its sidecar, and a
        // sidecar that cannot be removed leaves the file, never with the copy's pixels under its
        // own keywords and stars. A link there is replaced, not the file it points to.
        true => {
            use std::io::Write;
            crate::files::file_io::replace_name_with(
                new,
                |f| f.write_all(out),
                || {
                    held = Some(crate::metadata::xmp::held(new));
                    remove_side()
                },
            )?;
        }
        // A file of that name is not touched, nor its sidecar, because the copy is refused first.
        false => {
            use std::io::Write;
            let tmp = crate::files::file_io::tmp_path(new);
            crate::files::file_io::write_beside(
                &tmp,
                new,
                |f| f.write_all(out),
                |tmp| {
                    held = Some(crate::metadata::xmp::held(new));
                    crate::files::file_io::rename_never_over(tmp, new)
                },
            )?;
            remove_side()?;
        }
    }
    match source {
        Some(xmp) => crate::files::file_io::create_file(&side, derived_xmp(&xmp, Derived::Copy))
            .map_err(named),
        None => Ok(()),
    }
}

/// Whether `a` and `b` are names of one file, also through a link or a hard link. A copy is
/// never saved over the image it is made from (Save As, Save This Frame), and Rename Many's
/// repair tells a step done from one not done by it.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    matches!((file_id(a), file_id(b)), (Some(x), Some(y)) if x == y)
}

/// Returns what tells the file at `path` from every other: its device and inode. It is the
/// same however the file is reached, through a link, a hard link or a bind mount, where its
/// real path (`canonicalize`) differs. Returns `None` when the file cannot be looked up.
pub(crate) fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// File → Save As. It saves the copy `how` asks for as `to`, which the file dialog let replace
/// a file there. When the extension is made the format's, it is never saved over a file the
/// dialog did not ask about, and never over the image itself. Returns the file saved.
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
    if same_file(path, &to) {
        return Err(tr("choose another name: the image itself is not changed").into());
    }
    let (out, _) = copy_bytes(path, how)?;
    land(path, &to, &out, matches, how.strip).map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(to)
}

/// Whether `path` is animated. An animation is refused, because only the first frame would be
/// written back. It asks by what the file is, because it is read by its content. An animated
/// GIF named a.png is still animated, and a JPEG named so is not.
fn animated(path: &Path) -> Result<bool, String> {
    let format = crate::images::decode::image_reader(path)
        .map_err(|e| e.to_string())?
        .format();
    let file = || crate::images::decode::open(path).map(std::io::BufReader::new);
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

/// Reads the EXIF block and XMP of `path` and makes them what a file of new pixels of `size`
/// keeps of them, as the image turned or as a copy (`how`). The pixels are the stored ones
/// turned upright by EXIF with Auto Rotate (`rotate`) and then `turns` quarters by the view.
/// Both writers take the quarter flag from here, because a copy once passed none, and a PNG
/// copied a quarter turned kept its DPI across and down unswapped.
fn derived_metadata(
    path: &Path,
    rotate: bool,
    turns: u8,
    size: (u32, u32),
    how: Derived,
) -> Result<Metadata, String> {
    let (mut exif, mut xmp, stored) = crate::images::decode::image_reader(path)
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .map(|mut d| {
            let exif = exif::decoded(path, &mut d);
            let stored = rotate.then(|| exif::stored_orientation(path, &mut d));
            (exif, d.xmp_metadata().ok().flatten(), stored)
        })
        .unwrap_or_default();
    if let Some(exif) = &mut exif {
        exif::set_upright(exif, size)?;
    }
    if let Some(xmp) = &mut xmp {
        *xmp = derived_xmp(xmp, how);
    }
    let stored_quarter = stored.is_some_and(crate::images::decode::quarter_turn);
    Ok((exif, xmp, (turns % 2 == 1) != stored_quarter))
}

/// Makes the XMP packet `xmp` what a file derived from its image as `how` says keeps of it,
/// whether it was inside the image or in its sidecar. Its orientation is set, for new pixels
/// the properties of `DROPPED_XMP` are left out, and for a copy those of `DROPPED_FROM_COPIES`.
pub(crate) fn derived_xmp(xmp: &[u8], how: Derived) -> Vec<u8> {
    let mut out = xmp.to_vec();
    match how {
        Derived::Oriented(value) => exif::set_xmp_orientation(&mut out, value),
        Derived::Turned | Derived::Copy => {
            exif::set_xmp_orientation(&mut out, 1);
            let copy = (how == Derived::Copy).then_some(&DROPPED_FROM_COPIES[..]);
            for name in DROPPED_XMP.iter().chain(copy.unwrap_or_default()) {
                out = without(&out, name.as_bytes());
            }
        }
    }
    out
}

/// Makes the sidecar of `path`, whose image is being written anew as `how` says, say what the
/// image will. Otherwise a program that reads the sidecar first (digiKam) turns the photo back
/// by its old orientation. It is called after the new image is written whole and before it
/// takes the name (`replace_with_sidecar`): a sidecar that cannot be written then leaves the
/// image as it was, and a failed image write leaves the sidecar. Only a sidecar of its own,
/// readable as text and with something to change, is written, under its lock
/// (`xmp::update_sidecar`). A link or one that cannot be read is left alone, as the panel leaves
/// it. The sidecar is the one of the name the list shows, which is the one Glimt's panel reads,
/// also for a link to a photo.
fn derive_sidecar(path: &Path, how: Derived) -> Result<(), String> {
    let old = crate::metadata::xmp::own_sidecar(path).ok().flatten();
    let text = old.filter(|x| std::str::from_utf8(x).is_ok());
    if text.is_none_or(|x| derived_xmp(&x, how) == x) {
        return Ok(());
    }
    crate::metadata::xmp::update_sidecar(path, true, |x| {
        String::from_utf8(derived_xmp(x.as_bytes(), how)).ok()
    })
}

/// Leaves the property `name` out of `xmp`, written as an attribute (` name="…"`) or as an
/// element (`<name …>…</name>` or `<name …/>`). A property that is cut off is left as it is, and
/// so is one in a comment or one whose element would reach into a comment. The packet is copied
/// once, and each search goes on from where the last one ended (`Next`), so a hostile packet
/// takes linear time. Before, each property left out moved the rest of the packet, and each
/// element without its end tag searched to the end again, both quadratic, and a
/// `<!--<xmp:Thumbnails>-->` comment was cut to the next end tag, which left the packet broken.
fn without(xmp: &[u8], name: &[u8]) -> Vec<u8> {
    let end_tag = [&b"</"[..], name, b">"].concat();
    let [mut names, mut gts, mut ends] = [
        Next::new(xmp, name),
        Next::new(xmp, b">"),
        Next::new(xmp, &end_tag),
    ];
    let [mut opens, mut closes] = [Next::new(xmp, b"<!--"), Next::new(xmp, b"-->")];
    let [mut double, mut single] = [Next::new(xmp, b"\""), Next::new(xmp, b"'")];
    let (mut out, mut copied, mut from) = (Vec::with_capacity(xmp.len()), 0, 0);
    while let Some(at) = names.at(from) {
        // A comment that starts before the name is passed over whole.
        if let Some(open) = opens.at(from)
            && open < at
        {
            match closes.at(open + 4) {
                Some(close) => from = close + 3,
                None => break,
            }
            continue;
        }
        let end = at + name.len();
        let before = at.checked_sub(1).map(|i| xmp[i]);
        let after = xmp.get(end).copied();
        let cut = match before {
            // An element: up to its own "/>", or else to its end tag.
            Some(b'<')
                if after.is_some_and(|c| c == b'>' || c == b'/' || c.is_ascii_whitespace()) =>
            {
                match gts.at(end) {
                    Some(gt) if xmp[gt - 1] == b'/' => Some(gt + 1),
                    Some(gt) => ends.at(gt).map(|e| e + end_tag.len()),
                    None => None,
                }
            }
            // An attribute: its name, "=" and the quoted value, with the blank before it.
            Some(c) if c.is_ascii_whitespace() => {
                let blanks = |i: usize| {
                    i + xmp[i.min(xmp.len())..]
                        .iter()
                        .take_while(|c| c.is_ascii_whitespace())
                        .count()
                };
                let i = blanks(end);
                let eq = xmp.get(i) == Some(&b'=');
                let i = blanks(i + 1);
                match xmp.get(i).copied() {
                    Some(b'"') if eq => double.at(i + 1).map(|j| j + 1),
                    Some(b'\'') if eq => single.at(i + 1).map(|j| j + 1),
                    _ => None,
                }
            }
            _ => None,
        };
        // A cut that would reach into a comment is not made.
        match cut.filter(|&stop| opens.at(at).is_none_or(|open| open >= stop)) {
            Some(stop) => {
                out.extend_from_slice(&xmp[copied..at - 1]);
                (copied, from) = (stop, stop);
            }
            None => from = end,
        }
    }
    out.extend_from_slice(&xmp[copied..]);
    out
}

/// `img` turned as the view turns it, mirrored first and then turned clockwise `turns`
/// quarters.
fn turned(img: image::DynamicImage, turns: u8, mirrored: bool) -> image::DynamicImage {
    let img = if mirrored { img.fliph() } else { img };
    match turns % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    }
}

/// `img` encoded as `kind` with the profile, EXIF and XMP. A PNG keeps the other chunks of
/// `path` (`quarter` says it was turned a quarter since). It is encoded in memory, because an
/// encoder that owns its file flushes it on drop, where a full disk would go unnoticed and a
/// cut-off file would replace the photo. The pixels and the profile are made to fit `kind`
/// first (`fitted`), so every derived file is written from pixels its format holds.
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
    let (img, icc) = fitted(img, icc, kind);
    let (width, height) = (img.width(), img.height());
    if let Kind::WebP(true) = kind {
        // ffmpeg drops the profile, so it is put back. The pixels are never converted to sRGB
        // here: that followed the view's colour settings, so with colour management off a
        // Display P3 file lost its profile and kept its P3 pixels.
        let mut webp = lossy_webp(&img, webp_quality)?;
        if let Some(icc) = &icc {
            webp = webp_with_chunk(&webp, b"ICCP", icc, width, height)?;
        }
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
            match exif.map(|e| enc.set_exif_metadata(e)) {
                Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
                _ => img.write_with_encoder(enc),
            }
        }
    };
    written.map_err(|e| e.to_string())?;
    // The rest of what the PNG said of itself is kept: its text (a comment, an AI tool's
    // prompt), its DPI and its gamma. Otherwise a 300 dpi scan prints at 72 after a turn. It is
    // only taken from a PNG.
    if let Kind::Png = kind
        && has_ext(path, &["png"])
        && let Ok(old) = crate::images::decode::read_limited(path)
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

/// Makes `img` pixels that `kind` writes, and keeps `icc` only where it describes them. A JPEG
/// holds 8-bit grey or RGB, a WebP 8-bit RGB or RGBA, and a PNG no floats, so a 32-bit float
/// TIFF is written in 16 bits. Before, a PNG copy of such a TIFF failed. A profile is kept only
/// when its colour space, in bytes 16 to 20 of its header, is the pixels' own, grey or RGB.
/// Before, a grey JPEG's Gray profile went with RGB pixels, and a CMYK JPEG's profile with the
/// RGB it is decoded to, so other programs showed the copy in wrong colours.
fn fitted(
    img: image::DynamicImage,
    icc: Option<Vec<u8>>,
    kind: Kind,
) -> (image::DynamicImage, Option<Vec<u8>>) {
    use image::DynamicImage as D;
    let colour = img.color();
    let img = match kind {
        Kind::Jpeg(_) if !colour.has_color() => D::from(img.into_luma8()),
        Kind::Jpeg(_) => D::from(img.into_rgb8()),
        Kind::WebP(_) if colour.has_alpha() => D::from(img.into_rgba8()),
        Kind::WebP(_) => D::from(img.into_rgb8()),
        Kind::Png => match img {
            D::ImageRgb32F(_) => D::from(img.into_rgb16()),
            D::ImageRgba32F(_) => D::from(img.into_rgba16()),
            img => img,
        },
    };
    let space: &[u8] = if img.color().has_color() {
        b"RGB "
    } else {
        b"GRAY"
    };
    (img, icc.filter(|p| p.get(16..20) == Some(space)))
}

/// Puts `xmp` into `jpeg` in an APP1 segment of Adobe's name, right after the start (SOI).
/// An XMP packet too long for one segment is left out, so a copy from a PNG or TIFF with a long
/// editing history is still made.
fn jpeg_with_xmp(jpeg: &[u8], xmp: &[u8]) -> Result<Vec<u8>, String> {
    const NAME: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let len = 2 + NAME.len() + xmp.len();
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return Err("not a JPEG file".into());
    }
    // shortcut: Extended XMP over several segments is not written; write it if a copy that
    // keeps such a packet is ever asked for.
    if len > 0xFFFF {
        return Ok(jpeg.to_vec());
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
    // The 8-byte signature comes first, then IHDR, with its length 13, its name, 13 bytes and a
    // CRC.
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

/// The PNG `new` with the chunks of `old` that still hold for its pixels put after its header.
/// They are text, physical size, time and colour space. The profile, EXIF and XMP are not
/// taken, because they are written anew, nor what depends on how the pixels are stored
/// (palette, transparency, bit depth), nor the text of `DROPPED_PNG_TEXT`.
fn png_with_chunks(new: &[u8], old: &[u8], quarter: bool) -> Vec<u8> {
    const KEPT: [&[u8; 4]; 8] = [
        b"tEXt", b"zTXt", b"iTXt", b"pHYs", b"tIME", b"gAMA", b"cHRM", b"sRGB",
    ];
    let (end, mut kept) = (33, Vec::new());
    // Turned a quarter, pHYs, the pixels per unit across and down, are swapped too, also for a
    // square picture. Otherwise a fax-like scan of 204×98 dpi would print stretched.
    let swapped = quarter;
    let mut at = 8;
    while let Some(len) = old.get(at..at + 4) {
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let Some(chunk) = old.get(at..at.saturating_add(12).saturating_add(len)) else {
            break;
        };
        let name = &chunk[4..8];
        let xmp = name == b"iTXt" && chunk[8..].starts_with(b"XML:com.adobe.xmp\0");
        let old_profile = (DROPPED_PNG_TEXT.iter())
            .any(|key| matches!(name, b"tEXt" | b"zTXt" | b"iTXt") && chunk[8..].starts_with(key));
        if name == b"pHYs" && swapped && len == 9 {
            let body = [&b"pHYs"[..], &chunk[12..16], &chunk[8..12], &chunk[16..17]].concat();
            kept.extend(9u32.to_be_bytes());
            kept.extend(&body);
            kept.extend(crc32(&body).to_be_bytes());
        } else if KEPT.iter().any(|k| *k == name) && !xmp && !old_profile {
            kept.extend_from_slice(chunk);
        }
        // Text may come after the pixels too, so it reads up to the end.
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

/// The CRC that PNG chunks end with (ISO 3309), bit by bit. It runs once for each saved image.
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

/// Whether a WebP file is lossless (a VP8L image) or lossy (VP8). Returns `None` if neither is
/// found.
pub fn webp_lossless(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = crate::images::decode::open(path).ok()?;
    let mut head = [0; 12];
    file.read_exact(&mut head).ok()?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WEBP" {
        return None;
    }
    // The chunks come after the header. An extended file has VP8X, ICCP, ALPH … before the
    // image.
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

/// `img` as a lossy WebP of `quality`, encoded by ffmpeg in memory, because the `image` crate
/// writes only lossless ones. Its errors are read beside it, and it is stopped after
/// `external::LIMIT`.
pub fn lossy_webp(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut child = crate::files::external::stoppable_command("ffmpeg")
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
        .spawn()
        .map_err(|e| crate::tf!("a lossy WebP needs ffmpeg to be saved: {e}", e))?;
    // The image is written in a thread of its own while its output is read, so neither pipe
    // fills and stops it.
    let mut stdin = child.stdin.take();
    let feed = std::thread::spawn(move || drop(stdin.as_mut().map(|i| i.write_all(&png))));
    let max = crate::images::decode::MAX_DECODE;
    let (webp, err, ok) =
        crate::files::external::output_of(&mut child, max, crate::files::external::LIMIT);
    let _ = feed.join();
    match webp {
        Some(webp) if ok && !webp.is_empty() => Ok(webp),
        _ if err.trim().is_empty() => Err("ffmpeg made no WebP".into()),
        _ => Err(err.trim().to_owned()),
    }
}

/// `webp` with an ICCP, EXIF or XMP chunk (`name`), and its flag set in the extended format's
/// VP8X header, which is made for a simple file. The format puts the ICC profile before the
/// image's data and EXIF and XMP at the end, EXIF before XMP.
pub fn webp_with_chunk(
    webp: &[u8],
    name: &[u8; 4],
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let flag = match name {
        b"ICCP" => 0x20,
        b"EXIF" => 0x08,
        _ => 0x04,
    };
    if !webp.starts_with(b"RIFF") || webp.get(8..12) != Some(&b"WEBP"[..]) {
        return Err("not a WebP file".into());
    }
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let chunks = &webp[12..];
    // A lossless image tells its alpha in its header (bit 28 after the signature byte), and the
    // extended header must say it too, or viewers show it opaque.
    let alpha = chunks.starts_with(b"VP8L") && chunks.get(12).is_some_and(|b| b & 0x10 != 0);
    let flag = if alpha { flag | 0x10 } else { flag };
    let rest = if chunks.starts_with(b"VP8X") {
        out.extend(chunks.get(..18).ok_or("cut off")?);
        *out.get_mut(20).ok_or("cut off")? |= flag;
        &chunks[18..]
    } else {
        out.extend(b"VP8X\x0a\0\0\0");
        out.extend([flag, 0, 0, 0]);
        out.extend(&width.saturating_sub(1).to_le_bytes()[..3]);
        out.extend(&height.saturating_sub(1).to_le_bytes()[..3]);
        chunks
    };
    let mut chunk = name.to_vec();
    chunk.extend((data.len() as u32).to_le_bytes());
    chunk.extend(data);
    // Chunks have an even length.
    if data.len() % 2 == 1 {
        chunk.push(0);
    }
    if name == b"ICCP" {
        out.extend(chunk);
        out.extend(rest);
    } else {
        out.extend(rest);
        out.extend(chunk);
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// An XMP packet too long for one APP1 segment is left out of a JPEG, not a failed save.
    #[test]
    fn a_jpeg_without_an_xmp_too_long() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
        let with = super::jpeg_with_xmp(&jpeg, b"<x/>").unwrap();
        assert!(with.len() > jpeg.len() && with.ends_with(&jpeg[2..]));
        assert_eq!(super::jpeg_with_xmp(&jpeg, &[b' '; 70_000]).unwrap(), jpeg);
        assert!(super::jpeg_with_xmp(b"GIF89a", b"<x/>").is_err());
    }

    /// A damaged PNG or WebP given a chunk is refused or carried on as far as it goes, with no
    /// panic.
    #[test]
    fn survives_broken_files_given_a_chunk() {
        let mut png = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        for bytes in crate::support::test_support::broken(&png) {
            let _ = super::png_with_xmp(&bytes, b"<x:xmpmeta/>");
        }
        let mut webp = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut webp),
                image::ImageFormat::WebP,
            )
            .unwrap();
        for bytes in crate::support::test_support::broken(&webp) {
            let _ = super::webp_with_chunk(&bytes, b"EXIF", b"Exif", 4, 2);
        }
    }

    /// A PNG chunk named `name` holding `data`, with its length and CRC.
    fn png_chunk(name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        let body = [name, data].concat();
        c.extend(&body);
        c.extend(super::crc32(&body).to_be_bytes());
        c
    }

    /// A PNG of `width` by `height` black pixels.
    fn black_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    /// A PNG turned keeps its text and DPI, and nothing tied to how the old pixels were stored.
    #[test]
    fn keeps_png_chunks() {
        use image::ImageEncoder;
        let chunk = png_chunk;
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
        // Turned a quarter (a 2×3 picture now), pHYs across and down are swapped, and its CRC
        // is right.
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
    }

    /// A PNG of 204×98 dpi written a quarter turned, in place or as a copy, has its DPI across
    /// and down swapped, so it prints as it did. Before, a copy kept them as they were and
    /// printed stretched, because only the turn in place passed the flag.
    #[test]
    fn a_quarter_turn_swaps_the_dpi_of_every_png_written() {
        let dir = std::env::temp_dir().join(format!("glimt-dpi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let wide = black_png(3, 2);
        let make = || {
            let mut png = wide[..33].to_vec();
            png.extend(png_chunk(b"pHYs", &[0, 0, 0, 204, 0, 0, 0, 98, 1]));
            png.extend(&wide[33..]);
            std::fs::write(&file, png).unwrap();
        };
        let dpi = |path: &std::path::Path| {
            let out = std::fs::read(path).unwrap();
            let at = out.windows(4).position(|w| w == b"pHYs").unwrap();
            out[at + 4..at + 13].to_vec()
        };
        let swapped = [0, 0, 0, 98, 0, 0, 0, 204, 1];
        let how = |turns| super::CopyAs {
            page: 0,
            rotate: true,
            turns,
            mirrored: false,
            crop: None,
            long_side: None,
            format: Some(super::Format::Png),
            jpeg_quality: 90,
            webp_quality: 90,
            strip: false,
        };
        make();
        let copy = super::save_as(&file, &how(1), &dir.join("b.png")).unwrap();
        assert_eq!(dpi(&copy), swapped);
        let copy = super::save_as(&file, &how(2), &dir.join("c.png")).unwrap();
        assert_eq!(dpi(&copy), [0, 0, 0, 204, 0, 0, 0, 98, 1]);
        super::save_rotated(&file, true, 1, false, 90).unwrap();
        assert_eq!(dpi(&file), swapped);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A derived file is written from pixels its format holds: a float image as a 16-bit PNG,
    /// and grey as a grey JPEG. Before, a PNG of a 32-bit float TIFF failed to be written.
    #[test]
    fn a_derived_file_has_pixels_its_format_holds() {
        use image::ImageDecoder;
        let write = |img: image::DynamicImage, kind| {
            let out = super::encode(
                std::path::Path::new(""),
                img,
                None,
                None,
                None,
                kind,
                false,
                90,
            );
            let out = out.unwrap();
            let reader = image::ImageReader::new(std::io::Cursor::new(out));
            reader
                .with_guessed_format()
                .unwrap()
                .into_decoder()
                .unwrap()
                .color_type()
        };
        let float = image::Rgb32FImage::from_pixel(2, 2, image::Rgb([0.5, 0.25, 1.0]));
        assert_eq!(
            write(float.into(), super::Kind::Png),
            image::ColorType::Rgb16
        );
        let float = image::Rgba32FImage::from_pixel(2, 2, image::Rgba([0.5, 0.25, 1.0, 1.0]));
        assert_eq!(
            write(float.into(), super::Kind::Png),
            image::ColorType::Rgba16
        );
        let grey = image::GrayImage::from_pixel(2, 2, image::Luma([90]));
        assert_eq!(
            write(grey.into(), super::Kind::Jpeg(90)),
            image::ColorType::L8
        );
    }

    /// A derived file has the image's profile only where it describes the pixels written: a
    /// Gray profile with grey pixels, an RGB one with RGB pixels, and a CMYK one with neither.
    /// Before, a grey JPEG copied as a WebP, or a CMYK JPEG decoded to RGB, kept a profile of
    /// other pixels, which other programs showed in wrong colours.
    #[test]
    fn a_derived_file_has_a_profile_only_for_its_pixels() {
        use image::ImageDecoder;
        let rgb = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let other = |space: &[u8; 4]| {
            let mut p = rgb.clone();
            p[16..20].copy_from_slice(space);
            p
        };
        let (gray, cmyk) = (other(b"GRAY"), other(b"CMYK"));
        let profile = |img: image::DynamicImage, icc: &Vec<u8>, kind| {
            let out = super::encode(
                std::path::Path::new(""),
                img,
                Some(icc.clone()),
                None,
                None,
                kind,
                false,
                90,
            );
            let reader = image::ImageReader::new(std::io::Cursor::new(out.unwrap()));
            let mut dec = reader
                .with_guessed_format()
                .unwrap()
                .into_decoder()
                .unwrap();
            dec.icc_profile().unwrap()
        };
        let grey = || image::DynamicImage::from(image::GrayImage::new(2, 2));
        let colour = || image::DynamicImage::from(image::RgbImage::new(2, 2));
        use super::Kind::{Jpeg, Png, WebP};
        for kind in [Jpeg(90), Png] {
            assert_eq!(profile(grey(), &gray, kind), Some(gray.clone()));
            assert_eq!(profile(colour(), &gray, kind), None);
            assert_eq!(profile(colour(), &rgb, kind), Some(rgb.clone()));
            assert_eq!(profile(colour(), &cmyk, kind), None);
        }
        assert_eq!(profile(grey(), &gray, WebP(false)), None);
        assert_eq!(profile(grey(), &rgb, WebP(false)), Some(rgb.clone()));
    }

    /// A turned PNG given the chunks of a broken one keeps what it can read of them, without a
    /// panic or a loop.
    #[test]
    fn survives_broken_png_chunks() {
        let (tall, mut old) = (black_png(2, 3), black_png(3, 2));
        old.splice(33..33, png_chunk(b"pHYs", &[0, 0, 0, 204, 0, 0, 0, 98, 1]));
        for broken in crate::support::test_support::broken(&old) {
            super::png_with_chunks(&tall, &broken, true);
        }
    }

    /// WebP files broken in thousands of ways are told lossless, lossy or neither without a
    /// panic or a loop.
    #[test]
    fn survives_broken_webp() {
        use image::ImageEncoder;
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&[0; 4 * 2 * 3], 4, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        let file =
            std::env::temp_dir().join(format!("glimt-test-webp-{}.webp", std::process::id()));
        for bytes in crate::support::test_support::broken(&webp) {
            std::fs::write(&file, bytes).unwrap();
            super::webp_lossless(&file);
        }
        std::fs::remove_file(file).unwrap();
    }

    /// The new bytes replace the image with its permissions. A failed replace leaves no
    /// temporary file, and the old one as it was.
    #[test]
    fn replaces_images_whole() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-replace-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("busy.png")).unwrap();
        std::fs::write(dir.join("busy.png/inside"), "x").unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        crate::files::file_io::replace_file(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        // A folder cannot be replaced by a file.
        assert!(crate::files::file_io::replace_file(&dir.join("busy.png"), b"new").is_err());
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// image writes lossless WebP (VP8L), and anything else is not taken for one.
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
        let rows = crate::metadata::exif::read(&file);
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
        // Through a symlink, the file it points to is turned, and the link stays a link.
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        super::save_rotated(&link, false, 1, false, 90).unwrap();
        assert!(link.is_symlink());
        assert_eq!(image::image_dimensions(&file).unwrap(), (2, 4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An image is saved as the view showed it, also with a mirroring orientation. It is turned
    /// by it with Auto Rotate on, as it is without, and in both cases has an upright
    /// orientation after.
    #[test]
    fn saves_rotated_as_seen() {
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let (red, blue) = (image::Rgb([255, 0, 0]), image::Rgb([0, 0, 255]));
        // Orientation 2 is mirrored left to right.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x02\0\0\0\0\0\0";
        for (auto_rotate, top) in [(false, red), (true, blue)] {
            let mut enc =
                image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
            enc.set_exif_metadata(exif.to_vec()).unwrap();
            image::RgbImage::from_fn(2, 1, |x, _| if x == 0 { red } else { blue })
                .write_with_encoder(enc)
                .unwrap();
            // With a quarter turn clockwise, the left pixel of what the view shows goes on top.
            super::save_rotated(&file, auto_rotate, 1, false, 90).unwrap();
            let mut dec = image::ImageReader::open(&file)
                .unwrap()
                .into_decoder()
                .unwrap();
            assert_eq!(
                dec.orientation().unwrap(),
                image::metadata::Orientation::NoTransforms
            );
            let img = crate::images::decode::from_decoder(dec)
                .unwrap()
                .into_rgb8();
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
        // An ICC profile goes before the image's data, also after EXIF was added, and is read
        // back. A turned lossy WebP lost it, as ffmpeg drops it.
        let icc = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let with = super::webp_with_chunk(&webp, b"EXIF", exif, 5, 3).unwrap();
        let with = super::webp_with_chunk(&with, b"ICCP", &icc, 5, 3).unwrap();
        assert_eq!((&with[30..34], with[20]), (&b"ICCP"[..], 0x38));
        let mut dec = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(with)).unwrap();
        assert_eq!(dec.icc_profile().unwrap(), Some(icc));
        assert_eq!(dec.exif_metadata().unwrap().as_deref(), Some(&exif[..]));
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

    /// A part is saved as a new JPEG beside the image, as the view shows it, with its XMP. A
    /// second is never saved over the first, and the image itself is untouched.
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
            page: 0,
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
        // The right half is blue.
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

    /// A PNG whose EXIF is only in the text chunk exiv2 (digiKam, GIMP) and older ImageMagick
    /// write keeps it when turned in place and when cropped: in an eXIf chunk, upright, and
    /// without the old text chunk, which other programs would read the old way up. Before, the
    /// chunk was dropped and nothing took its EXIF over, so the date taken and camera were lost.
    #[test]
    fn png_exif_in_text_is_kept() {
        use crate::support::test_support::{CANON_EXIF, png_with_raw_profile};
        use image::ImageDecoder;
        let dir = std::env::temp_dir().join(format!("glimt-pngtext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let kept = |path: &std::path::Path| {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes.windows(16).any(|w| w == b"Raw profile type"));
            let mut dec = image::ImageReader::open(path)
                .unwrap()
                .into_decoder()
                .unwrap();
            let exif = dec.exif_metadata().unwrap().expect("an eXIf chunk");
            assert!(exif.windows(5).any(|w| w == b"Canon"));
            assert!(exif.windows(19).any(|w| w == b"2020:01:02 03:04:05"));
            let upright = image::metadata::Orientation::from_exif_chunk(&exif);
            assert_eq!(upright, Some(image::metadata::Orientation::NoTransforms));
        };
        png_with_raw_profile(&file, "zTXt", "Raw profile type exif", CANON_EXIF);
        super::save_rotated(&file, true, 1, false, 90).unwrap();
        kept(&file);
        png_with_raw_profile(&file, "tEXt", "Raw profile type APP1", CANON_EXIF);
        let how = super::CopyAs {
            page: 0,
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: Some([0, 0, 4, 4]),
            long_side: None,
            format: None,
            jpeg_quality: 92,
            webp_quality: 90,
            strip: false,
        };
        kept(&super::save_cropped(&file, &how).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Save As saves smaller and in another format, with its extension made the format's and
    /// the metadata kept or left out, and never over the image itself.
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
            page: 0,
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
        // When named without an extension, the format's is added.
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
        // Never the image itself, also asked for by name, or by another name of it.
        assert!(super::save_as(&file, &how(None, false), &file).is_err());
        let other = dir.join("other.png");
        std::fs::hard_link(&file, &other).unwrap();
        assert!(super::save_as(&file, &how(None, false), &other).is_err());
        assert_eq!(image::image_dimensions(&file).unwrap(), (400, 200));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A crop of a file whose name is not UTF-8 keeps that name's bytes. "caf\xe9.png" became
    /// "caf\u{FFFD}-cropped.png", a name the user never chose.
    #[test]
    fn a_crop_keeps_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let dir = std::env::temp_dir().join(format!("glimt-crop-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9.png"));
        image::RgbImage::new(8, 8)
            .save_with_format(&file, image::ImageFormat::Png)
            .unwrap();
        let how = super::CopyAs {
            page: 0,
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: Some([0, 0, 4, 4]),
            long_side: None,
            format: None,
            jpeg_quality: 90,
            webp_quality: 90,
            strip: false,
        };
        let new = super::save_cropped(&file, &how).unwrap();
        assert_eq!(
            new.file_name().unwrap().to_owned().into_vec(),
            b"caf\xe9-cropped.png"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An XMP packet as Photoshop or Bridge writes one, with keywords, a thumbnail of the whole
    /// frame, the pixel sizes as attributes and an orientation.
    const BRIDGE_XMP: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF>\
        <rdf:Description tiff:Orientation=\"6\" tiff:ImageWidth=\"8\" \
        exif:PixelYDimension='8'><dc:subject><rdf:Bag><rdf:li>beach</rdf:li></rdf:Bag>\
        </dc:subject><xmp:Thumbnails><rdf:Alt><rdf:li rdf:parseType=\"Resource\">\
        <xmpGImg:image>/9j/WHOLEFRAME</xmpGImg:image></rdf:li></rdf:Alt></xmp:Thumbnails>\
        <xap:Thumbnails/></rdf:Description></rdf:RDF></x:xmpmeta>";

    /// A crop keeps no picture of the whole frame: not in its XMP (xmp:Thumbnails), nor in its
    /// sidecar's, nor in a PNG's Photoshop resources ("Raw profile type 8bim"). It keeps the
    /// keywords and a PNG's comment, and its sidecar's orientation is upright as its pixels.
    /// The XMP's thumbnail was copied as it was, so a face cropped out and shared still had
    /// the whole photo inside.
    #[test]
    fn a_crop_keeps_no_picture_of_the_whole_frame() {
        let dir = std::env::temp_dir().join(format!("glimt-crop-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let mut png = super::png_with_xmp(&black_png(8, 8), BRIDGE_XMP.as_bytes()).unwrap();
        png.splice(
            33..33,
            png_chunk(b"tEXt", b"Raw profile type 8bim\0WHOLEFRAME"),
        );
        png.splice(33..33, png_chunk(b"tEXt", b"Comment\0hello"));
        std::fs::write(&file, &png).unwrap();
        std::fs::write(crate::metadata::xmp::sidecar(&file), BRIDGE_XMP).unwrap();
        let how = super::CopyAs {
            page: 0,
            rotate: false,
            turns: 0,
            mirrored: false,
            crop: Some([0, 0, 4, 4]),
            long_side: None,
            format: None,
            jpeg_quality: 90,
            webp_quality: 90,
            strip: false,
        };
        let new = super::save_cropped(&file, &how).unwrap();
        let has =
            |bytes: &[u8], what: &str| bytes.windows(what.len()).any(|w| w == what.as_bytes());
        let copy = std::fs::read(&new).unwrap();
        let side = std::fs::read(crate::metadata::xmp::sidecar(&new)).unwrap();
        for bytes in [&copy, &side] {
            assert!(!has(bytes, "WHOLEFRAME") && !has(bytes, "Thumbnails"));
            assert!(!has(bytes, "ImageWidth") && !has(bytes, "PixelYDimension"));
            assert!(has(bytes, "<rdf:li>beach</rdf:li>"));
        }
        assert!(has(&copy, "hello"));
        assert!(has(&side, "tiff:Orientation=\"1\""));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn saved sets the orientation in the photo's sidecar too, so digiKam, which reads
    /// the sidecar first, shows it turned as Glimt does. A JPEG's sidecar gets the new EXIF
    /// orientation, and a PNG turned in its pixels gets 1 and loses the old thumbnail. Before,
    /// only the XMP inside a JPEG was set, and the sidecar turned the photo back.
    #[test]
    fn a_turn_saved_sets_its_sidecar() {
        let dir = std::env::temp_dir().join(format!("glimt-turn-side-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let has = |path: &std::path::Path, what: &str| {
            let side = std::fs::read(crate::metadata::xmp::sidecar(path)).unwrap();
            side.windows(what.len()).any(|w| w == what.as_bytes())
        };
        let jpeg = dir.join("a.jpg");
        image::RgbImage::new(8, 4).save(&jpeg).unwrap();
        let side = BRIDGE_XMP.replace("Orientation=\"6\"", "Orientation=\"1\"");
        std::fs::write(crate::metadata::xmp::sidecar(&jpeg), side).unwrap();
        crate::metadata::exif::write_orientation(&jpeg, 1, false, true).unwrap();
        assert!(has(&jpeg, "tiff:Orientation=\"6\""));
        assert!(has(&jpeg, "<rdf:li>beach</rdf:li>"));
        let png = dir.join("b.png");
        std::fs::write(&png, black_png(8, 4)).unwrap();
        std::fs::write(crate::metadata::xmp::sidecar(&png), BRIDGE_XMP).unwrap();
        super::save_rotated(&png, true, 1, false, 90).unwrap();
        assert_eq!(image::image_dimensions(&png).unwrap(), (4, 8));
        assert!(has(&png, "tiff:Orientation=\"1\""));
        assert!(!has(&png, "WHOLEFRAME") && has(&png, "<rdf:li>beach</rdf:li>"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn saved to a read-only JPEG or PNG is refused, and its sidecar is left byte for
    /// byte as it was. Before, the sidecar was changed before the image was refused, so it said
    /// the JPEG was turned, and the PNG's lost its thumbnail and pixel sizes for good.
    #[test]
    fn a_refused_turn_leaves_its_sidecar() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-turn-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (jpeg, png) = (dir.join("a.jpg"), dir.join("b.png"));
        image::RgbImage::new(8, 4).save(&jpeg).unwrap();
        std::fs::write(&png, black_png(8, 4)).unwrap();
        let side = BRIDGE_XMP.replace("Orientation=\"6\"", "Orientation=\"1\"");
        for file in [&jpeg, &png] {
            std::fs::write(crate::metadata::xmp::sidecar(file), &side).unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        let image = std::fs::read(&jpeg).unwrap();
        assert!(crate::metadata::exif::write_orientation(&jpeg, 1, false, true).is_err());
        assert!(super::save_rotated(&png, true, 1, false, 90).is_err());
        assert_eq!(std::fs::read(&jpeg).unwrap(), image);
        for file in [&jpeg, &png] {
            let now = std::fs::read_to_string(crate::metadata::xmp::sidecar(file)).unwrap();
            assert_eq!(now, side, "{}", file.display());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy drops Lightroom's crop and darktable's history, which were set on the old frame,
    /// and keeps the rest of the develop settings. Before, a crop of a photo cropped in Lightroom
    /// or darktable was cropped again there. The image itself keeps them when it is turned.
    #[test]
    fn a_copy_drops_other_programs_crop_and_the_image_keeps_it() {
        let xmp = b"<rdf:Description crs:Exposure2012=\"+0.50\" crs:HasCrop=\"True\" \
            crs:CropTop=\"0.1\" crs:CropLeft=\"0.2\" crs:CropBottom=\"0.9\" \
            crs:CropRight=\"0.8\" crs:CropAngle=\"0\" darktable:history_end=\"2\">\
            <darktable:masks_history><rdf:Seq/></darktable:masks_history>\
            <darktable:history><rdf:Seq><rdf:li darktable:operation=\"crop\"/></rdf:Seq>\
            </darktable:history></rdf:Description>";
        let has = |x: &[u8], what: &str| x.windows(what.len()).any(|w| w == what.as_bytes());
        let copy = super::derived_xmp(xmp, super::Derived::Copy);
        for gone in ["crs:HasCrop", "crs:Crop", "darktable:"] {
            assert!(
                !has(&copy, gone),
                "{gone}: {}",
                String::from_utf8_lossy(&copy)
            );
        }
        assert!(has(&copy, "crs:Exposure2012=\"+0.50\""));
        // The image's own edits stay when it is turned, a JPEG by its orientation and a PNG or
        // WebP by its pixels. A turned PNG lost the whole darktable history of its sidecar.
        for how in [super::Derived::Oriented(6), super::Derived::Turned] {
            let turned = super::derived_xmp(xmp, how);
            assert!(has(&turned, "crs:CropTop") && has(&turned, "darktable:operation=\"crop\""));
        }
    }

    /// A property is left out of XMP in one pass, also from a hostile packet, and never through
    /// a comment. Before, 100,000 properties to leave out moved the rest of the packet each
    /// time, and a `<!--<xmp:Thumbnails>-->` comment was cut to the next end tag, so the
    /// comment's end went and the packet was broken.
    #[test]
    fn leaves_xmp_properties_out_in_one_pass() {
        let without = |x: &str| String::from_utf8(super::without(x.as_bytes(), b"xmp:Thumbnails"));
        let kept = "<!--<xmp:Thumbnails>--><a/>";
        assert_eq!(
            without(&format!("{kept}<xmp:Thumbnails>t</xmp:Thumbnails><b/>")).unwrap(),
            format!("{kept}<b/>")
        );
        assert_eq!(
            without("<xmp:Thumbnails>t<!-- --></xmp:Thumbnails>").unwrap(),
            "<xmp:Thumbnails>t<!-- --></xmp:Thumbnails>"
        );
        assert_eq!(
            without("<a xmp:Thumbnails='1' b=\"2\" xmp:Thumbnails = \"3\"/><xmp:Thumbnails/>")
                .unwrap(),
            "<a b=\"2\"/>"
        );
        let started = std::time::Instant::now();
        let many = " xmp:Thumbnails=\"1\"".repeat(100_000);
        assert_eq!(without(&many).unwrap(), "");
        let open = "<xmp:Thumbnails>".repeat(100_000);
        assert_eq!(without(&open).unwrap(), open);
        let unclosed = "<xmp:Thumbnails".repeat(100_000);
        assert_eq!(without(&unclosed).unwrap(), unclosed);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    /// XMP packets broken in thousands of ways are made a derived file's without a panic or a
    /// loop.
    #[test]
    fn survives_broken_xmp_derived() {
        for bytes in crate::support::test_support::broken(BRIDGE_XMP.as_bytes()) {
            super::derived_xmp(&bytes, super::Derived::Copy);
        }
    }

    /// A copy gets the sidecar of what it was made from, or none. It never gets the one of a
    /// file it replaced (b's keywords showed on a copy saved over b.png, also on one saved
    /// without metadata), nor one left at its name. A copy of a TIFF has the page shown, not the
    /// first.
    #[test]
    fn a_copy_has_its_own_sidecar_and_page() {
        use tiff::encoder::{TiffEncoder, colortype};
        let dir = std::env::temp_dir().join(format!("glimt-copy-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]))
            .save(&a)
            .unwrap();
        image::RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 255]))
            .save(&b)
            .unwrap();
        let side = |p: &std::path::Path| crate::metadata::xmp::sidecar(p);
        let text = |p: &std::path::Path| std::fs::read_to_string(side(p)).ok();
        std::fs::write(side(&a), "a's").unwrap();
        let how = |page, strip| super::CopyAs {
            page,
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: None,
            long_side: None,
            format: None,
            jpeg_quality: 90,
            webp_quality: 90,
            strip,
        };
        std::fs::write(side(&b), "b's").unwrap();
        super::save_as(&a, &how(0, false), &b).unwrap();
        assert_eq!(text(&b).as_deref(), Some("a's"));
        std::fs::write(side(&b), "b's").unwrap();
        super::save_as(&a, &how(0, true), &b).unwrap();
        assert_eq!(text(&b), None);
        // A sidecar that cannot be removed (a folder in its place) leaves b as it was.
        std::fs::create_dir(side(&b)).unwrap();
        let before = std::fs::read(&b).unwrap();
        assert!(super::save_as(&a, &how(0, false), &b).is_err());
        assert_eq!(std::fs::read(&b).unwrap(), before);
        std::fs::remove_dir(side(&b)).unwrap();
        // A sidecar left at the crop's name is not the crop's.
        let cropped = dir.join("a-cropped.png");
        std::fs::write(side(&cropped), "left").unwrap();
        let part = super::CopyAs {
            crop: Some([0, 0, 4, 4]),
            ..how(0, false)
        };
        assert_eq!(super::save_cropped(&a, &part).unwrap(), cropped);
        assert_eq!(text(&cropped).as_deref(), Some("a's"));
        // A link left at the next crop's sidecar name, to a file of the user's, goes, and the
        // file keeps what it had.
        let victim = dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let second = dir.join("a-cropped-2.png");
        std::os::unix::fs::symlink(&victim, side(&second)).unwrap();
        assert_eq!(super::save_cropped(&a, &part).unwrap(), second);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(
            !std::fs::symlink_metadata(side(&second))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(text(&second).as_deref(), Some("a's"));
        // A copy saved over a link replaces the link, not the file it points to.
        let linked = dir.join("linked.png");
        std::os::unix::fs::symlink(&victim, &linked).unwrap();
        super::save_as(&a, &how(0, true), &linked).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!std::fs::symlink_metadata(&linked).unwrap().is_symlink());
        // A source sidecar that cannot be read (a folder in its place) leaves no copy behind.
        std::fs::remove_file(side(&a)).unwrap();
        std::fs::create_dir(side(&a)).unwrap();
        assert!(super::save_cropped(&a, &part).is_err());
        assert!(!dir.join("a-cropped-3.png").exists());
        std::fs::remove_dir(side(&a)).unwrap();
        // Page 2 of a TIFF is blue, 6 by 2, not the red first page.
        let tif = dir.join("t.tif");
        let mut enc = TiffEncoder::new(std::fs::File::create(&tif).unwrap()).unwrap();
        enc.write_image::<colortype::RGB8>(8, 8, &[255, 0, 0].repeat(64))
            .unwrap();
        enc.write_image::<colortype::RGB8>(6, 2, &[0, 0, 255].repeat(12))
            .unwrap();
        drop(enc);
        let page = super::save_as(&tif, &how(1, false), &dir.join("p.png")).unwrap();
        let img = image::open(&page).unwrap().into_rgb8();
        assert_eq!(
            (img.dimensions(), img.get_pixel(0, 0).0),
            ((6, 2), [0, 0, 255])
        );
        // Copy Image takes the same, page 2, turned a quarter as shown. (It copied page 1,
        // unturned.)
        let turned = super::CopyAs {
            turns: 1,
            ..how(1, false)
        };
        let shown = super::shown_image(&tif, &turned).unwrap().into_rgb8();
        assert_eq!(
            (shown.dimensions(), shown.get_pixel(0, 0).0),
            ((2, 6), [0, 0, 255])
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A copy waits while the lock of its sidecar is held, as a keyword written from another
    /// window holds it. Without the wait, that keyword's sidecar could land between the old
    /// sidecar's removal and the copy's, and the copy failed after its image was written. The
    /// image itself is written beside its name meanwhile, so a slow share does not hold the lock
    /// for the whole write.
    #[test]
    fn a_copy_waits_for_its_sidecar_lock() {
        let dir = std::env::temp_dir().join(format!("glimt-copy-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(crate::metadata::xmp::sidecar(&a), "a's").unwrap();
        let held = crate::metadata::xmp::held(&b);
        let saver = {
            let (a, b) = (a.clone(), b.clone());
            std::thread::spawn(move || super::land(&a, &b, b"x", false, false))
        };
        let written = || {
            (std::fs::read_dir(&dir).unwrap().flatten()).any(|e| {
                e.file_name().to_string_lossy().starts_with(".glimt-tmp-")
                    && e.metadata().is_ok_and(|m| m.len() == 1)
            })
        };
        let start = std::time::Instant::now();
        while !written() && start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(written(), "the image waited for the sidecar's lock");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!b.exists(), "saved while the sidecar was held");
        drop(held);
        saver.join().unwrap().unwrap();
        let side = std::fs::read_to_string(crate::metadata::xmp::sidecar(&b)).unwrap();
        assert_eq!(side, "a's");
        assert_eq!(std::fs::read(&b).unwrap(), b"x");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// When "a.png" and "a.webp" are saved turned at the same time, each keeps its own pixels
    /// and format.
    #[test]
    fn saves_same_names_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, webp) = (dir.join("a.png"), dir.join("a.webp"));
        assert_ne!(
            crate::files::file_io::tmp_path(&png),
            crate::files::file_io::tmp_path(&webp)
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
