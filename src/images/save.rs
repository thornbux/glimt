//! Saving a turned image. The pixels are turned, the EXIF is kept with orientation 1, and the
//! XMP is kept, all in the same format. (A lossy WebP gets them back in RIFF chunks.)

use crate::files::file_io::FileFingerprint;
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

/// The XMP properties every copy for sharing keeps, also after Remove All, as its EXIF keeps
/// their tags (`exif::shared_block`): the orientation keeps a photo upright, and the colour
/// space says which colours its pixels are in.
const ALWAYS_SHARED_XMP: [&str; 2] = ["tiff:Orientation", "exif:ColorSpace"];

/// The largest EXIF block a JPEG holds. Its APP1 segment holds 65,533 bytes, and the encoder
/// puts six of them, "Exif\0\0", before the block.
pub(crate) const MAX_JPEG_EXIF: usize = 65_527;

/// The IDs of the Photoshop resources that a turned PNG drops: the thumbnails of Photoshop 4 and
/// of 5 and later, which show the frame unturned, and the copies of the EXIF and the XMP, which
/// hold the old orientation.
const DROPPED_PHOTOSHOP_RESOURCES: [u16; 4] = [0x0409, 0x040C, 0x0422, 0x0424];

// Makes `same_file_by_opening` on this thread find this name gone, while a look still finds it,
// as on a share whose cache still shows a name that the server removed.
#[cfg(test)]
thread_local! {
    pub(crate) static GONE_FROM_THE_SERVER: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

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
enum Encoding {
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

/// What tells one file or folder from every other, as `file_id` gives it: its device and
/// inode, or its real path where the file system gives every file inode 0.
pub(crate) type FileId = Result<(u64, u64), std::path::PathBuf>;

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
    /// a PNG's old copies of its EXIF and XMP, which it gets anew, and in EXIF the thumbnail,
    /// with the pixel sizes set anew). A PNG's Photoshop resources are kept without their
    /// thumbnail. Other programs' edits of the image stay, as they do on a JPEG's turn.
    Turned,
    /// A new file made from the image: a crop or a copy (Save As). It drops what `Turned` drops,
    /// and other programs' edits of the image it came from (`DROPPED_FROM_COPIES`).
    Copy,
    /// A copy for sharing (`images::shared_copy`): the image's own data byte for byte, with only
    /// the metadata the user keeps. `shared_metadata` reads what it may keep, and
    /// `SharedMetadata::written` makes the new EXIF block and XMP packet of what it keeps.
    Shared,
}

/// What a copy for sharing (`Derived::Shared`) may keep of an image's metadata: its EXIF block,
/// and the XMP properties of the image, of its sidecar and of its IPTC IIM, merged.
pub(crate) struct SharedMetadata {
    exif: Option<Vec<u8>>,
    xmp: crate::metadata::xmp::Properties,
}

impl SharedMetadata {
    /// Returns the names of the tags a copy for sharing may keep or leave out: the EXIF block's
    /// (`exif::shared_tag_names`) and the XMP properties'. The orientation and the colour space
    /// are not among them, as every copy keeps them.
    pub(crate) fn tag_names(&self) -> Vec<String> {
        let exif = self.exif.as_deref().map(exif::shared_tag_names);
        let xmp = (self.xmp.list.iter())
            .map(|(name, ..)| name.clone())
            .filter(|name| !ALWAYS_SHARED_XMP.contains(&name.as_str()));
        exif.unwrap_or_default().into_iter().chain(xmp).collect()
    }

    /// Puts the element `element` in place of the XMP property `name`, with the namespaces
    /// `namespaces` that bind its prefixes, when there is such a property.
    pub(crate) fn replace_property(
        &mut self,
        name: &str,
        element: String,
        namespaces: crate::metadata::xmp::Namespaces,
    ) {
        if let Some(property) = self.xmp.list.iter_mut().find(|(n, ..)| n == name) {
            (property.1, property.2) = (element, namespaces);
        }
    }

    /// Returns the new EXIF block (`exif::shared_block`) and XMP packet of a copy that keeps
    /// the tags `keeps` keeps, and always the orientation and the colour space. With
    /// `keeps_hdr_headroom`, a copy that leaves out an iPhone's maker note keeps its HDR
    /// headroom and gain, which its gain map needs. Each is `None` when nothing of it is kept.
    /// A property kept keeps only its own value (`xmp::with_own_value_only`): a known tag
    /// written inside it goes unless `keeps` keeps that tag, and so does any tag of the image's
    /// own that is left out. The fields of a structure, such as a face region's name, are no
    /// known tags and stay with it.
    pub(crate) fn written(
        &self,
        keeps: &dyn Fn(&str) -> bool,
        keeps_hdr_headroom: bool,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let exif = (self.exif.as_deref())
            .and_then(|raw| exif::shared_block(raw, keeps, keeps_hdr_headroom));
        let mut kept = self.xmp.clone();
        kept.list
            .retain(|(name, ..)| ALWAYS_SHARED_XMP.contains(&name.as_str()) || keeps(name));
        let is_own_tag = |name: &str| self.xmp.list.iter().any(|(n, ..)| n == name);
        let keeps_inside = |name: &str| {
            keeps(name) || !(crate::images::shared_copy::is_known(name) || is_own_tag(name))
        };
        for (_, element, namespaces) in &mut kept.list {
            *element =
                crate::metadata::xmp::with_own_value_only(element, namespaces, &keeps_inside);
        }
        let xmp = (!kept.list.is_empty()).then(|| crate::metadata::xmp::packet(&kept));
        (exif, xmp)
    }
}

/// Saves the view's rotation in a PNG or WebP, which have no orientation field to set. The
/// pixels are turned, and the file is written again with its ICC profile, EXIF block and XMP
/// (keywords, comment, and alt text other programs wrote into it). PNG and lossless WebP keep
/// every pixel. A lossy WebP is encoded again by ffmpeg at `webp_quality`, and gets its ICC
/// profile, EXIF block and XMP back afterwards. The new file replaces the old in one rename.
/// shortcut: a PNG with a palette or of 1, 2 or 4-bit grey is written in 8-bit RGB(A) or grey,
/// as the image crate writes neither, so its file grows; write it with the png crate if that
/// is reported.
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
    crate::files::file_io::check_writable(path).map_err(|e| e.to_string())?;
    // Taken before any read, so a file changed while it is turned is not written over.
    let read =
        crate::files::file_io::fingerprint_before_reading(path).map_err(|e| e.to_string())?;
    let png = has_ext(path, &["png"]);
    if !png && !has_ext(path, &["webp"]) {
        return Err(tr("only JPEG, PNG and WebP can store the rotation").into());
    }
    if animated(path)? {
        return Err(tr("animated images are not turned").into());
    }
    let encoding = match png {
        true => Encoding::Png,
        false => Encoding::WebP(webp_lossless(path) == Some(false)),
    };
    let (img, icc) = load_icc(path, rotate)?;
    let img = turned(img, turns, mirrored);
    let size = (img.width(), img.height());
    let (exif, xmp, quarter) = derived_metadata(path, rotate, turns, size, Derived::Turned)?;
    let mut out = encode(path, img, icc, exif, xmp, encoding, quarter, webp_quality)?;
    if png {
        out = png_with_photoshop_resources(&out, path)?;
    }
    replace_with_sidecar(path, named, read, out, Derived::Turned)
}

/// Replaces the image at `real` with `out` and makes the sidecar of `named`, the name the list
/// shows, say what the new image does (`how`). The sidecar is changed only once the image's own
/// write can no longer fail: the file is writable and still there and `out` is on disk beside
/// it. Before, the sidecar was changed first, so a read-only image or a full disk left a
/// sidecar that said the image was turned. This is the one way an image and its sidecar are
/// written again together, for a JPEG's orientation and a PNG or WebP turned. `read` is the
/// fingerprint of the image that `out` was made from (`file_io::replace_existing`).
pub(crate) fn replace_with_sidecar(
    real: &Path,
    named: &Path,
    read: FileFingerprint,
    out: Vec<u8>,
    how: Derived,
) -> Result<(), String> {
    // The fingerprint finds a copier that wrote on after the read, but not one that paused
    // meanwhile. The file's own end shows that it is not all there yet.
    let bytes = crate::images::decode::read_limited(real).map_err(|e| e.to_string())?;
    if let Some(why) = why_cut_short(&bytes, changed_lately(read.2)) {
        return Err(why);
    }
    drop(bytes);
    let step = |lock: &crate::metadata::xmp::SidecarLock| derive_sidecar(lock, named, how);
    replace_with_sidecar_locked(real, named, read, out, step)
}

/// Returns whether a file last changed at `modified` (seconds and nanoseconds since 1970, as a
/// fingerprint holds it) was changed within the last minute, or less than a minute ahead of
/// this computer's clock (a share's clock may run ahead). A file still being copied in was.
fn changed_lately(modified: (i64, i64)) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    (now - modified.0).abs() < 60
}

/// Returns why the JPEG, PNG or WebP `data` is not turned when it `looks_cut_short`, and `None`
/// when it does not. A JPEG whose image data is not reached may be damaged before it, as well
/// as still being copied, so it is not called the one. `changed_lately` says whether the file
/// was changed in the last minute.
fn why_cut_short(data: &[u8], changed_lately: bool) -> Option<String> {
    if !looks_cut_short(data, changed_lately) {
        return None;
    }
    if data.starts_with(&[0xff, 0xd8]) && exif::jpeg_scan_start(data).is_none() {
        return Some(tr("the file looks incomplete or damaged").into());
    }
    Some(tr("the file looks incomplete, as if it is still being copied").into())
}

/// Returns whether the JPEG, PNG or WebP `data` ends before its own end shows. That is a JPEG
/// with no end marker (FF D9) after its image data, or shorter than its own lengths say of what
/// follows that image (`jpeg_trailer_is_cut`), a PNG without its IEND chunk or with a chunk
/// longer than the file, or a WebP shorter than its RIFF header says. A file that is still being
/// copied in looks so.
///
/// The lengths of what follows a JPEG's image are counted only when the file `changed_lately`.
/// They are the file's own words, and an editor may leave them wrong. So an older file whose
/// lengths do not add up is turned, and it keeps what follows its image byte for byte. Before,
/// such a file was refused for good, as if it were still being copied.
fn looks_cut_short(data: &[u8], changed_lately: bool) -> bool {
    if data.starts_with(&[0xff, 0xd8]) {
        // Inside the image data, a 0xff byte is always followed by 0 or a restart marker.
        let scan = crate::metadata::exif::jpeg_scan_start(data);
        let eoi =
            scan.and_then(|s| Some(s + data[s..].windows(2).position(|w| w == [0xff, 0xd9])?));
        return eoi.is_none_or(|eoi| changed_lately && jpeg_trailer_is_cut(data, eoi + 2));
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut i = 8;
        while let Some(head) = data.get(i..i + 8) {
            let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
            // The chunk's length, type, data and CRC.
            let next = i.saturating_add(12).saturating_add(len);
            if next > data.len() {
                return true;
            }
            if &head[4..] == b"IEND" {
                return false;
            }
            i = next;
        }
        return true;
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        let size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        return data.len() < 8 + size;
    }
    false
}

/// Returns whether what follows the main image of the JPEG `data`, which ends at `image_end`, is
/// shorter than the file's own lengths say: the images that its MPF index names (a camera's
/// second image, a gain map), the items of its XMP `Container:Directory`, or the video at the
/// end that `GCamera:MicroVideoOffset` counts back to (a phone's motion photo). An offset of 0
/// means that the photo has no video. The first end marker is only the main image's end, so a
/// copy that paused in the video passed before and was written back without the rest. Data
/// that none of them names is no matter.
fn jpeg_trailer_is_cut(data: &[u8], image_end: usize) -> bool {
    if image_end >= data.len() {
        return false;
    }
    let mpf = exif::jpeg_segment_with(data, 0xe2, b"MPF\0");
    // The offsets count from the index's TIFF header, after "MPF\0".
    let mpf_end = mpf.and_then(|(at, body)| {
        let entries = exif::mp_entries(body)?;
        let ends = entries.iter().filter(|e| e[2] != 0);
        ends.map(|&[_, size, offset]| at as u64 + 4 + offset as u64 + size as u64)
            .max()
    });
    let xmp = exif::jpeg_segment_with(data, 0xe1, b"http://ns.adobe.com/xap/1.0/\0");
    let xmp = xmp
        .map(|(_, body)| String::from_utf8_lossy(body))
        .unwrap_or_default();
    // Only the photo's own properties count, not those of another document inside one, such
    // as a placed image in xmpMM:Pantry.
    let properties = crate::metadata::xmp::top_level_properties(xmp.as_bytes());
    let named = |found: &&crate::metadata::xmp::TopLevelProperty, name: &str| {
        xmp[found.name.clone()] == *name
    };
    let property = |name: &str| properties.iter().find(|found| named(found, name));
    let value_of = |name: &str| property(name).map(|found| &xmp[found.value.clone()]);
    // The first item is the main image, and the others follow it in their order. Their
    // lengths are read in every form XMP writes them in, also as the elements that ExifTool
    // writes, which were read as 0 before.
    let directory = property("Container:Directory").filter(|found| !found.is_attribute);
    let items = directory.map_or_else(Vec::new, |found| {
        crate::metadata::xmp::structure_items(
            xmp.as_bytes(),
            found.value.clone(),
            b"Container:Item",
        )
    });
    let lengths = items.iter().skip(1).flat_map(|fields| {
        ["Item:Length", "Item:Padding"].map(|name| {
            let field = fields.iter().find(|found| named(found, name));
            field.and_then(|found| leading_number(&xmp[found.value.clone()]))
        })
    });
    let items_length: u64 = lengths.flatten().sum();
    let offset = value_of("GCamera:MicroVideoOffset").and_then(leading_number);
    let offset = offset.filter(|&back| back != 0);
    let video_is_cut = offset.is_some_and(|back| {
        let start = (data.len() as u64)
            .checked_sub(back)
            .filter(|&s| s >= image_end as u64);
        let start = start.map(|s| s as usize);
        start.and_then(|s| data.get(s + 4..s + 8)) != Some(b"ftyp")
    });
    let length = data.len() as u64;
    video_is_cut
        || mpf_end.is_some_and(|end| length < end)
        || length < image_end as u64 + items_length
}

/// Returns the number that an XMP value `value` starts with, after its blanks, or `None` when
/// it starts with no digit.
fn leading_number(value: &str) -> Option<u64> {
    let value = value.trim_start();
    let digits = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    value[..digits].parse().ok()
}

/// Replaces the image at `real` with `out` after `step` changed the sidecar of `named`. The
/// sidecar's lock (`xmp::lock_sidecar`) is held from the step until the image has its new name. A
/// move in another window holds the same lock, so it moves the image and its sidecar together, both
/// before the step or both after the image is replaced. Before, the lock was let go after the step,
/// and a rename in that moment took the changed sidecar to the new name while the image, found
/// gone, was left as it was.
fn replace_with_sidecar_locked(
    real: &Path,
    named: &Path,
    read: FileFingerprint,
    out: Vec<u8>,
    step: impl FnOnce(&crate::metadata::xmp::SidecarLock) -> Result<(), String>,
) -> Result<(), String> {
    let lock = std::cell::OnceCell::new();
    let before = || {
        let lock = lock.get_or_init(|| crate::metadata::xmp::lock_sidecar(named));
        step(lock).map_err(std::io::Error::other)
    };
    crate::files::file_io::replace_existing(real, out, read, before).map_err(|e| e.to_string())?;
    crate::files::journal::note_written_anew(named, read);
    Ok(())
}

/// The copy of `path` that `how` asks for, encoded, and its format. It has its profile, and
/// unless stripped its EXIF (upright) and XMP. JPEG and a lossy WebP are at their quality (the
/// WebP through ffmpeg), and PNG and lossless WebP are without loss.
pub fn copy_bytes(path: &Path, how: &CopyAs) -> Result<(Vec<u8>, Format), String> {
    if animated(path)? {
        return Err(tr("animated images are not saved as a copy").into());
    }
    let format = how.format.unwrap_or_else(|| Format::of(path));
    let encoding = match format {
        Format::Jpeg => Encoding::Jpeg(how.jpeg_quality),
        Format::Png => Encoding::Png,
        Format::WebP => Encoding::WebP(false),
        Format::WebPLossy => Encoding::WebP(true),
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
    let out = encode(
        from,
        img,
        icc,
        exif,
        xmp,
        encoding,
        quarter,
        how.webp_quality,
    )?;
    Ok((out, format))
}

/// The pixels of `path` as `how` asks for them, with its ICC profile. That is the page shown,
/// turned as shown, the part, and smaller. Every copy of the shown image starts from these,
/// whether it is saved or put on the clipboard.
fn pixels(path: &Path, how: &CopyAs) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    let (img, icc) = crate::images::decode::load_page(path, how.page, how.rotate)?;
    let mut img = turned(img, how.turns, how.mirrored);
    if let Some([x, y, width, height]) = how.crop {
        if width == 0 || height == 0 || x + width > img.width() || y + height > img.height() {
            return Err(tr("the part is outside the image").into());
        }
        img = img.crop_imm(x, y, width, height);
    }
    if let Some(long) = how.long_side {
        img = shrink_to_long_side(img, long.max(1))?;
    }
    Ok((img, icc))
}

/// A frame or page `img`, in sRGB, as the PNG that Save This Frame and Export Frames write.
/// It is turned as the view showed it (`how`), as Save As turns its copy. Before, a frame
/// saved from a turned view came out unturned. It goes through `encode` as every copy does, so
/// a 32-bit float page is written in 16 bits.
pub fn frame_png(img: image::DynamicImage, how: &CopyAs) -> Result<Vec<u8>, String> {
    let img = turned(img, how.turns, how.mirrored);
    encode(
        Path::new(""),
        img,
        None,
        None,
        None,
        Encoding::Png,
        false,
        0,
    )
}

/// The image as `how` shows it, in sRGB, for Edit → Copy Image. That copied the image
/// unturned, and only its first page.
pub fn shown_image(path: &Path, how: &CopyAs) -> Result<image::DynamicImage, String> {
    let (img, icc) = pixels(path, how)?;
    Ok(crate::images::colour::to_srgb(img, icc.as_deref()))
}

/// Shrinks `img` so its longest side is `long` at most, by Lanczos3, which is sharp, for a copy
/// kept.
fn shrink_to_long_side(img: image::DynamicImage, long: u32) -> Result<image::DynamicImage, String> {
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

/// Returns the name of `path` without its extension, as its bytes, with `more` after it, as a
/// copy's name beside it. A name that is not UTF-8 is kept as it is, not made "caf\u{FFFD}".
pub(crate) fn stem_with_suffix(path: &Path, more: &str) -> std::ffi::OsString {
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
            1 => stem_with_suffix(path, &format!("-cropped.{ext}")),
            _ => stem_with_suffix(path, &format!("-cropped-{n}.{ext}")),
        };
        let new = path.with_file_name(name);
        match save_copy_with_sidecar(path, &new, &out, None, how.strip) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
            Ok(()) => return Ok(new),
        }
    }
    unreachable!()
}

/// Saves a copy of `path`, encoded as `out`, as `new`. It goes over a file there only when
/// `replace` is that file's fingerprint, taken when the file dialog asked about it, and only
/// while the file there is still that one. The copy gets `path`'s sidecar, or none when
/// `strip` or when `path` has none. A sidecar at `new`'s name belongs to the replaced file, or
/// was left without its image, and its keywords and comment are not the copy's. So it is
/// removed and the copy's is made new, never written through a link there, which a downloaded
/// folder may have pointed at any file of the user's (~/.bashrc). This is the one way a copy is
/// saved, for Crop, Save As and Save This Frame.
pub(crate) fn save_copy_with_sidecar(
    path: &Path,
    new: &Path,
    out: &[u8],
    replace: Option<crate::files::file_io::FileFingerprint>,
    strip: bool,
) -> std::io::Result<()> {
    // The lock of the copy's sidecar is held, as every writer of a sidecar holds it. A keyword
    // written to the replaced file from another window then waits, rather than landing between
    // the old sidecar's removal and the new one's creation. The image is written beside its name
    // before the lock is taken, because that is the slow part on a share, and other sidecars with
    // the same sidecar lock number would wait for it. The lock is taken before the image takes
    // its name and is held until the copy's sidecar is in place.
    let mut lock = None;
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
    #[allow(clippy::disallowed_methods)] // the replaced file's sidecar
    let remove_side = || match std::fs::remove_file(&side) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(named(e)),
        _ => Ok(()),
    };
    match replace {
        // The replaced file's sidecar goes after the image is written beside it and before the
        // image takes the name. A failed write then leaves the file and its sidecar, and a
        // sidecar that cannot be removed leaves the file, never with the copy's pixels under its
        // own keywords and stars. A link there is replaced, not the file it points to. Another
        // file put at the name since the dialog asked is left alone.
        Some(asked_about) => {
            use std::io::Write;
            crate::files::file_io::replace_name_with(
                new,
                |f| f.write_all(out),
                || {
                    crate::files::file_io::is_still_the_file(
                        new,
                        crate::files::file_io::KnownAs::fingerprint(asked_about),
                    )?;
                    lock = Some(crate::metadata::xmp::lock_sidecar(new));
                    remove_side()
                },
            )?;
            crate::files::journal::note_written_anew(new, asked_about);
        }
        // A file of that name is not touched, nor its sidecar, because the copy is refused first.
        None => {
            use std::io::Write;
            let tmp = crate::files::file_io::tmp_path(new);
            crate::files::file_io::write_beside(
                &tmp,
                new,
                |f| f.write_all(out),
                |tmp| {
                    lock = Some(crate::metadata::xmp::lock_sidecar(new));
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
    use std::os::unix::fs::MetadataExt;
    let named = |p: &Path| {
        let m = std::fs::metadata(p).ok()?;
        Some((
            file_id_from_metadata(p, &m),
            (m.len(), m.mtime(), m.mtime_nsec()),
        ))
    };
    match (named(a), named(b)) {
        (Some(x), Some(y)) => same_named(x, y),
        _ => false,
    }
}

/// Whether `a` and `b` are names of one file, as `same_file` says, but by opening each name
/// rather than looking it up. On a share, a look may be answered from the cache, which still
/// shows a name that the server removed when its answer was lost. An open asks the server
/// (NFS's close-to-open, SMB, SFTP). So a name may be removed as a second name of a file only
/// when this says so, and a name that cannot be opened counts as not there.
pub(crate) fn same_file_by_opening(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let named = |p: &Path| {
        #[cfg(test)]
        if GONE_FROM_THE_SERVER.with_borrow(|gone| gone.as_deref() == Some(p)) {
            return None;
        }
        let (_file, m) = crate::images::decode::open_regular_with_metadata(p).ok()?;
        Some((
            file_id_from_metadata(p, &m),
            (m.len(), m.mtime(), m.mtime_nsec()),
        ))
    };
    match (named(a), named(b)) {
        (Some(x), Some(y)) => same_named(x, y),
        _ => false,
    }
}

/// Whether two names, each by its key and its (size, modification time in seconds and
/// nanoseconds), are of one file. One file has one size and time under every name. The size
/// and time are compared too because NFSv3 with `nohide` and overlayfs without `xino` can give
/// two files the same device and inode, and Rename Many's repair removes a name that it takes
/// for a second name of the same file. Where the file system gives no inode, two names that
/// differ only in case are one file when the file system ignores case (`case_folded`).
fn same_named(
    (id_a, at_a): (FileId, (u64, i64, i64)),
    (id_b, at_b): (FileId, (u64, i64, i64)),
) -> bool {
    at_a == at_b
        && match (id_a, id_b) {
            (Err(a), Err(b)) => a == b || case_folded(&a, &b),
            (a, b) => a == b,
        }
}

/// Whether `a` and `b`, two names of files that are there, in one folder and differing only in
/// case, reach one file because the file system ignores case. A file system that keeps case
/// lists both names, since both are there. One that ignores case lists the file once, under
/// the name it was made with, so at least one of the two is missing from the listing. Save As
/// `a.JPG` to `a.jpg` on sshfs with `-o use_ino` from a Mac would otherwise write over the
/// image with its own copy.
/// shortcut: the folder is listed whole, which is only done without inodes and for names that
/// differ only in case; names that differ in Unicode normalization (as APFS ignores) are still
/// taken for two files, fold that too if such a share is met.
fn case_folded(a: &Path, b: &Path) -> bool {
    let name = |p: &Path| {
        p.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned)
    };
    let (Some(x), Some(y)) = (name(a), name(b)) else {
        return false;
    };
    let Some(folder) = a.parent().filter(|&f| Some(f) == b.parent()) else {
        return false;
    };
    if x == y || x.to_lowercase() != y.to_lowercase() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    let mut listed = [false; 2];
    for entry in entries {
        // A listing cut short says nothing, so the names stay two files.
        let Ok(entry) = entry else {
            return false;
        };
        let found = entry.file_name();
        listed[0] |= found.to_str() == Some(x.as_str());
        listed[1] |= found.to_str() == Some(y.as_str());
    }
    listed != [true, true]
}

/// Returns what tells the file or folder at `path` from every other (`FileId`). Every
/// comparison of two files or folders by who they are goes through it. Returns `None` when the
/// file cannot be looked up.
pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    Some(file_id_from_metadata(path, &std::fs::metadata(path).ok()?))
}

/// Returns the id of `path` from its metadata `meta`, which the caller looked up itself, for
/// example without following a link. The inode is read here and in no other place, so no
/// comparison can take inode 0 for one file (clippy.toml refuses the inode elsewhere).
#[allow(clippy::disallowed_methods)] // the one place an inode tells files apart
pub(crate) fn file_id_from_metadata(path: &Path, meta: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    file_id_from_inode(path, (meta.dev(), meta.ino()))
}

/// Returns the identity of the file that the folder entry `entry` names: the inode the entry
/// gives with the name, so no file is looked at. Returns `None` for inode 0. A listing of
/// 500,000 files cannot look at each file, so the inode is read here as well as in
/// `file_id_from_metadata`, and in no other place.
#[allow(clippy::disallowed_methods)] // a folder entry's inode, which costs no look at the file
pub(crate) fn identity_of_entry(entry: &std::fs::DirEntry) -> crate::files::file_io::Identity {
    use std::os::unix::fs::DirEntryExt;
    std::num::NonZeroU64::new(entry.ino())
}

/// Returns the id of `path` from its device and inode, `id`. The pair is the same however the
/// file is reached, through a link, a hard link or a bind mount, where its real path differs.
/// But sshfs with `-o use_ino` and some other mounts give every file inode 0, so the pair
/// would make all their files one, and the real path tells them apart instead. A test gives
/// such ids through this function.
pub(crate) fn file_id_from_inode(path: &Path, id: (u64, u64)) -> FileId {
    match id.1 {
        0 => Err(crate::model::listing::real_dir(path)),
        _ => Ok(id),
    }
}

/// File → Save As. It saves the copy `how` asks for as `to`, which the file dialog let replace
/// the file there whose fingerprint was `at_pick`. When the extension is made the format's, it
/// is never saved over a file the dialog did not ask about, and never over the image itself.
/// Returns the file saved.
pub fn save_as(
    path: &Path,
    how: &CopyAs,
    to: &Path,
    at_pick: Option<crate::files::file_io::FileFingerprint>,
) -> Result<std::path::PathBuf, String> {
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
    save_copy_with_sidecar(path, &to, &out, at_pick.filter(|_| matches), how.strip)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(to)
}

/// Whether `path` is animated. An animation is refused, because only the first frame would be
/// written back. It asks by what the file is, because it is read by its content. An animated
/// GIF named a.png is still animated, and a JPEG named so is not.
fn animated(path: &Path) -> Result<bool, String> {
    let format = crate::images::decode::image_reader(path)
        .map_err(|e| e.to_string())?
        .format();
    let file = || crate::images::decode::open_regular(path).map(std::io::BufReader::new);
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
    // The EXIF block comes from the panel's reader, also for the formats the image crate
    // cannot open.
    let mut exif = exif::derived(path, how);
    let (mut xmp, stored) = crate::images::decode::image_reader(path)
        .ok()
        .and_then(|r| r.into_decoder().ok())
        .map(|mut d| {
            let stored = rotate.then(|| exif::stored_orientation(&mut d, exif.as_deref()));
            (d.xmp_metadata().ok().flatten(), stored)
        })
        .unwrap_or_default();
    // A PNG's EXIF and XMP that the decoder does not read, after the pixels, compressed or in
    // ImageMagick's raw profiles, are read here, by the reader of every chunk the PNG written
    // may drop (`png_with_chunks`). Their orientation was not shown, so `stored` is read
    // without it.
    if has_ext(path, &["png"])
        && (xmp.is_none() || exif.is_none())
        && let Ok(bytes) = crate::images::decode::read_limited(path)
    {
        let found = exif::png_metadata(&bytes);
        exif = exif.or_else(|| exif::derived_of(&found.exif?, how));
        xmp = xmp.or(found.xmp);
    }
    if let Some(exif) = &mut exif {
        exif::set_upright(exif, size)?;
    }
    if let Some(xmp) = &mut xmp {
        *xmp = derived_xmp(xmp, how);
    }
    let stored_quarter = stored.is_some_and(crate::images::decode::is_quarter_turn);
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
                out = xmp_without_property(&out, name.as_bytes());
            }
        }
        // A copy for sharing writes a new packet of the properties kept instead
        // (`SharedMetadata::written`), and its pixels and orientation are the image's own.
        Derived::Shared => {}
    }
    out
}

/// Reads what a copy for sharing of `path` may keep of its metadata. That is the EXIF block
/// `embedded_exif` that the walk of the image found, and the XMP properties of `embedded`, the
/// packet inside the image, of its sidecar and of `iim`, the properties its IPTC IIM gives. A
/// property of the sidecar, which Glimt writes, wins over one of the same name inside the image,
/// and IIM only adds what neither has. Returns an error when the sidecar is there but cannot be
/// read, as a copy of the image without it would leave out the alt text the user wrote for it.
pub(crate) fn shared_metadata(
    path: &Path,
    embedded_exif: Option<&[u8]>,
    embedded: Option<&[u8]>,
    iim: crate::metadata::xmp::Properties,
) -> Result<SharedMetadata, String> {
    use crate::metadata::xmp;
    // A PNG without an eXIf chunk may keep its EXIF in a text chunk, as exiv2 and GIMP write it,
    // which the panel's reader unpacks (`exif::derived`).
    let is_png = crate::model::listing::has_ext(path, &["png"]);
    let exif = match embedded_exif {
        Some(block) => Some(block.to_vec()),
        None if is_png => exif::derived(path, Derived::Shared),
        None => None,
    };
    let text = |bytes: &[u8]| std::str::from_utf8(bytes).ok().map(xmp::properties);
    let mut properties = embedded.and_then(text).unwrap_or_default();
    let sidecar = xmp::own_sidecar(path).map_err(|e| e.to_string())?;
    properties.add_over(sidecar.as_deref().and_then(text).unwrap_or_default());
    properties.add_missing(iim);
    Ok(SharedMetadata {
        exif,
        xmp: properties,
    })
}

/// Makes the sidecar of `path`, whose image is being written anew as `how` says, say what the
/// image will. Otherwise a program that reads the sidecar first (digiKam) turns the photo back
/// by its old orientation. It is called after the new image is written whole and before it
/// takes the name (`replace_with_sidecar`): a sidecar that cannot be written then leaves the
/// image as it was, and a failed image write leaves the sidecar. Only a sidecar of its own,
/// readable as text and with something to change, is written, under its lock `lock`. A link or
/// one that cannot be read is left alone, as the panel leaves it. The sidecar is the one of the
/// name the list shows, which is the one Glimt's panel reads, also for a link to a photo.
fn derive_sidecar(
    lock: &crate::metadata::xmp::SidecarLock,
    path: &Path,
    how: Derived,
) -> Result<(), String> {
    let old = crate::metadata::xmp::own_sidecar(path).ok().flatten();
    let text = old.filter(|x| std::str::from_utf8(x).is_ok());
    if text.is_none_or(|x| derived_xmp(&x, how) == x) {
        return Ok(());
    }
    crate::metadata::xmp::update_locked_sidecar(lock, path, true, |x| {
        String::from_utf8(derived_xmp(x.as_bytes(), how)).ok()
    })
}

/// Leaves the property `name` out of `xmp`, written as an attribute (` name="…"` or with single
/// quotes) or as an element (`<name …>…</name>` or `<name …/>`), every time it is a top-level
/// property of an rdf:Description (`xmp::top_level_properties`). One inside another property is
/// left as it is, such as the rating of another document in xmpMM:Pantry. An element goes with
/// the white space before it, so a sidecar written again and again does not gather empty lines.
/// Every XMP editor that replaces a property takes the old one out here, as a search for one
/// spelling misses the others (`xmp:Rating='3'`) and leaves the property twice. A property with
/// a comment or a CDATA section inside it goes whole too, as the walk reads its end past them.
/// Before, one with a comment was left, and the editor then wrote the property a second time.
pub(crate) fn xmp_without_property(xmp: &[u8], name: &[u8]) -> Vec<u8> {
    let (mut out, mut copied) = (Vec::with_capacity(xmp.len()), 0);
    for found in crate::metadata::xmp::top_level_properties(xmp) {
        if xmp[found.name] != *name {
            continue;
        }
        let start = found.whole.start;
        let start = match found.is_attribute {
            true if start > copied && xmp[start - 1].is_ascii_whitespace() => start - 1,
            true => start,
            false => copied + xmp[copied..start].trim_ascii_end().len(),
        };
        out.extend_from_slice(&xmp[copied..start]);
        copied = found.whole.end;
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

/// `img` encoded as `encoding` with the profile, EXIF and XMP. A PNG keeps the other chunks of
/// `path` (`quarter` says it was turned a quarter since). It is encoded in memory, because an
/// encoder that owns its file flushes it on drop, where a full disk would go unnoticed and a
/// cut-off file would replace the photo. The pixels and the profile are made to fit `encoding`
/// first (`writable_pixels`), so every derived file is written from pixels its format holds.
#[allow(clippy::too_many_arguments)] // what a derived file is made of, each told apart
fn encode(
    path: &Path,
    img: image::DynamicImage,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
    encoding: Encoding,
    quarter: bool,
    webp_quality: u8,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let (img, icc) = writable_pixels(img, icc, encoding);
    let (width, height) = (img.width(), img.height());
    if let Encoding::WebP(true) = encoding {
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
    // A PNG's old chunks of EXIF or XMP are dropped only when the new file carries them anew.
    // Photoshop's resources are always dropped, because they may hold a thumbnail of the whole
    // frame; a turn puts them back without it (`png_with_photoshop_resources`).
    let carried = (exif.is_some(), xmp.is_some());
    let carries = |kind| match kind {
        exif::PngMetadataKind::Exif => carried.0,
        exif::PngMetadataKind::Xmp => carried.1,
        exif::PngMetadataKind::PhotoshopResources => true,
    };
    let mut out = Vec::new();
    let written = match encoding {
        Encoding::Png => {
            let mut enc = image::codecs::png::PngEncoder::new(&mut out);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            match exif.map(|e| enc.set_exif_metadata(e)) {
                Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
                _ => img.write_with_encoder(enc),
            }
        }
        Encoding::Jpeg(quality) => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            // A block larger than an APP1 segment holds is left out rather than fail the copy.
            let exif = exif.filter(|e| e.len() <= MAX_JPEG_EXIF);
            match exif.map(|e| enc.set_exif_metadata(e)) {
                Some(Err(e)) => Err(image::ImageError::Unsupported(e)),
                _ => img.write_with_encoder(enc),
            }
        }
        Encoding::WebP(_) => {
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
    if let Encoding::Png = encoding
        && has_ext(path, &["png"])
        && let Ok(old) = crate::images::decode::read_limited(path)
    {
        out = png_with_chunks(&out, &old, quarter, &carries);
    }
    if let Some(xmp) = &xmp {
        out = match encoding {
            Encoding::Png => png_with_xmp(&out, xmp)?,
            Encoding::Jpeg(_) => jpeg_with_xmp(&out, xmp)?,
            Encoding::WebP(_) => webp_with_chunk(&out, b"XMP ", xmp, width, height)?,
        };
    }
    Ok(out)
}

/// Makes `img` pixels that `encoding` writes, and keeps `icc` only where it describes them. A JPEG
/// holds 8-bit grey or RGB, a WebP 8-bit RGB or RGBA, and a PNG no floats, so a 32-bit float
/// TIFF is written in 16 bits. Before, a PNG copy of such a TIFF failed. A profile is kept only
/// when it fits the pixels (`colour::profile_fits`). Before, a grey JPEG's Gray profile went
/// with RGB pixels, and a CMYK JPEG's profile with the RGB it is decoded to, so other programs
/// showed the copy in wrong colours.
fn writable_pixels(
    img: image::DynamicImage,
    icc: Option<Vec<u8>>,
    encoding: Encoding,
) -> (image::DynamicImage, Option<Vec<u8>>) {
    use image::DynamicImage as D;
    let colour = img.color();
    let img = match encoding {
        Encoding::Jpeg(_) if !colour.has_color() => D::from(img.into_luma8()),
        Encoding::Jpeg(_) => D::from(img.into_rgb8()),
        Encoding::WebP(_) if colour.has_alpha() => D::from(img.into_rgba8()),
        Encoding::WebP(_) => D::from(img.into_rgb8()),
        Encoding::Png => match img {
            D::ImageRgb32F(_) => D::from(img.into_rgb16()),
            D::ImageRgba32F(_) => D::from(img.into_rgba16()),
            img => img,
        },
    };
    let fits = |p: &Vec<u8>| crate::images::colour::profile_fits(p, img.color());
    let icc = icc.filter(fits);
    (img, icc)
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
    let mut chunk = b"iTXt".to_vec();
    // Keyword, no compression, no language, no translated keyword, then the text.
    chunk.extend(b"XML:com.adobe.xmp\0\0\0\0\0");
    chunk.extend(xmp);
    png_with_chunk_after_header(png, &chunk)
}

/// Returns `png` with the chunk `chunk` (its name and data) right after the header (IHDR), with
/// its length and CRC.
fn png_with_chunk_after_header(png: &[u8], chunk: &[u8]) -> Result<Vec<u8>, String> {
    // The 8-byte signature comes first, then IHDR, with its length 13, its name, 13 bytes and a
    // CRC.
    let end = 33;
    if png.get(12..16) != Some(&b"IHDR"[..]) || png.len() < end {
        return Err("not a PNG file".into());
    }
    let mut out = png[..end].to_vec();
    out.extend(((chunk.len() - 4) as u32).to_be_bytes());
    out.extend(chunk);
    out.extend(crc32(chunk).to_be_bytes());
    out.extend(&png[end..]);
    Ok(out)
}

/// Puts the Photoshop resources ("Raw profile type 8bim") of the PNG at `old` into the turned
/// PNG `new`, without their thumbnail and old orientation (`DROPPED_PHOTOSHOP_RESOURCES`). They
/// hold the IPTC caption, copyright and keywords that ImageMagick writes there. `png_with_chunks`
/// drops them from every file written with new pixels, because the thumbnail shows the whole
/// frame, so a crop or copy keeps none of them. Resources that cannot be read stay dropped, as
/// before.
fn png_with_photoshop_resources(new: &[u8], old: &Path) -> Result<Vec<u8>, String> {
    let resources = (crate::images::decode::read_limited(old).ok())
        .and_then(|bytes| exif::png_metadata(&bytes).photoshop_resources)
        .and_then(|bytes| without_dropped_photoshop_resources(&bytes));
    let Some(resources) = resources.filter(|r| !r.is_empty()) else {
        return Ok(new.to_vec());
    };
    // Written as ImageMagick writes a raw profile: its type, its length and 36 bytes a line.
    let hex: Vec<String> = (resources.chunks(36))
        .map(|line| line.iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    let text = format!("\n8bim\n{:8}\n{}\n", resources.len(), hex.join("\n"));
    let chunk = [&b"tEXtRaw profile type 8bim\0"[..], text.as_bytes()].concat();
    png_with_chunk_after_header(new, &chunk)
}

/// Returns Photoshop's image resources `bytes` without the thumbnails and the copies of the EXIF
/// and XMP (`DROPPED_PHOTOSHOP_RESOURCES`). Each resource is a signature, an ID, a name of even
/// length and its data, padded to even. Returns `None` when a resource runs past the end, so
/// that resources that cannot be read are never kept with a thumbnail inside.
fn without_dropped_photoshop_resources(bytes: &[u8]) -> Option<Vec<u8>> {
    let (mut kept, mut at) = (Vec::new(), 0);
    while at < bytes.len() {
        let id = u16::from_be_bytes(bytes.get(at + 4..at + 6)?.try_into().ok()?);
        let name_len = *bytes.get(at + 6)? as usize;
        let size_at = at + 6 + (1 + name_len).next_multiple_of(2);
        let size = u32::from_be_bytes(bytes.get(size_at..size_at + 4)?.try_into().ok()?);
        let end = (size_at + 4).checked_add(size as usize)?;
        if end > bytes.len() {
            return None;
        }
        // The last resource may leave out its padding byte.
        let next = (end + (size as usize) % 2).min(bytes.len());
        if !DROPPED_PHOTOSHOP_RESOURCES.contains(&id) {
            kept.extend_from_slice(&bytes[at..next]);
        }
        at = next;
    }
    Some(kept)
}

/// The PNG `new` with the chunks of `old` that still hold for its pixels put after its header.
/// They are text, physical size, time, colour space and the HDR colour chunks (cICP, mDCV,
/// cLLI, in both spellings), which hold for the same pixel values. The profile is
/// not taken, because it is written anew from the decoder's reading of it, which the PNG allows
/// only before the pixels, nor what depends on how the pixels are stored (palette,
/// transparency, bit depth). A chunk of EXIF, XMP or Photoshop resources
/// (`exif::png_metadata_kind`) is dropped only when `carries` says the new file holds that kind
/// anew, and it is kept otherwise.
fn png_with_chunks(
    new: &[u8],
    old: &[u8],
    quarter: bool,
    carries: &dyn Fn(exif::PngMetadataKind) -> bool,
) -> Vec<u8> {
    const KEPT: [&[u8; 4]; 13] = [
        b"tEXt", b"zTXt", b"iTXt", b"pHYs", b"tIME", b"gAMA", b"cHRM", b"sRGB", b"cICP", b"mDCV",
        b"mDCv", b"cLLI", b"cLLi",
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
        let metadata = exif::png_metadata_kind(name, &chunk[8..8 + len]);
        let carried = metadata.is_some_and(carries);
        if name == b"pHYs" && swapped && len == 9 {
            let body = [&b"pHYs"[..], &chunk[12..16], &chunk[8..12], &chunk[16..17]].concat();
            kept.extend(9u32.to_be_bytes());
            kept.extend(&body);
            kept.extend(crc32(&body).to_be_bytes());
        } else if (KEPT.iter().any(|k| *k == name) || metadata.is_some()) && !carried {
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
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
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
    let mut file = crate::images::decode::open_regular(path).ok()?;
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
/// `external::PROGRAM_TIME_LIMIT`.
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
    // No busy pointer: it feeds a program, which the caller's thread waits for.
    let feed = std::thread::spawn(move || drop(stdin.as_mut().map(|i| i.write_all(&png))));
    let max = crate::images::decode::MAX_DECODE_BYTES;
    let (webp, err, ok) = crate::files::external::output_of(
        &mut child,
        max,
        crate::files::external::PROGRAM_TIME_LIMIT,
    );
    // Its write's failure shows in ffmpeg's output, and a panic is passed on.
    (feed.join()).unwrap_or_else(|e| std::panic::resume_unwind(e));
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
    /// panic. A damaged JPEG, PNG or WebP is looked at for its end with no panic.
    #[test]
    fn survives_broken_files_given_a_chunk() {
        let mut png = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        for bytes in crate::support::test_support::broken(&png) {
            let _ = super::png_with_xmp(&bytes, b"<x:xmpmeta/>");
            let _ = super::looks_cut_short(&bytes, true);
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
            let _ = super::looks_cut_short(&bytes, true);
        }
        let mut jpeg = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        for bytes in crate::support::test_support::broken(&jpeg) {
            let _ = super::looks_cut_short(&bytes, true);
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

    /// A turned image with another name (a hard link) is saved, and the window is told once
    /// that the other name keeps the old image. A file with one name is saved without that note.
    #[test]
    fn tells_that_a_hard_link_keeps_the_old_image() {
        let dir = std::env::temp_dir().join(format!("glimt-hard-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, other, alone) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        std::fs::write(&file, black_png(4, 2)).unwrap();
        std::fs::write(&alone, black_png(4, 2)).unwrap();
        std::fs::hard_link(&file, &other).unwrap();
        let noted = || crate::files::file_io::OTHER_NAMES_NOTED.with_borrow_mut(|n| n.take_note());
        noted();
        super::save_rotated(&file, false, 1, false, 90).unwrap();
        assert_eq!(image::image_dimensions(&file).unwrap(), (2, 4));
        assert_eq!(std::fs::read(&other).unwrap(), black_png(4, 2));
        let told = noted();
        assert!(
            told.as_ref().is_some_and(|t| t.contains("a.png")),
            "{told:?}"
        );
        super::save_rotated(&alone, false, 1, false, 90).unwrap();
        assert_eq!(noted(), None);
        // A save that fails after the file was found writable (a GIF is not turned) gives no
        // note, as the other names were not left behind.
        let (gif, gif_link) = (dir.join("g.gif"), dir.join("h.gif"));
        std::fs::write(&gif, "GIF89a").unwrap();
        std::fs::hard_link(&gif, &gif_link).unwrap();
        assert!(super::save_rotated(&gif, false, 1, false, 90).is_err());
        assert_eq!(noted(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turned PNG keeps its extended attributes, where KDE's Dolphin keeps its tags. Before,
    /// the new file had none, so a turn in Glimt lost them without a word.
    #[test]
    fn a_turned_image_keeps_its_extended_attributes() {
        use crate::support::test_support::{attribute, set_attribute};
        let dir = std::env::temp_dir().join(format!("glimt-turn-xattr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        std::fs::write(&file, black_png(4, 2)).unwrap();
        if set_attribute(&file, "user.test", b"kept") {
            super::save_rotated(&file, false, 1, false, 90).unwrap();
            assert_eq!(image::image_dimensions(&file).unwrap(), (2, 4));
            assert_eq!(attribute(&file, "user.test").as_deref(), Some(&b"kept"[..]));
        } else {
            eprintln!("no extended attributes on this file system: not tested");
        }
        std::fs::remove_dir_all(dir).unwrap();
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
        let out = super::png_with_chunks(&new, &old, false, &|_| true);
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
        let out = super::png_with_chunks(&tall, &old, true, &|_| true);
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
        let copy = super::save_as(
            &file,
            &how(1),
            &dir.join("b.png"),
            crate::files::file_io::fingerprint(&dir.join("b.png")),
        )
        .unwrap();
        assert_eq!(dpi(&copy), swapped);
        let copy = super::save_as(
            &file,
            &how(2),
            &dir.join("c.png"),
            crate::files::file_io::fingerprint(&dir.join("c.png")),
        )
        .unwrap();
        assert_eq!(dpi(&copy), [0, 0, 0, 204, 0, 0, 0, 98, 1]);
        super::save_rotated(&file, true, 1, false, 90).unwrap();
        assert_eq!(dpi(&file), swapped);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG is written also when its EXIF block is larger than an APP1 segment holds, without
    /// the block, and a block that fits is kept. A RAW's EXIF can be that large.
    #[test]
    fn a_jpeg_is_written_without_an_exif_block_too_large() {
        use crate::support::test_support::CANON_EXIF;
        use image::ImageDecoder;
        let exif_of = |exif: Vec<u8>| {
            let img = image::RgbImage::new(2, 2).into();
            let encoding = super::Encoding::Jpeg(90);
            let path = std::path::Path::new("");
            let out = super::encode(path, img, None, Some(exif), None, encoding, false, 90);
            let reader = image::ImageReader::new(std::io::Cursor::new(out.unwrap()));
            let mut dec = reader
                .with_guessed_format()
                .unwrap()
                .into_decoder()
                .unwrap();
            dec.exif_metadata().unwrap()
        };
        assert_eq!(exif_of(CANON_EXIF.to_vec()).as_deref(), Some(CANON_EXIF));
        let mut large = CANON_EXIF.to_vec();
        large.resize(super::MAX_JPEG_EXIF + 1, 0);
        assert_eq!(exif_of(large), None);
    }

    /// A derived file is written from pixels its format holds: a float image as a 16-bit PNG,
    /// and grey as a grey JPEG. Before, a PNG of a 32-bit float TIFF failed to be written.
    #[test]
    fn a_derived_file_has_pixels_its_format_holds() {
        use image::ImageDecoder;
        let write = |img: image::DynamicImage, encoding| {
            let out = super::encode(
                std::path::Path::new(""),
                img,
                None,
                None,
                None,
                encoding,
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
            write(float.into(), super::Encoding::Png),
            image::ColorType::Rgb16
        );
        let float = image::Rgba32FImage::from_pixel(2, 2, image::Rgba([0.5, 0.25, 1.0, 1.0]));
        assert_eq!(
            write(float.into(), super::Encoding::Png),
            image::ColorType::Rgba16
        );
        let grey = image::GrayImage::from_pixel(2, 2, image::Luma([90]));
        assert_eq!(
            write(grey.into(), super::Encoding::Jpeg(90)),
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
        let profile = |img: image::DynamicImage, icc: &Vec<u8>, encoding| {
            let out = super::encode(
                std::path::Path::new(""),
                img,
                Some(icc.clone()),
                None,
                None,
                encoding,
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
        use super::Encoding::{Jpeg, Png, WebP};
        for encoding in [Jpeg(90), Png] {
            assert_eq!(profile(grey(), &gray, encoding), Some(gray.clone()));
            assert_eq!(profile(colour(), &gray, encoding), None);
            assert_eq!(profile(colour(), &rgb, encoding), Some(rgb.clone()));
            assert_eq!(profile(colour(), &cmyk, encoding), None);
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
            super::png_with_chunks(&tall, &broken, true, &|_| true);
        }
    }

    /// A PNG with a palette and transparency keeps every pixel when it is turned, as the manual
    /// says, though it is written in full colour.
    #[test]
    fn a_turned_palette_png_keeps_its_pixels() {
        let dir = std::env::temp_dir().join(format!("glimt-turn-palette-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let mut enc = png::Encoder::new(std::fs::File::create(&file).unwrap(), 3, 1);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Two);
        enc.set_palette(vec![255, 0, 0, 0, 255, 0, 0, 0, 255]);
        enc.set_trns(vec![255, 128]);
        let mut writer = enc.write_header().unwrap();
        // The indices 0, 1 and 2, two bits each.
        writer.write_image_data(&[0b0001_1000]).unwrap();
        writer.finish().unwrap();
        let before = image::open(&file).unwrap().into_rgba8();
        super::save_rotated(&file, false, 1, false, 90).unwrap();
        let after = image::open(&file).unwrap().into_rgba8();
        assert_eq!(after, image::imageops::rotate90(&before));
        assert_eq!(before.get_pixel(1, 0).0, [0, 255, 0, 128]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A Photoshop image resource of the kind `id` holding `data`, with an empty name.
    fn photoshop_resource(id: u16, data: &[u8]) -> Vec<u8> {
        let mut resource = [&b"8BIM"[..], &id.to_be_bytes(), &[0, 0]].concat();
        resource.extend((data.len() as u32).to_be_bytes());
        resource.extend(data);
        if data.len() % 2 == 1 {
            resource.push(0);
        }
        resource
    }

    /// A PNG turned in place keeps its Photoshop resources, where ImageMagick keeps the IPTC
    /// caption and copyright, without the thumbnail among them. Before, a turn dropped them all,
    /// as a crop does (`a_crop_keeps_no_picture_of_the_whole_frame`). The resources' copies of
    /// the EXIF and XMP go too, because they hold the old orientation.
    #[test]
    fn a_turned_png_keeps_its_photoshop_resources_without_the_thumbnail() {
        let dir = std::env::temp_dir().join(format!("glimt-turn-8bim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let resources = [
            photoshop_resource(0x0404, b"COPYRIGHT"),
            photoshop_resource(0x040C, b"WHOLEFRAME"),
            photoshop_resource(0x0422, b"EXIF ORIENTATION 6"),
            photoshop_resource(0x0424, b"XMP ORIENTATION 6"),
        ]
        .concat();
        let key = "Raw profile type 8bim";
        crate::support::test_support::png_with_raw_profile(&file, "zTXt", key, &resources);
        super::save_rotated(&file, false, 1, false, 90).unwrap();
        let png = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&file).unwrap()));
        let reader = png.read_info().unwrap();
        let keys: [&[u8]; 1] = [key.as_bytes()];
        let texts: Vec<String> = crate::metadata::exif::png_texts(reader.info(), &keys).collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        let kept = crate::metadata::exif::raw_profile_bytes(&texts[0]).unwrap();
        assert_eq!(kept, photoshop_resource(0x0404, b"COPYRIGHT"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Photoshop resources broken in thousands of ways are read without a panic or a loop.
    #[test]
    fn survives_broken_photoshop_resources() {
        let resources = [
            photoshop_resource(0x0404, b"COPYRIGHT"),
            photoshop_resource(0x0409, b"WHOLEFRAME"),
        ]
        .concat();
        for broken in crate::support::test_support::broken(&resources) {
            super::without_dropped_photoshop_resources(&broken);
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

    /// A PNG turned keeps its XMP and EXIF when they come after the pixels, where ExifTool adds
    /// XMP. Before, the decoder read neither, and the turned PNG dropped both chunks, so the
    /// keywords were lost from the user's own file.
    #[test]
    fn a_turned_png_keeps_the_metadata_after_its_pixels() {
        use image::ImageDecoder;
        let dir = std::env::temp_dir().join(format!("glimt-turn-after-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        image::RgbImage::new(4, 2).save(&file).unwrap();
        // IFD0 with Make "Canon".
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x0f\0\x02\0\0\0\x06\0\0\0\x1a\0\0\0\0Canon\0";
        let xmp = b"<x:xmpmeta>Oslo</x:xmpmeta>";
        let chunk = |body: Vec<u8>| {
            let mut out = ((body.len() - 4) as u32).to_be_bytes().to_vec();
            out.extend(&body);
            out.extend(super::crc32(&body).to_be_bytes());
            out
        };
        let mut bytes = std::fs::read(&file).unwrap();
        let iend = bytes.len() - 12;
        let after = [
            chunk([&b"iTXtXML:com.adobe.xmp\0\0\0\0\0"[..], xmp].concat()),
            chunk([&b"eXIf"[..], exif].concat()),
        ];
        bytes.splice(iend..iend, after.concat());
        std::fs::write(&file, &bytes).unwrap();
        super::save_rotated(&file, false, 1, false, 90).unwrap();
        let mut dec = image::ImageReader::open(&file)
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (2, 4));
        assert_eq!(dec.xmp_metadata().unwrap().as_deref(), Some(&xmp[..]));
        let rows = crate::metadata::exif::read(&file);
        assert!(rows.contains(&("Camera", "Canon".to_string())), "{rows:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A PNG turned keeps the XMP and EXIF that sit only in a chunk the turn drops: ImageMagick's
    /// raw XMP profile, XMP compressed after the pixels, and a raw EXIF profile after them.
    /// Before, the turn dropped each of these chunks without reading it, so the keywords or the
    /// camera and date were lost from the user's own file.
    #[test]
    fn a_turned_png_keeps_metadata_in_every_chunk_it_drops() {
        use crate::support::test_support::{CANON_EXIF, png_with_texts, raw_profile_text};
        use image::ImageDecoder;
        let dir = std::env::temp_dir().join(format!("glimt-turn-forms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let xmp = "<x:xmpmeta>Oslo</x:xmpmeta>";
        let raw_xmp = raw_profile_text("xmp", xmp.as_bytes());
        let raw_exif = raw_profile_text("exif", CANON_EXIF);
        let cases = [
            (
                "raw XMP profile",
                ("zTXt", "Raw profile type xmp", &raw_xmp[..]),
                true,
            ),
            (
                "compressed XMP after",
                ("iTXt", "XML:com.adobe.xmp", xmp),
                false,
            ),
            (
                "raw EXIF after",
                ("zTXt", "Raw profile type exif", &raw_exif[..]),
                false,
            ),
        ];
        let mut lost = Vec::new();
        for (case, text, before) in cases {
            std::fs::write(&file, png_with_texts(&[text], before)).unwrap();
            super::save_rotated(&file, false, 1, false, 90).unwrap();
            let kept = match case {
                "raw EXIF after" => {
                    let rows = crate::metadata::exif::read(&file);
                    rows.iter()
                        .any(|(label, value)| *label == "Camera" && value.contains("Canon"))
                }
                _ => {
                    let reader = image::ImageReader::open(&file).unwrap();
                    let mut dec = reader.into_decoder().unwrap();
                    let xmp = dec.xmp_metadata().unwrap().unwrap_or_default();
                    xmp.windows(4).any(|w| w == b"Oslo")
                }
            };
            if !kept {
                lost.push(case);
            }
        }
        assert!(lost.is_empty(), "lost: {lost:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every chunk of what is said about a PNG that a PNG written with new pixels drops, when the
    /// new file carries every kind of metadata, is one that `exif::png_metadata` reads back, and
    /// none is dropped when the new file carries none. The chunks are every text chunk of
    /// `PNG_METADATA_TEXT` and others, eXIf, and the colour and size chunks that hold for the
    /// same pixel values. A list of dropped chunks kept apart from the reader once dropped XMP
    /// and EXIF that nothing read, and the HDR colour chunks were dropped as well.
    #[test]
    fn a_png_chunk_is_dropped_only_when_it_is_read_back() {
        use crate::support::test_support::{CANON_EXIF, raw_profile_text};
        let raw = raw_profile_text("x", CANON_EXIF);
        let keys = (crate::metadata::exif::PNG_METADATA_TEXT.iter())
            .map(|(key, _)| *key)
            .chain([
                &b"Raw profile type iptc"[..],
                b"Raw profile type icc",
                b"Comment",
            ]);
        let mut chunks: Vec<(&[u8], Vec<u8>)> = keys
            .map(|key| {
                let text = match key {
                    b"XML:com.adobe.xmp" => b"<x:xmpmeta>Oslo</x:xmpmeta>".to_vec(),
                    _ => raw.as_bytes().to_vec(),
                };
                (&b"tEXt"[..], [key, b"\0", &text].concat())
            })
            .collect();
        chunks.push((b"eXIf", CANON_EXIF.to_vec()));
        chunks.push((b"tIME", vec![7, 234, 1, 2, 3, 4, 5]));
        chunks.push((b"pHYs", vec![0, 0, 11, 19, 0, 0, 11, 19, 1]));
        chunks.push((b"gAMA", 45_455u32.to_be_bytes().to_vec()));
        chunks.push((b"cHRM", vec![1; 32]));
        chunks.push((b"sRGB", vec![0]));
        // BT.2100 PQ, as an HDR PNG says it, and its mastering display and light levels.
        chunks.push((b"cICP", vec![9, 16, 0, 1]));
        for name in [b"mDCV", b"mDCv"] {
            chunks.push((name, vec![2; 24]));
        }
        for name in [b"cLLI", b"cLLi"] {
            chunks.push((name, vec![3; 8]));
        }
        let new = black_png(2, 2);
        for (name, body) in chunks {
            let chunk = png_chunk(name, &body);
            let mut old = new.clone();
            old.splice(old.len() - 12..old.len() - 12, chunk.clone());
            let has = |out: &[u8]| out.windows(chunk.len()).any(|w| w == chunk);
            let shown = if name == b"tEXt" {
                &body[..body.len().min(21)]
            } else {
                name
            };
            let label = String::from_utf8_lossy(shown).into_owned();
            assert!(
                has(&super::png_with_chunks(&new, &old, false, &|_| false)),
                "{label}"
            );
            if !has(&super::png_with_chunks(&new, &old, false, &|_| true)) {
                let found = crate::metadata::exif::png_metadata(&old);
                let read = [found.exif, found.xmp, found.photoshop_resources];
                assert!(
                    read.iter().any(Option::is_some),
                    "{label} dropped, not read"
                );
            }
        }
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

    /// A copy of a HEIC or RAW photo takes the EXIF the panel shows, which the image crate
    /// cannot read from them, as a bare TIFF block cut to what its directories name. Before, a
    /// copy of such a photo had no EXIF, and a RAW's whole 1 MB head would not fit a JPEG.
    #[test]
    fn a_copy_of_a_heic_or_raw_keeps_its_exif() {
        use crate::support::test_support::CANON_EXIF;
        let dir = std::env::temp_dir().join(format!("glimt-heic-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let picture = vec![0x55; 200_000];
        let heic = [
            &b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\x10"[..],
            b"Exif\0\0",
            CANON_EXIF,
            &picture,
        ];
        let raw = [CANON_EXIF, &picture];
        let mut files = vec![("a.nef", raw.concat())];
        // A HEIC is read only when built with it.
        if cfg!(feature = "heic") {
            files.push(("a.heic", heic.concat()));
        }
        for (name, bytes) in files {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            let (exif, _, _) =
                super::derived_metadata(&file, true, 0, (4, 4), super::Derived::Copy).unwrap();
            let exif = exif.expect(name);
            assert_eq!(exif.len(), CANON_EXIF.len(), "{name}");
            assert!(
                exif.starts_with(b"MM\0*") && exif.ends_with(b"03:04:05\0"),
                "{name}"
            );
            let upright = image::metadata::Orientation::from_exif_chunk(&exif);
            assert_eq!(upright, Some(image::metadata::Orientation::NoTransforms));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A PNG turned keeps its whole EXIF block, also what lies past the values its directories
    /// name, and an Exif directory named by a pointer of type IFD (13), as libtiff writes it.
    /// Before, the turned file got the block cut as for a copy, which lost both, and with them
    /// the date the photo was taken.
    #[test]
    fn a_turned_png_keeps_its_whole_exif() {
        use crate::support::test_support::CANON_EXIF;
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-turned-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut block = [CANON_EXIF, b"a SubIFDs directory"].concat();
        // The Exif pointer's type, LONG (4), is made IFD (13).
        block[37] = 13;
        let file = dir.join("a.png");
        let mut out = std::fs::File::create(&file).unwrap();
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        enc.set_exif_metadata(block.clone()).unwrap();
        image::RgbImage::new(4, 4).write_with_encoder(enc).unwrap();
        let (exif, _, _) =
            super::derived_metadata(&file, true, 0, (4, 4), super::Derived::Turned).unwrap();
        let exif = exif.unwrap();
        assert_eq!(exif.len(), block.len());
        assert!(exif.ends_with(b"03:04:05\0a SubIFDs directory"));
        let upright = image::metadata::Orientation::from_exif_chunk(&exif);
        assert_eq!(upright, Some(image::metadata::Orientation::NoTransforms));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A WebP whose EXIF chunk starts with "Exif\0\0", as some programs write it, gives a
    /// JPEG or PNG copy an EXIF block that starts with its TIFF header. Before, a JPEG copy had
    /// the header twice and a PNG copy an eXIf chunk no program reads.
    #[test]
    fn a_copy_of_a_webp_with_an_exif_header_has_one() {
        use crate::support::test_support::CANON_EXIF;
        use image::{ImageDecoder, ImageEncoder};
        let dir = std::env::temp_dir().join(format!("glimt-webp-exif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.webp");
        let mut out = std::fs::File::create(&file).unwrap();
        let mut enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
        enc.set_exif_metadata([&b"Exif\0\0"[..], CANON_EXIF].concat())
            .unwrap();
        image::RgbImage::new(4, 4).write_with_encoder(enc).unwrap();
        for format in [super::Format::Jpeg, super::Format::Png] {
            let how = super::CopyAs {
                page: 0,
                rotate: true,
                turns: 0,
                mirrored: false,
                crop: None,
                long_side: None,
                format: Some(format),
                jpeg_quality: 90,
                webp_quality: 90,
                strip: false,
            };
            let to = super::save_as(
                &file,
                &how,
                &dir.join("b"),
                crate::files::file_io::fingerprint(&dir.join("b")),
            )
            .unwrap();
            let mut dec = image::ImageReader::open(&to)
                .unwrap()
                .into_decoder()
                .unwrap();
            let exif = dec.exif_metadata().unwrap().expect("an EXIF block");
            assert!(exif.starts_with(b"MM\0*"), "{}", to.display());
        }
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
            crate::files::file_io::fingerprint(&dir.join("b")),
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
            crate::files::file_io::fingerprint(&dir.join("c.webp")),
        );
        let mut dec = image::ImageReader::open(to.unwrap())
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.dimensions(), (100, 50));
        assert!(dec.exif_metadata().unwrap().is_none());
        // Never the image itself, also asked for by name, or by another name of it.
        assert!(
            super::save_as(
                &file,
                &how(None, false),
                &file,
                crate::files::file_io::fingerprint(&file)
            )
            .is_err()
        );
        let other = dir.join("other.png");
        std::fs::hard_link(&file, &other).unwrap();
        assert!(
            super::save_as(
                &file,
                &how(None, false),
                &other,
                crate::files::file_io::fingerprint(&other)
            )
            .is_err()
        );
        assert_eq!(image::image_dimensions(&file).unwrap(), (400, 200));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Save As replaces only the file the chooser let it replace. A file put at a name that was
    /// free at the pick, or put in the place of the file picked, stays as it is.
    #[test]
    fn saves_as_only_over_the_file_picked() {
        use crate::files::file_io::fingerprint;
        let dir = std::env::temp_dir().join(format!("glimt-save-as-picked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        image::RgbImage::new(4, 4).save(&a).unwrap();
        let how = super::CopyAs {
            page: 0,
            rotate: true,
            turns: 0,
            mirrored: false,
            crop: None,
            long_side: None,
            format: None,
            jpeg_quality: 90,
            webp_quality: 90,
            strip: false,
        };
        std::fs::write(&b, "picked").unwrap();
        let at_pick = fingerprint(&b);
        std::fs::remove_file(&b).unwrap();
        std::fs::write(&b, "another one").unwrap();
        assert!(super::save_as(&a, &how, &b, at_pick).is_err());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "another one");
        std::fs::write(&c, "theirs").unwrap();
        assert!(super::save_as(&a, &how, &c, None).is_err());
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "theirs");
        super::save_as(&a, &how, &b, fingerprint(&b)).unwrap();
        assert_eq!(image::image_dimensions(&b).unwrap(), (4, 4));
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

    /// A JPEG, PNG or WebP whose end is not there yet, as one still being copied in, is not
    /// turned, and it is left byte for byte. Before, a JPEG was turned from the part copied so
    /// far, and the copier then wrote the rest into the file taken away, so the photo was left
    /// cut off at its name. The PNG and WebP decoders refuse such a file too, and the end is
    /// checked for them all the same.
    #[test]
    fn a_file_cut_short_is_not_turned() {
        let dir = std::env::temp_dir().join(format!("glimt-turn-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let whole = |name: &str, format| {
            let mut bytes = Vec::new();
            let img = image::RgbImage::from_fn(64, 32, |x, y| image::Rgb([x as u8, y as u8, 7]));
            img.write_to(&mut std::io::Cursor::new(&mut bytes), format)
                .unwrap();
            (dir.join(name), bytes)
        };
        let (jpeg, jpeg_bytes) = whole("a.jpg", image::ImageFormat::Jpeg);
        let (png, png_bytes) = whole("b.png", image::ImageFormat::Png);
        let (webp, webp_bytes) = whole("c.webp", image::ImageFormat::WebP);
        // The JPEG without its end marker, the PNG without its IEND chunk, and the WebP
        // without its last byte.
        let cut = [
            (&jpeg, &jpeg_bytes[..jpeg_bytes.len() - 2]),
            (&png, &png_bytes[..png_bytes.len() - 12]),
            (&webp, &webp_bytes[..webp_bytes.len() - 1]),
        ];
        for (file, bytes) in cut {
            std::fs::write(file, bytes).unwrap();
        }
        assert!(crate::metadata::exif::write_orientation(&jpeg, 1, false, true).is_err());
        assert!(super::save_rotated(&png, false, 1, false, 90).is_err());
        assert!(super::save_rotated(&webp, false, 1, false, 90).is_err());
        for (file, bytes) in cut {
            assert_eq!(std::fs::read(file).unwrap(), bytes, "{}", file.display());
        }
        // Whole, each is turned.
        for (file, bytes) in [
            (&jpeg, &jpeg_bytes),
            (&png, &png_bytes),
            (&webp, &webp_bytes),
        ] {
            assert!(!super::looks_cut_short(bytes, true), "{}", file.display());
        }
        std::fs::write(&jpeg, &jpeg_bytes).unwrap();
        crate::metadata::exif::write_orientation(&jpeg, 1, false, true).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG whose image data is not reached is called incomplete or damaged, not still being
    /// copied, as a wrong segment length before it looks the same. One cut in its image data is
    /// still being copied.
    #[test]
    fn a_jpeg_without_image_data_is_called_damaged() {
        let mut jpeg = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // SOI and the whole JFIF segment, and nothing after.
        let damaged = super::why_cut_short(&jpeg[..20], true).unwrap();
        assert_eq!(
            damaged,
            crate::support::lang::tr("the file looks incomplete or damaged")
        );
        let copied = super::why_cut_short(&jpeg[..jpeg.len() - 2], true).unwrap();
        assert!(copied.contains("still being copied"), "{copied}");
        assert_eq!(super::why_cut_short(&jpeg, true), None);
    }

    /// A camera's MPF JPEG or a phone's motion photo whose copy paused after the main image is
    /// not turned, and it is left byte for byte. Before, the main image's end marker passed it,
    /// and the second image or the video was cut off.
    #[test]
    fn a_jpeg_cut_after_its_main_image_is_not_turned() {
        use crate::images::shared_copy::{jpeg_segment, mpf_segment};
        let mut main = Vec::new();
        image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([x as u8, y as u8, 7]))
            .write_to(
                &mut std::io::Cursor::new(&mut main),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let second = main.clone();
        // The MPF segment goes after SOI, so its TIFF header is at 10 and the main image is
        // 90 bytes longer.
        let main_size = (main.len() + 90) as u32;
        let mpf = mpf_segment(
            [0x030000, 0],
            main_size,
            second.len() as u32,
            main_size - 10,
        );
        let mpf_file = [&main[..2], &mpf, &main[2..], &second].concat();
        let video = [&b"\0\0\0\x18ftypmp42"[..], &[7; 200]].concat();
        let with_xmp = |xmp: String| {
            let body = [&b"http://ns.adobe.com/xap/1.0/\0"[..], xmp.as_bytes()].concat();
            [&main[..2], &jpeg_segment(0xe1, &body), &main[2..], &video].concat()
        };
        let micro_video = with_xmp(format!(
            "<rdf:Description GCamera:MicroVideoOffset=\"{}\"/>",
            video.len()
        ));
        let directory = |length: usize| {
            format!(
                "<Container:Directory><rdf:Seq><rdf:li><Container:Item \
                 Item:Semantic=\"Primary\"/></rdf:li><rdf:li><Container:Item \
                 Item:Semantic=\"MotionPhoto\" Item:Length=\"{length}\"/></rdf:li></rdf:Seq>\
                 </Container:Directory>"
            )
        };
        let container = with_xmp(format!(
            "<rdf:Description>{}</rdf:Description>",
            directory(video.len())
        ));
        // ExifTool writes each item's fields as elements when it rewrites the XMP, as when it
        // geotags a photo. These are a Pixel's and a Samsung's motion photo as it leaves them.
        // Before, their lengths were read as 0, and a copy cut in the video was turned.
        let pixel = with_xmp(format!(
            "<rdf:Description rdf:about=''>\n <GCamera:MotionPhoto>1</GCamera:MotionPhoto>\n \
             <Container:Directory>\n  <rdf:Seq>\n   <rdf:li rdf:parseType='Resource'>\n    \
             <Container:Item rdf:parseType='Resource'>\n     \
             <Item:Mime>image/jpeg</Item:Mime>\n     <Item:Semantic>Primary</Item:Semantic>\n    \
             </Container:Item>\n   </rdf:li>\n   <rdf:li rdf:parseType='Resource'>\n    \
             <Container:Item rdf:parseType='Resource'>\n     \
             <Item:Length>{}</Item:Length>\n     <Item:Mime>video/mp4</Item:Mime>\n     \
             <!-- <Item:Length>0</Item:Length> -->\n     \
             <Item:Padding>0</Item:Padding>\n     \
             <Item:Semantic>MotionPhoto</Item:Semantic>\n    </Container:Item>\n   \
             </rdf:li>\n  </rdf:Seq>\n </Container:Directory>\n</rdf:Description>",
            video.len()
        ));
        let samsung = with_xmp(format!(
            "<rdf:Description rdf:about=''>\n <Container:Directory>\n  <rdf:Seq>\n   \
             <rdf:li rdf:parseType='Resource'>\n    \
             <Container:Item rdf:parseType='Resource'>\n     \
             <Item:Mime>image/jpeg</Item:Mime>\n     <Item:Padding>0</Item:Padding>\n     \
             <Item:Semantic>Primary</Item:Semantic>\n    </Container:Item>\n   </rdf:li>\n   \
             <rdf:li rdf:parseType='Resource'>\n    \
             <Container:Item rdf:parseType='Resource'>\n     \
             <Item:Length>{}</Item:Length>\n     <Item:Mime>video/mp4</Item:Mime>\n     \
             <Item:Padding>0</Item:Padding>\n     <Item:Semantic>MotionPhoto</Item:Semantic>\n    \
             </Container:Item>\n   </rdf:li>\n  </rdf:Seq>\n </Container:Directory>\n\
             <GCamera:MotionPhotoVersion>1</GCamera:MotionPhotoVersion>\n</rdf:Description>",
            video.len()
        ));
        // The fields of an item may also be in an rdf:Description of their own.
        let described = with_xmp(format!(
            "<rdf:Description><Container:Directory><rdf:Seq><rdf:li><Container:Item>\
             <rdf:Description Item:Semantic='Primary'/></Container:Item></rdf:li><rdf:li>\
             <Container:Item><rdf:Description Item:Semantic='MotionPhoto'>\
             <Item:Length>{}</Item:Length></rdf:Description></Container:Item></rdf:li>\
             </rdf:Seq></Container:Directory></rdf:Description>",
            video.len()
        ));
        let forms = [
            &mpf_file,
            &micro_video,
            &container,
            &pixel,
            &samsung,
            &described,
        ];
        for whole in forms {
            assert!(!super::looks_cut_short(whole, true));
            assert!(super::looks_cut_short(&whole[..whole.len() - 10], true));
        }
        // The lengths of another document inside a property, here a placed image's in
        // xmpMM:Pantry, are not the photo's. Before, they were read as its own, and the photo
        // was taken for one still being copied.
        let pantry = with_xmp(format!(
            "<rdf:Description><xmpMM:Pantry><rdf:Bag><rdf:li><rdf:Description \
             GCamera:MicroVideoOffset=\"99999\">{}</rdf:Description></rdf:li></rdf:Bag>\
             </xmpMM:Pantry></rdf:Description>",
            directory(99999)
        ));
        assert!(!super::looks_cut_short(&pantry, true));
        let dir = std::env::temp_dir().join(format!("glimt-turn-mpf-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let cut = &mpf_file[..mpf_file.len() - 10];
        std::fs::write(&file, cut).unwrap();
        assert!(crate::metadata::exif::write_orientation(&file, 1, false, true).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), cut);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG whose own lengths of what follows its image are wrong, as an editor may leave
    /// them, is turned once it has not changed for a minute, and it keeps what follows its
    /// image. A `GCamera:MicroVideoOffset` of 0, which means no video, is never counted. Before,
    /// both were refused for good as if they were still being copied.
    #[test]
    fn a_jpeg_with_stale_trailer_lengths_is_turned_once_it_rests() {
        use crate::images::shared_copy::jpeg_segment;
        let mut main = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut main),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let trailer = [&b"SEFH"[..], &[7; 100]].concat();
        let with_offset = |offset: usize| {
            let xmp = format!("<rdf:Description GCamera:MicroVideoOffset=\"{offset}\"/>");
            let body = [&b"http://ns.adobe.com/xap/1.0/\0"[..], xmp.as_bytes()].concat();
            [&main[..2], &jpeg_segment(0xe1, &body), &main[2..], &trailer].concat()
        };
        assert!(!super::looks_cut_short(&with_offset(0), true));
        // An offset left by an editor that dropped the video and added its own trailer.
        let stale = with_offset(5000);
        assert!(super::looks_cut_short(&stale, true));
        assert!(!super::looks_cut_short(&stale, false));
        let now = std::time::SystemTime::now();
        let since_1970 = |t: std::time::SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
        };
        assert!(super::changed_lately((since_1970(now), 0)));
        let hour = std::time::Duration::from_secs(3600);
        assert!(!super::changed_lately((since_1970(now - hour), 0)));
        let dir = std::env::temp_dir().join(format!("glimt-turn-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, &stale).unwrap();
        // Just written, it may still be coming in.
        assert!(crate::metadata::exif::write_orientation(&file, 1, false, true).is_err());
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(now - hour)
            .unwrap();
        crate::metadata::exif::write_orientation(&file, 1, false, true).unwrap();
        assert!(std::fs::read(&file).unwrap().ends_with(&trailer));
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
        // Each text is the rest of a Description's start tag and what it holds.
        let without_thumbnails = |x: &str| {
            let packet = format!("<rdf:Description{x}</rdf:Description>");
            let out = super::xmp_without_property(packet.as_bytes(), b"xmp:Thumbnails");
            let out = String::from_utf8(out).unwrap();
            let inside = out.strip_prefix("<rdf:Description").unwrap();
            inside
                .strip_suffix("</rdf:Description>")
                .unwrap()
                .to_owned()
        };
        let kept = "><!--<xmp:Thumbnails>--><a/>";
        assert_eq!(
            without_thumbnails(&format!("{kept}<xmp:Thumbnails>t</xmp:Thumbnails><b/>")),
            format!("{kept}<b/>")
        );
        // A property with a comment or CDATA section inside it goes whole, also when that
        // holds what looks like its end tag.
        for inside in [
            "<!-- -->",
            "<!-- </xmp:Thumbnails> -->",
            "<![CDATA[</xmp:Thumbnails>]]>",
        ] {
            let commented = format!("><xmp:Thumbnails>t{inside}</xmp:Thumbnails><b/>");
            assert_eq!(without_thumbnails(&commented), "><b/>");
        }
        assert_eq!(
            without_thumbnails(
                " xmp:Thumbnails='1' b=\"2\" xmp:Thumbnails = \"3\"><xmp:Thumbnails/>"
            ),
            " b=\"2\">"
        );
        let started = std::time::Instant::now();
        let many = " xmp:Thumbnails=\"1\"".repeat(100_000);
        assert_eq!(without_thumbnails(&format!("{many}>")), ">");
        let open = format!(">{}", "<xmp:Thumbnails>".repeat(100_000));
        assert_eq!(without_thumbnails(&open), open);
        let unclosed = format!(">{}", "<xmp:Thumbnails".repeat(100_000));
        assert_eq!(without_thumbnails(&unclosed), unclosed);
        let comments = format!(">{}", "<!-- -->".repeat(100_000));
        assert_eq!(without_thumbnails(&comments), comments);
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
        super::save_as(
            &a,
            &how(0, false),
            &b,
            crate::files::file_io::fingerprint(&b),
        )
        .unwrap();
        assert_eq!(text(&b).as_deref(), Some("a's"));
        std::fs::write(side(&b), "b's").unwrap();
        super::save_as(
            &a,
            &how(0, true),
            &b,
            crate::files::file_io::fingerprint(&b),
        )
        .unwrap();
        assert_eq!(text(&b), None);
        // A sidecar that cannot be removed (a folder in its place) leaves b as it was.
        std::fs::create_dir(side(&b)).unwrap();
        let before = std::fs::read(&b).unwrap();
        assert!(
            super::save_as(
                &a,
                &how(0, false),
                &b,
                crate::files::file_io::fingerprint(&b)
            )
            .is_err()
        );
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
        super::save_as(
            &a,
            &how(0, true),
            &linked,
            crate::files::file_io::fingerprint(&linked),
        )
        .unwrap();
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
        let page = super::save_as(
            &tif,
            &how(1, false),
            &dir.join("p.png"),
            crate::files::file_io::fingerprint(&dir.join("p.png")),
        )
        .unwrap();
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
        let lock = crate::metadata::xmp::lock_sidecar(&b);
        let saver = {
            let (a, b) = (a.clone(), b.clone());
            std::thread::spawn(move || super::save_copy_with_sidecar(&a, &b, b"x", None, false))
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
        drop(lock);
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

    /// On a file system that gives every file inode 0 (sshfs with `-o use_ino`), two files are
    /// still told apart, and a link to a file is still that file. Told apart by device and inode
    /// alone, Find Duplicates kept one file of the whole folder, and the tree walk one folder.
    #[test]
    fn files_without_unique_inodes_are_told_apart() {
        let dir = std::env::temp_dir().join(format!("glimt-test-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, link) = (dir.join("a.jpg"), dir.join("b.jpg"), dir.join("link.jpg"));
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        std::os::unix::fs::symlink("a.jpg", &link).unwrap();
        let file_id = |p: &std::path::Path| super::file_id_from_inode(p, (1, 0));
        assert_ne!(file_id(&a), file_id(&b));
        assert_eq!(file_id(&a), file_id(&link));
        assert_eq!(
            super::file_id_from_inode(&a, (1, 7)),
            super::file_id_from_inode(&b, (1, 7))
        );
        assert!(super::same_file(&a, &link) && !super::same_file(&a, &b));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Two files that a file system gives the same device and inode (NFSv3 with `nohide`,
    /// overlayfs without `xino`) are still two files when their size or time differs. Before,
    /// Rename Many's repair took such a file for a second name of the one it renamed and
    /// removed it.
    #[test]
    fn files_sharing_an_inode_by_mistake_are_told_apart() {
        let (one, other) = ((3, 1_700_000_000, 5), (4, 1_700_000_000, 5));
        assert!(super::same_named((Ok((1, 7)), one), (Ok((1, 7)), one)));
        assert!(!super::same_named((Ok((1, 7)), one), (Ok((1, 7)), other)));
        assert!(!super::same_named((Ok((1, 7)), one), (Ok((1, 8)), one)));
    }

    /// Without inodes, two names that differ only in case are one file where the folder lists
    /// only one of them (a server that ignores case), and two where it lists both.
    #[test]
    fn names_differing_in_case_are_one_file_where_case_is_ignored() {
        let dir = std::env::temp_dir().join(format!("glimt-test-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (lower, upper) = (dir.join("a.jpg"), dir.join("a.JPG"));
        std::fs::write(&lower, "x").unwrap();
        let stamp = (1, 1_700_000_000, 0);
        let same = || super::same_named((Err(upper.clone()), stamp), (Err(lower.clone()), stamp));
        assert!(same());
        assert!(!super::case_folded(&lower, &dir.join("b.jpg")));
        std::fs::write(&upper, "y").unwrap();
        assert!(!same());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A move in another window that comes during a turn's sidecar step waits until the image
    /// has its new name, so the turned image and its changed sidecar move together. Before, the
    /// sidecar's lock was let go after the step: the move took the changed sidecar along, and
    /// the image, found gone, was not turned.
    #[test]
    fn a_move_during_the_sidecar_step_waits_for_the_image() {
        use crate::metadata::xmp::{lock_sidecar, sidecar, update_locked_sidecar};
        let dir = std::env::temp_dir().join(format!("glimt-test-side-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, "old").unwrap();
        std::fs::write(sidecar(&a), "old side").unwrap();
        let (go, went) = std::sync::mpsc::channel::<()>();
        let (done, moved) = std::sync::mpsc::channel();
        let mover = {
            let (a, b) = (a.clone(), b.clone());
            // Another window moves the image and then its sidecar, under the sidecar's lock, as
            // Glimt's moves do.
            std::thread::spawn(move || {
                went.recv().unwrap();
                let _lock = lock_sidecar(&a);
                std::fs::rename(&a, &b).unwrap();
                std::fs::rename(sidecar(&a), sidecar(&b)).unwrap();
                done.send(()).unwrap();
            })
        };
        let step = |lock: &crate::metadata::xmp::SidecarLock| {
            update_locked_sidecar(lock, &a, true, |_| Some("new side".into()))?;
            go.send(()).unwrap();
            // A move that is not held back lands now, before the image is looked at again.
            let _ = moved.recv_timeout(std::time::Duration::from_millis(200));
            Ok(())
        };
        let read = crate::files::file_io::fingerprint(&a).unwrap();
        let replaced = super::replace_with_sidecar_locked(&a, &a, read, b"new".to_vec(), step);
        mover.join().unwrap();
        assert!(replaced.is_ok(), "{replaced:?}");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(sidecar(&b)).unwrap(), "new side");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
