//! EXIF: a minimal reader of its TIFF structure (from JPEG, PNG, WebP and TIFF) for the panel's
//! fields a photographer looks at, every field for the EXIF window, the dates that sorting by
//! EXIF date reads, and the orientation, which turning a JPEG writes.

use image::metadata::Orientation;
use std::path::Path;

/// The labels of the panel's rows, in the order `read` gives them. `--remote info` prints a
/// line for each, also for a file that lacks it, so a script always finds the same keys. A new
/// row goes at the end, as scripts read these keys.
pub const LABELS: [&str; 10] = [
    "Camera",
    "Lens",
    "Taken",
    "Exposure",
    "Aperture",
    "ISO",
    "Focal length",
    "Exposure bias",
    "Flash",
    "GPS",
];

/// The start of a JPEG's APP1 segment that holds a part of its extended XMP
/// (`jpeg_extended_xmp`).
pub const EXTENDED_XMP: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";

/// The longest extended XMP that `jpeg_extended_xmp` joins, 16 MiB. The longest that cameras
/// write are Google Camera's Lens Blur photos, whose GImage:Data holds the whole original JPEG
/// in base64, a few MB for a 12 MP photo. Dynamic Depth's Device element, which is what the
/// extended XMP is read for, is more than 64 kB in most files and often some MB, as the Dynamic
/// Depth 1.0 specification says. Without a cap, a crafted packet could be as long as the file,
/// up to 512 MiB, and the memory and the reading of it would grow with it.
pub const MAX_EXTENDED_XMP: usize = 16 << 20;

/// TIFF tags used in more than one place (the panel's own are named in `tag_name`).
const ORIENTATION: u16 = 0x0112;

const EXIF_IFD: u16 = 0x8769;

const GPS_IFD: u16 = 0x8825;

const INTEROP_IFD: u16 = 0xa005;

/// TIFF value types.
const BYTE: u16 = 1;

const ASCII: u16 = 2;

const SHORT: u16 = 3;

const LONG: u16 = 4;

const RATIONAL: u16 = 5;

const UNDEFINED: u16 = 7;

const SLONG: u16 = 9;

const SRATIONAL: u16 = 10;

/// A directory's offset, as libtiff writes the Exif and GPS pointers.
const IFD: u16 = 13;

/// The fields of IFD0 that describe the stored pixels of the file the block came from: their
/// size, layout, strips, tiles, thumbnail and the SubIFDs of a RAW. A copy has pixels of its
/// own, so its block drops them, and the DNG fields (`DNG_FIELDS`) with them.
const PIXEL_FIELDS: [u16; 23] = [
    0x00fe, 0x00ff, 0x0100, 0x0101, 0x0102, 0x0103, 0x0106, 0x0111, 0x0115, 0x0116, 0x0117, 0x0118,
    0x0119, 0x011c, 0x0142, 0x0143, 0x0144, 0x0145, 0x014a, 0x0152, 0x0153, 0x0201, 0x0202,
];

/// The tags of the DNG specification, which describe a RAW's pixels and how to develop them.
const DNG_FIELDS: std::ops::RangeInclusive<u16> = 0xc612..=0xcd3f;

/// The longest text read from one field. A description or a comment is far shorter. Without a
/// cap, a crafted block of 1,500 fields all naming the same megabyte would take gigabytes.
const MAX_TEXT: usize = 4096;

/// The name Copy for Sharing gives the EXIF thumbnail (IFD1), a small JPEG of the whole photo.
pub const EXIF_THUMBNAIL: &str = "EXIF thumbnail";

/// The tag of the maker note, the camera maker's own block in the Exif directory.
const MAKER_NOTE: u16 = 0x927c;

/// The start of an iPhone's maker note: the maker, a version and the byte order (big-endian).
/// Its directory follows, and its offsets count from the start of the maker note.
const APPLE_MAKER_NOTE_HEADER: &[u8] = b"Apple iOS\0\0\x01MM";

/// The tags of an iPhone's maker note that say how bright its HDR gain map may make the photo:
/// HDRHeadroom (33) and HDRGain (48), each a signed rational (exiftool's Apple.pm).
const APPLE_HDR_TAGS: [u16; 2] = [33, 48];

/// EXIF orientations 1–8 as 2×2 matrices on (x, y) with y down: what to do with the stored
/// pixels to show them upright.
const ORIENTATIONS: [[i8; 4]; 8] = [
    [1, 0, 0, 1],
    [-1, 0, 0, 1],
    [-1, 0, 0, -1],
    [1, 0, 0, -1],
    [0, 1, 1, 0],
    [0, -1, 1, 0],
    [0, -1, -1, 0],
    [0, 1, -1, 0],
];

/// The keywords of a PNG's text chunks (tEXt, zTXt or iTXt) that hold its metadata, and what
/// each holds. An eXIf chunk holds EXIF too. This is the one list of them: `png_metadata` reads
/// each, wherever it stands, and a file written with new pixels drops one only when it carries
/// what the chunk held (`save::png_with_chunks`), so no chunk is dropped that was not read back.
/// The XMP chunk comes before ImageMagick's copy of it, so that it is read first.
pub(crate) const PNG_METADATA_TEXT: [(&[u8], PngMetadataKind); 5] = [
    (b"Raw profile type exif", PngMetadataKind::Exif),
    (b"Raw profile type APP1", PngMetadataKind::Exif),
    (b"XML:com.adobe.xmp", PngMetadataKind::Xmp),
    (b"Raw profile type xmp", PngMetadataKind::Xmp),
    (
        b"Raw profile type 8bim",
        PngMetadataKind::PhotoshopResources,
    ),
];

/// A directory being written into a new block (`shared_block`). It holds its entries as (tag,
/// type, count, value), each value as its bytes in the block's byte order.
type NewIfd = Vec<(u16, u16, u32, Vec<u8>)>;

/// A field of the EXIF window: (directory, name, value).
pub type Field = (&'static str, String, String);

/// JPEG segments: (marker, start of its data, length of its data).
type Segments = Vec<(u8, usize, usize)>;

type Ifd = Vec<(u16, Entry)>;

struct Tiff<'a> {
    data: &'a [u8],
    is_little_endian: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.is_little_endian {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.is_little_endian {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// The entries of the IFD at `offset`, by tag.
    fn ifd(&self, offset: usize) -> Vec<(u16, Entry)> {
        let n = self.u16(offset).unwrap_or(0) as usize;
        (0..n.min(500))
            .filter_map(|i| {
                let e = offset + 2 + i * 12;
                let (tag, kind, count) = (self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?);
                let size = type_size(kind);
                // Values of up to four bytes are stored in the entry itself.
                let at = if size * count as usize <= 4 {
                    e + 8
                } else {
                    self.u32(e + 8)? as usize
                };
                Some((
                    tag,
                    Entry {
                        field_type: kind,
                        count,
                        value_offset: at,
                    },
                ))
            })
            .collect()
    }

    fn text(&self, e: Entry) -> Option<String> {
        let bytes = self
            .data
            .get(e.value_offset..e.value_offset + (e.count as usize).min(MAX_TEXT))?;
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// Any field as text: strings as they are, numbers and fractions listed, other data as
    /// text if it reads as such, else its length.
    fn value(&self, e: Entry) -> String {
        let n = (e.count as usize).min(16);
        let list = |items: Vec<String>| {
            let more = if e.count as usize > n { " …" } else { "" };
            items.join(" ") + more
        };
        match e.field_type {
            ASCII => self.text(e).unwrap_or_default(),
            BYTE | UNDEFINED => {
                let bytes = self
                    .data
                    .get(e.value_offset..e.value_offset + (e.count as usize).min(256))
                    .unwrap_or(&[]);
                let text = String::from_utf8_lossy(bytes);
                let text = text.trim_end_matches('\0').trim();
                if !text.is_empty() && text.chars().all(|c| !c.is_control()) {
                    text.to_owned()
                } else if e.count <= 8 {
                    list(bytes.iter().map(|b| b.to_string()).collect())
                } else {
                    format!("({} bytes)", e.count)
                }
            }
            SHORT => list(
                (0..n)
                    .filter_map(|i| self.u16(e.value_offset + 2 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            LONG | SLONG => list(
                (0..n)
                    .filter_map(|i| self.u32(e.value_offset + 4 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            RATIONAL | SRATIONAL => list(
                (0..n)
                    .filter_map(|i| self.rational(e, i))
                    .map(|v| format!("{}", (v * 10000.0).round() / 10000.0))
                    .collect(),
            ),
            _ => format!("({} values)", e.count),
        }
    }

    fn int(&self, e: Entry) -> Option<u32> {
        // No value at all: the bytes of its value field are not a number.
        if e.count == 0 {
            return None;
        }
        match e.field_type {
            SHORT => self.u16(e.value_offset).map(u32::from),
            LONG | SLONG | IFD => self.u32(e.value_offset),
            _ => None,
        }
    }

    /// Rational number `i` of the entry (signed for SRATIONAL).
    fn rational(&self, e: Entry, i: usize) -> Option<f64> {
        // Only the values the entry has: a GPS position of one value read its minutes and
        // seconds from whatever followed.
        if i >= e.count as usize {
            return None;
        }
        let at = e.value_offset + i * 8;
        let (n, d) = (self.u32(at)?, self.u32(at + 4)?);
        let (n, d) = if e.field_type == SRATIONAL {
            (n as i32 as f64, d as i32 as f64)
        } else {
            (n as f64, d as f64)
        };
        (d != 0.0).then(|| n / d)
    }
}

/// A tag's type, count and where its value starts.
#[derive(Clone, Copy)]
struct Entry {
    field_type: u16,
    count: u32,
    value_offset: usize,
}

/// What a PNG's metadata chunk holds (`PNG_METADATA_TEXT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PngMetadataKind {
    Exif,
    Xmp,
    /// Photoshop's image resources, with the IPTC caption, copyright and keywords.
    PhotoshopResources,
}

/// The EXIF block, XMP packet and Photoshop resources of a PNG, as `png_metadata` read them.
#[derive(Default, Debug)]
pub(crate) struct PngMetadata {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub photoshop_resources: Option<Vec<u8>>,
}

impl PngMetadata {
    /// Returns the place for what a chunk of the kind `kind` holds.
    fn slot(&mut self, kind: PngMetadataKind) -> &mut Option<Vec<u8>> {
        match kind {
            PngMetadataKind::Exif => &mut self.exif,
            PngMetadataKind::Xmp => &mut self.xmp,
            PngMetadataKind::PhotoshopResources => &mut self.photoshop_resources,
        }
    }
}

/// The entry for `tag` in `ifd`.
/// An exposure time as cameras show it: a fraction up to 1/4 s, then seconds with a decimal
/// (0.4 s, not "1/3 s").
fn exposure(s: f64) -> String {
    match s {
        s if s > 0.0 && s <= 0.25 => format!("1/{} s", (1.0 / s).round()),
        s if s < 1.0 => format!("{s:.1} s"),
        s => format!("{s} s"),
    }
}

fn find(ifd: &[(u16, Entry)], tag: u16) -> Option<Entry> {
    ifd.iter().find(|e| e.0 == tag).map(|e| e.1)
}

/// (label, value) rows for the panel, in display order; empty if the file has no EXIF.
pub fn read(path: &Path) -> Vec<(&'static str, String)> {
    read_exif_block(path).map_or_else(Vec::new, |raw| parse(&raw))
}

/// `read` of `path`, and every field of IFD0, the Exif and the GPS directory for the EXIF
/// window (unknown tags by number), from one reading of its EXIF block: on a network share
/// each reading of a large photo costs.
pub fn read_and_all(path: &Path) -> (Vec<(&'static str, String)>, Vec<Field>) {
    read_exif_block(path).map_or_else(Default::default, |raw| (parse(&raw), fields(&raw)))
}

/// `read` of `path`, and its EXIF orientation (1–8 as stored), from one reading of its EXIF
/// block. The orientation is `None` when the file has none.
pub fn read_with_orientation(path: &Path) -> (Vec<(&'static str, String)>, Option<u32>) {
    let Some(raw) = read_exif_block(path) else {
        return Default::default();
    };
    let orientation = ifds(&raw).and_then(|(t, ifd0, _, _)| t.int(find(&ifd0, ORIENTATION)?));
    (parse(&raw), orientation)
}

/// Every field of an EXIF block, for the EXIF window.
fn fields(raw: &[u8]) -> Vec<Field> {
    let Some((t, ifd0, exif, gps)) = ifds(raw) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (group, ifd) in [("Image", &ifd0), ("Exif", &exif), ("GPS", &gps)] {
        for &(tag, e) in ifd.iter() {
            // The links to the other directories are not fields of their own.
            if group != "GPS" && matches!(tag, EXIF_IFD | GPS_IFD | INTEROP_IFD) {
                continue;
            }
            let name = tag_name(group == "GPS", tag).map_or(format!("0x{tag:04x}"), str::to_owned);
            rows.push((group, name, t.value(e)));
        }
    }
    rows
}

fn tag_name(gps: bool, tag: u16) -> Option<&'static str> {
    if gps {
        return Some(match tag {
            0x00 => "GPSVersionID",
            0x01 => "GPSLatitudeRef",
            0x02 => "GPSLatitude",
            0x03 => "GPSLongitudeRef",
            0x04 => "GPSLongitude",
            0x05 => "GPSAltitudeRef",
            0x06 => "GPSAltitude",
            0x07 => "GPSTimeStamp",
            0x08 => "GPSSatellites",
            0x0c => "GPSSpeedRef",
            0x0d => "GPSSpeed",
            0x10 => "GPSImgDirectionRef",
            0x11 => "GPSImgDirection",
            0x12 => "GPSMapDatum",
            0x1b => "GPSProcessingMethod",
            0x1d => "GPSDateStamp",
            _ => return None,
        });
    }
    Some(match tag {
        0x000b => "ProcessingSoftware",
        0x010e => "ImageDescription",
        0x010f => "Make",
        0x0110 => "Model",
        0x0112 => "Orientation",
        0x011a => "XResolution",
        0x011b => "YResolution",
        0x0128 => "ResolutionUnit",
        0x0131 => "Software",
        0x0132 => "DateTime",
        0x013b => "Artist",
        0x013c => "HostComputer",
        0x0213 => "YCbCrPositioning",
        0x02bc => "XMLPacket",
        0x8298 => "Copyright",
        0x829a => "ExposureTime",
        0x829d => "FNumber",
        0x83bb => "IPTC-NAA",
        0x8822 => "ExposureProgram",
        0x8827 => "ISO",
        0x8830 => "SensitivityType",
        0x9000 => "ExifVersion",
        0x9003 => "DateTimeOriginal",
        0x9004 => "DateTimeDigitized",
        0x9010 => "OffsetTime",
        0x9011 => "OffsetTimeOriginal",
        0x9012 => "OffsetTimeDigitized",
        0x9101 => "ComponentsConfiguration",
        0x9201 => "ShutterSpeedValue",
        0x9202 => "ApertureValue",
        0x9203 => "BrightnessValue",
        0x9204 => "ExposureBiasValue",
        0x9205 => "MaxApertureValue",
        0x9206 => "SubjectDistance",
        0x9207 => "MeteringMode",
        0x9208 => "LightSource",
        0x9209 => "Flash",
        0x920a => "FocalLength",
        0x927c => "MakerNote",
        0x9286 => "UserComment",
        0x9290 => "SubSecTime",
        0x9291 => "SubSecTimeOriginal",
        0x9292 => "SubSecTimeDigitized",
        0x9c9b => "XPTitle",
        0x9c9c => "XPComment",
        0x9c9d => "XPAuthor",
        0x9c9e => "XPKeywords",
        0x9c9f => "XPSubject",
        0xa000 => "FlashpixVersion",
        0xa001 => "ColorSpace",
        0xa002 => "PixelXDimension",
        0xa003 => "PixelYDimension",
        0xa20e => "FocalPlaneXResolution",
        0xa20f => "FocalPlaneYResolution",
        0xa210 => "FocalPlaneResolutionUnit",
        0xa217 => "SensingMethod",
        0xa300 => "FileSource",
        0xa301 => "SceneType",
        0xa401 => "CustomRendered",
        0xa402 => "ExposureMode",
        0xa403 => "WhiteBalance",
        0xa404 => "DigitalZoomRatio",
        0xa405 => "FocalLengthIn35mmFormat",
        0xa406 => "SceneCaptureType",
        0xa407 => "GainControl",
        0xa408 => "Contrast",
        0xa409 => "Saturation",
        0xa40a => "Sharpness",
        0xa40c => "SubjectDistanceRange",
        0xa420 => "ImageUniqueID",
        0xa430 => "CameraOwnerName",
        0xa431 => "BodySerialNumber",
        0xa432 => "LensSpecification",
        0xa433 => "LensMake",
        0xa434 => "LensModel",
        0xa435 => "LensSerialNumber",
        0xa436 => "ImageTitle",
        0xa437 => "Photographer",
        0xa438 => "ImageEditor",
        0xa439 => "CameraFirmware",
        0xa43a => "RAWDevelopingSoftware",
        0xa43b => "ImageEditingSoftware",
        0xa43c => "MetadataEditingSoftware",
        _ => return None,
    })
}

/// DateTimeOriginal (or DateTimeDigitized) as "YYYY:MM:DD HH:MM:SS", which sorts as text. Some
/// programs write the day with dashes ("YYYY-MM-DD"), which is made the same form, so the
/// date sort and Pan view put those photos among the others of their day. Before, they came
/// after or before every other date, and Pan view showed their day twice.
pub fn date(path: &Path, digitized: bool) -> Option<String> {
    // A decoder panicking on one file must not end the sort or the Pan view that asked.
    crate::support::threads::catch_panic(|| {
        let raw = read_exif_block(path)?;
        let (t, _, exif, _) = ifds(&raw)?;
        let tag = if digitized { 0x9004 } else { 0x9003 };
        let mut date = t.text(exif.iter().find(|(t2, _)| *t2 == tag)?.1)?;
        if let Some(day) = date.get(..10) {
            let day = day.replace('-', ":");
            date.replace_range(..10, &day);
        }
        Some(date)
    })
    .flatten()
}

/// Whether the EXIF block says the pixels are Adobe RGB without an ICC profile, as cameras set
/// to Adobe RGB do: ColorSpace 2, or "uncalibrated" with the Interoperability index R03 (R98 is
/// sRGB). Read as Geeqie reads it.
pub fn is_adobe_rgb(raw: &[u8]) -> bool {
    let Some((t, _, exif, _)) = ifds(raw) else {
        return false;
    };
    match find(&exif, 0xa001).and_then(|e| t.int(e)) {
        Some(2) => true,
        Some(0xffff) => {
            let interop = find(&exif, INTEROP_IFD).and_then(|e| t.int(e));
            let index = interop.and_then(|at| find(&t.ifd(at as usize), 0x0001));
            index.and_then(|e| t.text(e)).is_some_and(|i| i == "R03")
        }
        _ => false,
    }
}

/// The Orientation tag of a TIFF block (a TIFF-based RAW file's header).
pub fn orientation(raw: &[u8]) -> Option<image::metadata::Orientation> {
    let (t, ifd0, _, _) = ifds(raw)?;
    let e = find(&ifd0, ORIENTATION)?;
    image::metadata::Orientation::from_exif(t.int(e)? as u8)
}

fn compose_orientations(a: [i8; 4], b: [i8; 4]) -> [i8; 4] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
    ]
}

/// The orientation after the view's `turns` quarter turns clockwise, mirrored first if
/// `mirrored`, on top of orientation `o`.
pub fn turned(o: u8, turns: u8, mirrored: bool) -> u8 {
    let mut m = ORIENTATIONS[(o.clamp(1, 8) - 1) as usize];
    if mirrored {
        m = compose_orientations([-1, 0, 0, 1], m);
    }
    for _ in 0..turns % 4 {
        m = compose_orientations([0, -1, 1, 0], m);
    }
    ORIENTATIONS
        .iter()
        .position(|&x| x == m)
        .map_or(1, |i| i as u8 + 1)
}

/// Save the view's rotation in a JPEG as its EXIF orientation: the two bytes of the field if
/// there is one, or a small EXIF block if the file has none. The pixels are not touched, and the
/// file is replaced in one step (a new file renamed over it), keeping its permissions. The
/// view's turns are on what it showed: the file's orientation when Auto Rotate was on
/// (`rotate`), else the pixels as stored. A sidecar that holds the orientation is set to it too.
pub fn write_orientation(
    path: &Path,
    turns: u8,
    mirrored: bool,
    rotate: bool,
) -> Result<(), String> {
    // Through a symlink, the file it points to: the new file is renamed over that, not the link.
    let real = &std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    // Taken before the read, so a JPEG still being copied in is not written back cut off.
    let read =
        crate::files::file_io::fingerprint_before_reading(real).map_err(|e| e.to_string())?;
    let data = crate::images::decode::read_limited(real).map_err(|e| e.to_string())?;
    let (data, new) = oriented(data, turns, mirrored, rotate)?;
    let how = crate::images::save::Derived::Oriented(new);
    crate::images::save::replace_with_sidecar(real, path, read, data, how)
}

/// The JPEG `data` with the view's rotation as its EXIF orientation, as `write_orientation`
/// writes it. Returns the new data and the orientation it has.
fn oriented(
    mut data: Vec<u8>,
    turns: u8,
    mirrored: bool,
    rotate: bool,
) -> Result<(Vec<u8>, u8), String> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err(crate::support::lang::tr("only JPEG files can store the orientation").into());
    }
    let (segments, _) = jpeg_segments(&data).ok_or("cut off")?;
    let exif = segments
        .iter()
        .find(|s| s.0 == 0xe1 && segment(&data, s).starts_with(b"Exif\0\0"));
    let new;
    match exif {
        Some(&(_, start, len)) => {
            let block = data.get(start..start + len).ok_or("cut off")?;
            let (t, ifd0, _, _) = ifds(block).ok_or("unreadable EXIF")?;
            let little = t.is_little_endian;
            let found = find(&ifd0, ORIENTATION);
            match found {
                // One value, so it sits in the entry itself, inside the block.
                Some(e) if e.field_type == SHORT && e.count == 1 => {
                    // Read first, which keeps the write inside the block: an entry at its very end
                    // has no room for its value.
                    let old = t.u16(e.value_offset).ok_or("cut off")?;
                    // A value outside 1 to 8 is shown unturned, as the view and the panel read
                    // it, so the turn starts from 1 and the file shows what the view showed.
                    let shown = match u8::try_from(old) {
                        Ok(o @ 1..=8) if rotate => o,
                        _ => 1,
                    };
                    new = turned(shown, turns, mirrored) as u16;
                    // Offsets count from the TIFF header, which ends the block as `t.data` does.
                    let at = start + len - t.data.len() + e.value_offset;
                    let bytes = if little {
                        new.to_le_bytes()
                    } else {
                        new.to_be_bytes()
                    };
                    data.get_mut(at..at + 2)
                        .ok_or("cut off")?
                        .copy_from_slice(&bytes);
                }
                Some(_) => return Err("unusual orientation field".into()),
                None => {
                    new = turned(1, turns, mirrored) as u16;
                    add_orientation(&mut data, start, len, little, new)?;
                }
            }
        }
        None => {
            new = turned(1, turns, mirrored) as u16;
            let mut app1 = vec![0xff, 0xe1, 0, 34];
            app1.extend(b"Exif\0\0MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
            app1.extend(new.to_be_bytes());
            app1.extend([0, 0, 0, 0, 0, 0]);
            // After a JFIF segment if there is one, else right after the start marker.
            let at = match segments.first() {
                Some(&(0xe0, start, len)) => start + len,
                _ => 2,
            };
            // A JFIF segment longer than the file: cut off.
            if at > data.len() {
                return Err("cut off".into());
            }
            data.splice(at..at, app1);
        }
    }
    // The XMP's the same, as Lightroom and digiKam write one there too: else programs that read
    // it first would turn the photo the old way.
    if let Some((segments, _)) = jpeg_segments(&data)
        && let Some(&(_, start, len)) = (segments.iter()).find(|s| {
            s.0 == 0xe1 && segment(&data, s).starts_with(b"http://ns.adobe.com/xap/1.0/\0")
        })
    {
        let end = (start + len).min(data.len());
        let how = crate::images::save::Derived::Oriented(new as u8);
        let xmp = crate::images::save::derived_xmp(&data[start..end], how);
        // Only the orientation's one digit changes, so the segment keeps its length.
        data[start..end].copy_from_slice(&xmp);
    }
    Ok((data, new as u8))
}

/// Sets the XMP's `tiff:Orientation` to `value` (1–8), as an attribute or an element. The
/// length stays, as a value is one digit. Only the photo's own orientation changes, a top-level
/// property (`xmp::properties_named`), and not that of another document in xmpMM:Pantry. It is
/// found by its namespace, whatever prefix the packet binds to it.
pub fn set_xmp_orientation(xmp: &mut [u8], value: u8) {
    for found in crate::metadata::xmp::properties_named(xmp, "tiff:Orientation") {
        let text = &xmp[found.value.clone()];
        let blanks = text.iter().take_while(|c| c.is_ascii_whitespace()).count();
        if let [digit] = text.trim_ascii()
            && digit.is_ascii_digit()
        {
            xmp[found.value.start + blanks] = b'0' + value.min(9);
        }
    }
}

/// Set the orientation in an EXIF block, as PNG and WebP keep it, to 1: for pixels that were
/// turned upright. A block without the field is left as it is, and so is one without a TIFF
/// header: no program finds an orientation in it to turn the pixels again. Also clears the EXIF
/// thumbnail (IFD1), which a copy must not keep: of a crop it shows the whole frame, so every
/// copy goes through here, upright or not. The pixel sizes the block has are set to `size`,
/// the copy's own. This is what `save::Derived::Turned` and `Copy` keep of EXIF.
pub fn set_upright(block: &mut [u8], size: (u32, u32)) -> Result<(), String> {
    let Some((t, ifd0, exif, _)) = ifds(block) else {
        return Ok(());
    };
    // A size that does not fit the field's type is left as it was.
    let sizes: Vec<(usize, Vec<u8>)> = [(0xa002, size.0), (0xa003, size.1)]
        .into_iter()
        .filter_map(|(tag, value)| {
            let e = find(&exif, tag).filter(|e| e.count == 1)?;
            let bytes = match (e.field_type, t.is_little_endian) {
                (SHORT, true) => u16::try_from(value).ok()?.to_le_bytes().to_vec(),
                (SHORT, false) => u16::try_from(value).ok()?.to_be_bytes().to_vec(),
                (LONG, true) => value.to_le_bytes().to_vec(),
                (LONG, false) => value.to_be_bytes().to_vec(),
                _ => return None,
            };
            Some((e.value_offset, bytes))
        })
        .collect();
    let found = find(&ifd0, ORIENTATION);
    if found.is_some_and(|e| e.field_type != SHORT) {
        return Err("unusual orientation field".into());
    }
    // Offsets count from the TIFF header, after any "Exif\0\0" before it.
    let head = block.len() - t.data.len();
    // The EXIF thumbnail (IFD1) would show the old way up, and of a crop the whole frame: left
    // out, as IFD0 is made the last, and its bytes cleared, as a tool that looks for a JPEG in
    // the block would still find it.
    let next = t
        .u32(4)
        .and_then(|o| Some(o as usize + 2 + 12 * t.u16(o as usize)? as usize));
    let thumbnail = next.and_then(|n| t.u32(n)).filter(|&o| o != 0).map(|o| {
        let ifd1 = t.ifd(o as usize);
        // A JPEG thumbnail (offset and length), or an uncompressed one's strips, each of them.
        let nth = |e: Entry, i: usize| {
            let size = if e.field_type == SHORT { 2 } else { 4 };
            t.int(Entry {
                value_offset: e.value_offset + i * size,
                ..e
            })
        };
        let parts = |at: u16, len: u16| {
            let (Some(at), Some(len)) = (find(&ifd1, at), find(&ifd1, len)) else {
                return Vec::new();
            };
            // A count from the file: no more strips than a thumbnail has room for.
            (0..(at.count.min(len.count) as usize).min(4096))
                .filter_map(|i| Some((nth(at, i)?, nth(len, i)?)))
                .collect()
        };
        [parts(0x0201, 0x0202), parts(0x0111, 0x0117)].concat()
    });
    let little = t.is_little_endian;
    for (at, len) in thumbnail.into_iter().flatten() {
        let start = head + at as usize;
        if let Some(bytes) = block.get_mut(start..start.saturating_add(len as usize)) {
            bytes.fill(0);
        }
    }
    if let Some(n) = next.and_then(|n| block.get_mut(head + n..head + n + 4)) {
        n.fill(0);
    }
    for (at, bytes) in sizes {
        if let Some(field) = block.get_mut(head + at..head + at + bytes.len()) {
            field.copy_from_slice(&bytes);
        }
    }
    let Some(e) = found else {
        return Ok(());
    };
    let at = head + e.value_offset;
    let one = if little {
        1u16.to_le_bytes()
    } else {
        1u16.to_be_bytes()
    };
    block
        .get_mut(at..at + 2)
        .ok_or("cut off")?
        .copy_from_slice(&one);
    Ok(())
}

/// Returns the segments of a JPEG before its image data, and whether the image data was
/// reached. Returns `None` when a segment's length is cut off. Bytes between segments that are
/// no marker, which some phones and editors write, are skipped to the next 0xff, as decoders
/// and Copy for Sharing's walk (`shared_copy::walk_jpeg_segments`) skip them.
fn jpeg_segments(data: &[u8]) -> Option<(Segments, bool)> {
    let mut segments = Vec::new();
    let mut i = 2;
    while let (Some(&byte), Some(&marker)) = (data.get(i), data.get(i + 1)) {
        if byte != 0xff || marker == 0 {
            match data[i + 1..].iter().position(|&b| b == 0xff) {
                Some(next) => i += 1 + next,
                None => break,
            }
            continue;
        }
        // Fill bytes: a marker may follow any number of 0xff.
        if marker == 0xff {
            i += 1;
            continue;
        }
        if marker == 0xda {
            return Some((segments, true));
        }
        // A marker without a length, outside a scan where it does not belong.
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
        segments.push((marker, i + 4, len.saturating_sub(2)));
        i += 2 + len;
    }
    Some((segments, false))
}

/// Where a JPEG's image data begins (its first SOS marker), past the segments before it and what
/// they hold, such as the EXIF thumbnail, which is a JPEG of its own. Returns `None` if not
/// reached.
pub fn jpeg_scan_start(data: &[u8]) -> Option<usize> {
    let (segments, reached) = jpeg_segments(data)?;
    reached.then(|| segments.last().map_or(2, |&(_, start, len)| start + len))
}

/// Returns the contents of the first segment of the JPEG `data`, before its image data, that
/// has `marker` and starts with `prefix`, with where they start in `data`. Returns `None` when
/// it has none.
pub fn jpeg_segment_with<'a>(
    data: &'a [u8],
    marker: u8,
    prefix: &[u8],
) -> Option<(usize, &'a [u8])> {
    let (segments, _) = jpeg_segments(data)?;
    let found =
        (segments.iter()).find(|s| s.0 == marker && segment(data, s).starts_with(prefix))?;
    Some((found.1, segment(data, found)))
}

/// Returns the extended XMP of the JPEG `data` whose GUID is `guid`, joined from its parts.
/// The extended XMP is the rest of a packet too long for one segment, and the main packet names
/// its GUID in xmpNote:HasExtendedXMP. The XMP spec (part 3) puts each part in an APP1 segment
/// of its own, after `EXTENDED_XMP`, the GUID as 32 hex digits, and the whole packet's length
/// and the part's offset in it (4 bytes each, big-endian). The parts may come in any order.
/// Returns `None` when a part of the GUID is missing or too short, or the parts do not agree on
/// the length, or the packet is longer than `MAX_EXTENDED_XMP`.
pub fn jpeg_extended_xmp(data: &[u8], guid: &[u8]) -> Option<Vec<u8>> {
    let (segments, _) = jpeg_segments(data)?;
    let mut packet: Option<Vec<u8>> = None;
    let mut filled = Vec::new();
    for found in segments.iter().filter(|s| s.0 == 0xe1) {
        let Some(rest) = segment(data, found).strip_prefix(EXTENDED_XMP) else {
            continue;
        };
        if rest.get(..32) != Some(guid) {
            continue;
        }
        let head = rest.get(32..40)?;
        let whole = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let offset = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
        if whole > data.len() || whole > MAX_EXTENDED_XMP {
            return None;
        }
        let packet = packet.get_or_insert_with(|| vec![0; whole]);
        let part = &rest[40..];
        let end = offset.checked_add(part.len())?;
        if packet.len() != whole || end > whole {
            return None;
        }
        packet[offset..end].copy_from_slice(part);
        filled.push(offset..end);
    }
    // The parts must cover the packet with no gap between them.
    filled.sort_by_key(|part| part.start);
    let mut covered = 0;
    for part in filled {
        if part.start > covered {
            return None;
        }
        covered = covered.max(part.end);
    }
    packet.filter(|packet| covered == packet.len())
}

/// The bytes of a segment from `jpeg_segments`, as far as `data` has them: a prefix is looked
/// for in the segment only, not in what follows a short one.
fn segment<'a>(data: &'a [u8], &(_, start, len): &(u8, usize, usize)) -> &'a [u8] {
    data.get(start..).map_or(&[], |d| &d[..len.min(d.len())])
}

/// The APP1 segment of a JPEG that starts with `prefix` (EXIF or XMP), from its first 256 KB,
/// where it is: the decoder would read the whole file, and sorting by EXIF date reads every file
/// of the folder. `Some(None)` if it has none; `None` if it is no JPEG or that part is not enough
/// to tell, so the decoder reads the whole file after all.
pub fn jpeg_app1(path: &Path, prefix: &[u8]) -> Option<Option<Vec<u8>>> {
    let head = crate::images::decode::read_head(path, 1 << 18).ok()?;
    if !head.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let (segments, done) = jpeg_segments(&head)?;
    let found = segments
        .iter()
        .find(|s| s.0 == 0xe1 && segment(&head, s).starts_with(prefix));
    match found {
        Some(&(_, start, len)) => Some(Some(head.get(start..start + len)?.to_vec())),
        None => done.then_some(None),
    }
}

/// Add an orientation field to an EXIF block that has none: a copy of IFD0 with the field is
/// put at the end of the block and the header pointed at it. Everything else stays where it
/// is, so no other offset changes.
fn add_orientation(
    data: &mut Vec<u8>,
    start: usize,
    len: usize,
    little: bool,
    value: u16,
) -> Result<(), String> {
    let tiff = start + 6;
    let p16 = |v: u16| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let p32 = |v: u32| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    // Read inside the EXIF block only: an offset past it would copy the picture as fields.
    let block = data.get(..start + len).ok_or("cut off")?;
    let read = Tiff {
        data: block,
        is_little_endian: little,
    };
    let ifd = tiff + read.u32(tiff + 4).ok_or("cut off")? as usize;
    let n = read.u16(ifd).ok_or("cut off")? as usize;
    let entries = block.get(ifd + 2..ifd + 2 + 12 * n).ok_or("cut off")?;
    let mut list: Vec<Vec<u8>> = entries.chunks(12).map(<[u8]>::to_vec).collect();
    let next = block
        .get(ifd + 2 + 12 * n..ifd + 6 + 12 * n)
        .ok_or("cut off")?
        .to_vec();
    let mut entry = Vec::new();
    entry.extend(p16(ORIENTATION));
    entry.extend(p16(SHORT));
    entry.extend(p32(1));
    entry.extend(p16(value));
    entry.extend([0, 0]);
    list.push(entry);
    // Fields are sorted by tag.
    list.sort_by_key(|e| {
        Tiff {
            data: e,
            is_little_endian: little,
        }
        .u16(0)
        .unwrap_or(0)
    });
    let tiff_len = len - 6;
    let mut added = vec![0; tiff_len % 2]; // word aligned
    let new_ifd = (tiff_len + added.len()) as u32;
    added.extend(p16(list.len() as u16));
    added.extend(list.concat());
    added.extend(next);
    let seg_len = len + 2 + added.len();
    if seg_len > 0xffff {
        return Err("EXIF block too large".into());
    }
    data.splice(start + len..start + len, added);
    data[start - 2..start].copy_from_slice(&(seg_len as u16).to_be_bytes());
    data[tiff + 4..tiff + 8].copy_from_slice(&p32(new_ifd));
    Ok(())
}

fn read_exif_block(path: &Path) -> Option<Vec<u8>> {
    if crate::model::listing::has_ext(path, crate::images::raw::EXTS) {
        return crate::images::raw::exif(path);
    }
    #[cfg(feature = "jxl")]
    if crate::model::listing::has_ext(path, crate::model::listing::JXL_EXTS) {
        let file = std::io::BufReader::new(crate::images::decode::open_regular(path).ok()?);
        let mut dec = jxl_oxide::integration::JxlDecoder::new(file).ok()?;
        return block_from_decoder(path, &mut dec);
    }
    // HEIC and AVIF keep the EXIF block as an item of its own, "Exif\0\0" and a TIFF header.
    // From the first MB, where it usually is, before the whole file; a MB of it at most.
    if crate::files::external::is_heif(path) {
        let find = |data: &[u8]| {
            let at = (data.windows(8))
                .position(|w| w.starts_with(b"Exif\0\0") && matches!(&w[6..], b"II" | b"MM"))?;
            Some(data[at..data.len().min(at + (1 << 20))].to_vec())
        };
        let head = crate::images::decode::read_head(path, 1 << 20).ok()?;
        // Found too near the head's end, it may be cut short (EXIF blocks reach 64 KB).
        let whole = head.len() < 1 << 20;
        let in_head = find(&head).filter(|e| whole || e.len() > 1 << 16);
        // Read whole only when the head was not all of it, and never more than a decode would.
        let rest = || find(&crate::images::decode::read_limited(path).ok()?);
        return in_head.or_else(|| (!whole).then(rest).flatten());
    }
    if crate::model::listing::is_jpeg(path)
        && let Some(exif) = jpeg_app1(path, b"Exif\0\0")
    {
        return exif;
    }
    // A PNG is read once, by the png crate, as the date sort and the search read every PNG of
    // a folder. Through the image crate, one without an eXIf chunk was opened a second time for
    // its text.
    if crate::model::listing::has_ext(path, &["png"])
        && let Some(exif) = png_exif(path)
    {
        return exif;
    }
    let mut dec = crate::images::decode::image_reader(path)
        .ok()?
        .into_decoder()
        .ok()?;
    block_from_decoder(path, &mut dec)
}

/// Reads the EXIF block of `path` as a file made from its image keeps it (`how`). It is read as the
/// panel reads it (`read_exif_block`), so a copy of a HEIC, AVIF, JPEG XL or RAW photo keeps the
/// EXIF the panel shows. Before, a copy read it only through the image crate, which cannot open
/// those, and lost it. The block is made a bare TIFF block, because a JPEG's encoder puts its own
/// "Exif\0\0" before it and a PNG's eXIf chunk must start with the TIFF header. A turned image
/// keeps the rest of its block whole, because it is the user's own file and the cut would lose what
/// lies past the values it follows, such as a SubIFDs directory or a maker note's data. Only a copy
/// is cut (`derived_block`). A copy for sharing gets the whole block too, because it takes only
/// the tags the user keeps into a block made new (`shared_block`). Returns `None` when there is
/// no block.
pub fn derived(path: &Path, how: crate::images::save::Derived) -> Option<Vec<u8>> {
    derived_of(&read_exif_block(path)?, how)
}

/// Makes the EXIF block `raw` what a file made as `how` keeps of it, as `derived` does.
/// Returns `None` for a block without a TIFF header.
pub fn derived_of(raw: &[u8], how: crate::images::save::Derived) -> Option<Vec<u8>> {
    match how {
        crate::images::save::Derived::Copy => derived_block(raw),
        _ => Some(ifds(raw)?.0.data.to_vec()),
    }
}

/// Cuts the EXIF block `raw` to what a copy keeps of it: a bare TIFF block of the directories
/// that IFD0, Exif, GPS and Interop are and the values they name, without the fields of IFD0
/// that describe the stored pixels (`drop_pixel_fields`). The thumbnail (IFD1), which a copy
/// drops anyway, and a RAW's or HEIC's picture after the block are left out. Returns
/// `None` for a block without a TIFF header.
fn derived_block(raw: &[u8]) -> Option<Vec<u8>> {
    let mut data = ifds(raw)?.0.data.to_vec();
    drop_pixel_fields(&mut data);
    let (t, ifd0, exif, gps) = ifds(&data)?;
    let offset = |ifd: &Ifd, tag| Some(t.int(find(ifd, tag)?)? as usize);
    let interop_at = offset(&exif, INTEROP_IFD);
    let interop = interop_at.map(|at| t.ifd(at)).unwrap_or_default();
    let dirs = [
        (t.u32(4).map(|at| at as usize), &ifd0),
        (offset(&ifd0, EXIF_IFD), &exif),
        (offset(&ifd0, GPS_IFD), &gps),
        (interop_at, &interop),
    ];
    let mut end = 8;
    for (at, ifd) in dirs {
        let Some(at) = at else { continue };
        let n = t.u16(at).unwrap_or(0) as usize;
        end = end.max(at.saturating_add(6 + 12 * n));
        for (_, e) in ifd {
            end = end.max(
                e.value_offset
                    .saturating_add(type_size(e.field_type) * e.count as usize),
            );
        }
    }
    Some(t.data[..end.min(t.data.len())].to_vec())
}

/// Removes from IFD0 of the bare TIFF block `data` the fields that describe the stored pixels
/// of the file it came from (`PIXEL_FIELDS`, `DNG_FIELDS`). The entries kept move up, and the
/// space left, with the pointer to IFD1, is cleared. Before, a copy of a RAW said it had the
/// RAW's size, strips and SubIFDs. A directory that runs past the block is left as it is.
fn drop_pixel_fields(data: &mut [u8]) {
    let Some((t, _, _, _)) = ifds(data) else {
        return;
    };
    let (little, at) = (t.is_little_endian, t.u32(4).unwrap_or(0) as usize);
    let n = t.u16(at).unwrap_or(0) as usize;
    let Some(entries) = data.get_mut(at..at.saturating_add(6 + 12 * n)) else {
        return;
    };
    let tag = |e: &[u8]| match little {
        true => u16::from_le_bytes([e[0], e[1]]),
        false => u16::from_be_bytes([e[0], e[1]]),
    };
    let kept: Vec<u8> = (entries[2..2 + 12 * n].chunks(12))
        .filter(|e| !PIXEL_FIELDS.contains(&tag(e)) && !DNG_FIELDS.contains(&tag(e)))
        .flatten()
        .copied()
        .collect();
    let count = (kept.len() / 12) as u16;
    entries[..2].copy_from_slice(&match little {
        true => count.to_le_bytes(),
        false => count.to_be_bytes(),
    });
    entries[2..].fill(0);
    entries[2..2 + kept.len()].copy_from_slice(&kept);
}

/// Returns the name Copy for Sharing gives the tag `tag` of the directory `dir` ("Image", "Exif"
/// or "GPS"). It is the tag's name in the EXIF window, or else the directory and the tag's
/// number, so a tag without a name can still be left out.
fn shared_tag_name(dir: &str, tag: u16) -> String {
    tag_name(dir == "GPS", tag).map_or_else(|| format!("{dir} 0x{tag:04x}"), str::to_owned)
}

/// Returns the name that the word `stored` of the file of the last choice (`LeftOut`) has now.
/// A tag without a name is stored as its directory and number, such as "Exif 0xa460". When a
/// later Glimt gives that tag a name in `tag_name`, the stored number still means the same tag,
/// now under its name, so a tag the user left out is not kept unseen. Returns `stored` itself
/// for every other word.
pub fn shared_tag_name_now(stored: &str) -> String {
    let named = stored.split_once(" 0x").and_then(|(dir, number)| {
        let dir = ["Image", "Exif", "GPS"].into_iter().find(|d| *d == dir)?;
        let is_hex = number.len() == 4 && number.bytes().all(|b| b.is_ascii_hexdigit());
        let tag = u16::from_str_radix(number, 16).ok().filter(|_| is_hex)?;
        Some(shared_tag_name(dir, tag))
    });
    named.unwrap_or_else(|| stored.to_owned())
}

/// Returns the names a copy for sharing gives the tags of IFD0, Exif and GPS: each name of
/// `tag_name`, and in each directory one name of the form a tag without a name gets
/// (`shared_tag_name`). The file of the last choice holds them, so src/frozen-1.0.txt pins them.
#[cfg(test)]
pub(crate) fn every_shared_tag_name() -> Vec<String> {
    let mut names = Vec::new();
    for dir in ["Image", "Exif", "GPS"] {
        let all = (0..=u16::MAX).map(|tag| shared_tag_name(dir, tag));
        let (unnamed, named): (Vec<String>, Vec<String>) = all.partition(|n| n.contains(" 0x"));
        names.extend(named);
        names.extend(unnamed.last().cloned());
    }
    names.sort();
    names.dedup();
    names
}

/// Whether the tag `tag` of the directory `dir` is in every copy for sharing, also after Remove
/// All. The orientation keeps a photo upright, and the colour space says that a photo without a
/// profile is Adobe RGB, so both decide how the copy looks.
fn is_always_shared(dir: &str, tag: u16) -> bool {
    (dir == "Image" && tag == ORIENTATION) || (dir == "Exif" && tag == 0xa001)
}

/// The tags of the directories IFD0, Exif and GPS that a copy for sharing can take, each with
/// its directory's name. The pointers to the other directories are not tags of their own, and
/// IFD0's fields of the stored pixels (`PIXEL_FIELDS`, `DNG_FIELDS`) are in no copy.
fn shareable<'a>(
    ifd0: &'a Ifd,
    exif: &'a Ifd,
    gps: &'a Ifd,
) -> impl Iterator<Item = (&'static str, u16, Entry)> + 'a {
    let dirs = [("Image", ifd0), ("Exif", exif), ("GPS", gps)];
    dirs.into_iter().flat_map(|(dir, ifd)| {
        (ifd.iter())
            .filter(move |(tag, _)| {
                let pointer = dir != "GPS" && matches!(*tag, EXIF_IFD | GPS_IFD | INTEROP_IFD);
                let pixels =
                    dir == "Image" && (PIXEL_FIELDS.contains(tag) || DNG_FIELDS.contains(tag));
                !pointer && !pixels
            })
            .map(move |&(tag, e)| (dir, tag, e))
    })
}

/// Returns the EXIF thumbnail of the block `t`: the JPEG that IFD1 points at, or `None` when the
/// block has none or it lies past the block.
fn exif_thumbnail<'a>(t: &Tiff<'a>) -> Option<&'a [u8]> {
    let ifd0 = t.u32(4)? as usize;
    let ifd1 = t.u32(ifd0 + 2 + 12 * t.u16(ifd0)? as usize)? as usize;
    if ifd1 == 0 {
        return None;
    }
    let ifd1 = t.ifd(ifd1);
    let at = t.int(find(&ifd1, 0x0201)?)? as usize;
    let len = t.int(find(&ifd1, 0x0202)?)? as usize;
    t.data.get(at..at.checked_add(len)?)
}

/// Returns the names of the tags of the EXIF block `raw` that Copy for Sharing can leave out
/// (`shared_tag_name`), and `EXIF_THUMBNAIL` when it has one. The tags every copy keeps
/// (`is_always_shared`) are not among them. Returns nothing for a block without a TIFF header.
pub fn shared_tag_names(raw: &[u8]) -> Vec<String> {
    let Some((t, ifd0, exif, gps)) = ifds(raw) else {
        return Vec::new();
    };
    let mut names: Vec<String> = shareable(&ifd0, &exif, &gps)
        .filter(|&(dir, tag, _)| !is_always_shared(dir, tag))
        .map(|(dir, tag, _)| shared_tag_name(dir, tag))
        .collect();
    if exif_thumbnail(&t).is_some() {
        names.push(EXIF_THUMBNAIL.into());
    }
    names
}

/// Makes a new EXIF block for a copy for sharing. It holds the tags of `raw` that `keeps` keeps
/// by their names (`shared_tag_name`), and always the orientation, the colour space and the
/// Interop directory's index and version, which decide how the image looks. Its other entries
/// are not taken, as a crafted one could point at the GPS directory's values. Nothing else of
/// `raw` is in it: each value is copied on its own into a block made new, so the bytes of a tag
/// left out cannot linger in it. The block is written in `raw`'s byte order, so the values are
/// copied as they are. The EXIF thumbnail (IFD1) is in it, as a JPEG, only when `keeps` keeps
/// `EXIF_THUMBNAIL`. With `keeps_hdr_headroom`, a maker note left out of an iPhone's photo is
/// written new with only its HDR headroom and gain (`apple_hdr_maker_note`), which its gain map
/// needs. Returns `None` when `raw` has no TIFF header or nothing is kept.
/// shortcut: a kept MakerNote moves to where the new block puts it, so a maker note that points
/// at offsets from the TIFF header (Canon) loses them, as in every tool that writes a new block;
/// move its offsets too if users need a maker note kept whole.
pub fn shared_block(
    raw: &[u8],
    keeps: &dyn Fn(&str) -> bool,
    keeps_hdr_headroom: bool,
) -> Option<Vec<u8>> {
    let (t, ifd0, exif, gps) = ifds(raw)?;
    // The value bytes taken so far. A real block holds no more of them than its own size, but a
    // crafted one can name one large value from every entry, which would fill memory if each
    // were copied. Such a block is refused whole.
    let taken = std::cell::Cell::new(0usize);
    let value = |e: &Entry| {
        let len = type_size(e.field_type).checked_mul(e.count as usize)?;
        let bytes = t
            .data
            .get(e.value_offset..e.value_offset.checked_add(len)?)?;
        taken.set(taken.get().saturating_add(len));
        (taken.get() <= t.data.len()).then(|| bytes.to_vec())
    };
    let mut dirs: [NewIfd; 5] = Default::default();
    let mut seen = std::collections::HashSet::new();
    for (dir, tag, e) in shareable(&ifd0, &exif, &gps) {
        // A tag named twice is taken once, the first time, as the reader takes it (`find`).
        if !seen.insert((dir, tag)) {
            continue;
        }
        if is_always_shared(dir, tag) || keeps(&shared_tag_name(dir, tag)) {
            let at = ["Image", "Exif", "GPS"]
                .iter()
                .position(|d| *d == dir)
                .unwrap_or(0);
            dirs[at].extend(value(&e).map(|v| (tag, e.field_type, e.count, v)));
        } else if keeps_hdr_headroom
            && dir == "Exif"
            && tag == MAKER_NOTE
            && !dirs[1].iter().any(|kept| kept.0 == MAKER_NOTE)
        {
            let maker_note = value(&e).and_then(|v| apple_hdr_maker_note(&v));
            dirs[1].extend(maker_note.map(|v| (tag, UNDEFINED, v.len() as u32, v)));
        }
    }
    let interop = find(&exif, INTEROP_IFD).and_then(|e| t.int(e));
    for (tag, e) in interop.map(|at| t.ifd(at as usize)).unwrap_or_default() {
        // The index ("R98", "R03") and the version ("0100") are four bytes each.
        let value = value(&e).filter(|v| matches!(tag, 0x0001 | 0x0002) && v.len() <= 4);
        dirs[3].extend(value.map(|v| (tag, e.field_type, e.count, v)));
    }
    if taken.get() > t.data.len() {
        return None;
    }
    let thumbnail = keeps(EXIF_THUMBNAIL).then(|| exif_thumbnail(&t)).flatten();
    write_block(t.is_little_endian, dirs, thumbnail)
}

/// Makes a new iPhone maker note that holds only the HDR headroom and gain
/// (`APPLE_HDR_TAGS`) of the maker note `maker_note`. Chrome and Apple read them to show the
/// photo's HDR gain map, and they are numbers that say nothing of the place, the camera or the
/// owner. Every other tag of the maker note is left out. Returns `None` when `maker_note` is no
/// iPhone's (`APPLE_MAKER_NOTE_HEADER`) or has no HDR headroom.
fn apple_hdr_maker_note(maker_note: &[u8]) -> Option<Vec<u8>> {
    if !maker_note.starts_with(APPLE_MAKER_NOTE_HEADER) {
        return None;
    }
    let t = Tiff {
        data: maker_note,
        is_little_endian: false,
    };
    let mut kept: Vec<(u16, &[u8])> = Vec::new();
    for (tag, e) in t.ifd(APPLE_MAKER_NOTE_HEADER.len()) {
        // A crafted note may name a tag twice; the first is taken, as Chrome takes it.
        let is_new = !kept.iter().any(|(seen, _)| *seen == tag);
        if is_new
            && APPLE_HDR_TAGS.contains(&tag)
            && (e.field_type, e.count) == (SRATIONAL, 1)
            && let Some(value) = maker_note.get(e.value_offset..e.value_offset.saturating_add(8))
        {
            kept.push((tag, value));
        }
    }
    if !kept.iter().any(|(tag, _)| *tag == APPLE_HDR_TAGS[0]) {
        return None;
    }
    kept.sort_by_key(|(tag, _)| *tag);
    let values_at = APPLE_MAKER_NOTE_HEADER.len() + 2 + 12 * kept.len() + 4;
    let mut out = APPLE_MAKER_NOTE_HEADER.to_vec();
    out.extend((kept.len() as u16).to_be_bytes());
    for (i, (tag, _)) in kept.iter().enumerate() {
        out.extend(tag.to_be_bytes());
        out.extend(SRATIONAL.to_be_bytes());
        out.extend(1u32.to_be_bytes());
        out.extend(((values_at + 8 * i) as u32).to_be_bytes());
    }
    out.extend([0; 4]);
    kept.iter().for_each(|(_, value)| out.extend(*value));
    Some(out)
}

/// Writes a new TIFF block of the directories `dirs` (IFD0, Exif, GPS, Interop and IFD1, whose
/// entries are made here for `thumbnail`), in the byte order `little` says. The pointers between
/// them are added for the directories that have entries. The directories come one after the
/// other from byte 8, and the values longer than four bytes after them, the thumbnail first.
/// Returns `None` when there is nothing to write.
fn write_block(little: bool, mut dirs: [NewIfd; 5], thumbnail: Option<&[u8]>) -> Option<Vec<u8>> {
    let p16 = |v: u16| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let p32 = |v: u32| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    if let Some(jpeg) = thumbnail {
        // The thumbnail's offset is set below, where the values start.
        dirs[4] = vec![
            (0x0103, SHORT, 1, p16(6).to_vec()),
            (0x0201, LONG, 1, vec![0; 4]),
            (0x0202, LONG, 1, p32(jpeg.len() as u32).to_vec()),
        ];
    }
    for (from, to, tag) in [(3, 1, INTEROP_IFD), (1, 0, EXIF_IFD), (2, 0, GPS_IFD)] {
        if !dirs[from].is_empty() {
            dirs[to].push((tag, LONG, 1, vec![0; 4]));
        }
    }
    if dirs.iter().all(Vec::is_empty) {
        return None;
    }
    dirs.iter_mut().for_each(|d| d.sort_by_key(|e| e.0));
    // Where each directory starts. IFD0 is always there, and the others only with entries.
    let mut offsets = [0u32; 5];
    let mut end = 8;
    for (i, d) in dirs.iter().enumerate() {
        if i == 0 || !d.is_empty() {
            offsets[i] = end as u32;
            end += 2 + 12 * d.len() + 4;
        }
    }
    let mut values = thumbnail.map(<[u8]>::to_vec).unwrap_or_default();
    let set = |d: &mut NewIfd, tag: u16, v: u32| {
        (d.iter_mut().filter(|e| e.0 == tag)).for_each(|e| e.3 = p32(v).to_vec())
    };
    set(&mut dirs[4], 0x0201, end as u32);
    set(&mut dirs[1], INTEROP_IFD, offsets[3]);
    set(&mut dirs[0], EXIF_IFD, offsets[1]);
    set(&mut dirs[0], GPS_IFD, offsets[2]);
    let mut out = if little {
        b"II*\0".to_vec()
    } else {
        b"MM\0*".to_vec()
    };
    out.extend(p32(8));
    for (i, d) in dirs.iter().enumerate() {
        if i != 0 && d.is_empty() {
            continue;
        }
        out.extend(p16(d.len() as u16));
        for (tag, kind, count, value) in d {
            out.extend(p16(*tag));
            out.extend(p16(*kind));
            out.extend(p32(*count));
            if value.len() <= 4 {
                out.extend(value);
                out.extend(&[0, 0, 0, 0][value.len()..]);
            } else {
                // Values start on an even offset, as TIFF asks.
                values.extend(vec![0; values.len() % 2]);
                out.extend(p32((end + values.len()) as u32));
                values.extend(value);
            }
        }
        // IFD0 leads to IFD1, the thumbnail's directory, when there is one.
        let next = if i == 0 { offsets[4] } else { 0 };
        out.extend(p32(next));
    }
    out.extend(values);
    Some(out)
}

/// Moves the offsets of the images that the index of an MPF segment names (a phone's second
/// image or gain map after the main one) by `by` bytes, and sets the size of the first, the
/// main image, to `first_size`. A copy for sharing that keeps them calls it, because the
/// metadata before the main image changed, while the images after it are copied as they are.
/// `mpf` is the segment's contents, from "MPF\0". Returns `None`, with nothing changed, when its
/// index cannot be read.
pub fn shift_mp_offsets(mpf: &mut [u8], by: i64, first_size: u32) -> Option<()> {
    let (start, n, little) = mp_index(mpf)?;
    let index = mpf.get_mut(start..start.checked_add(16 * n)?)?;
    let read = |b: &[u8]| {
        let b = [b[0], b[1], b[2], b[3]];
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    let write = |v: u32| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    // The new offsets are found first, so an index that cannot take them is left as it was.
    let mut moved = Vec::with_capacity(n);
    for entry in index.chunks(16).skip(1) {
        let offset = read(&entry[8..12]);
        moved.push(match offset {
            0 => 0,
            _ => u32::try_from(offset as i64 + by).ok()?,
        });
    }
    for (i, entry) in index.chunks_mut(16).enumerate() {
        match i {
            0 => entry[4..8].copy_from_slice(&write(first_size)),
            _ => entry[8..12].copy_from_slice(&write(moved[i - 1])),
        }
    }
    Some(())
}

/// Returns where the index of the MPF segment `mpf` (its contents, from "MPF\0") starts in it,
/// how many entries of 16 bytes it has, and whether they are little-endian. Returns `None` when
/// the index cannot be read.
fn mp_index(mpf: &[u8]) -> Option<(usize, usize, bool)> {
    let (t, ifd0, _, _) = ifds(mpf.get(4..)?)?;
    let e = find(&ifd0, 0xb002).filter(|e| e.field_type == UNDEFINED)?;
    Some((
        4 + e.value_offset,
        e.count as usize / 16,
        t.is_little_endian,
    ))
}

/// Returns the entries of the index of the MPF segment `mpf` (its contents, from "MPF\0"), one
/// for each image: its attribute, its size, and its offset from the segment's TIFF header,
/// which is 0 for the main image. Returns `None` when the index cannot be read.
pub fn mp_entries(mpf: &[u8]) -> Option<Vec<[u32; 3]>> {
    let (start, n, little) = mp_index(mpf)?;
    let index = mpf.get(start..start.checked_add(16 * n)?)?;
    let read = |b: &[u8]| {
        let b = [b[0], b[1], b[2], b[3]];
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    let entries = index.chunks(16);
    Some(
        entries
            .map(|e| [read(&e[..4]), read(&e[4..8]), read(&e[8..12])])
            .collect(),
    )
}

/// Reads the EXIF block of `path` through `dec`, the image crate's decoder of it. A PNG keeps
/// EXIF in an eXIf chunk, which the decoder reads, or in the text chunk that exiv2 (digiKam,
/// GIMP) and older ImageMagick write (`png_exif`). This is the one reader of an EXIF block the
/// image crate decodes, so the panel, the date sort and a copy or turned file all see the same.
pub fn block_from_decoder(path: &Path, dec: &mut impl image::ImageDecoder) -> Option<Vec<u8>> {
    let png = || crate::model::listing::has_ext(path, &["png"]);
    (dec.exif_metadata().ok().flatten()).or_else(|| png().then(|| png_exif(path)).flatten()?)
}

/// Reads the orientation that the image `dec` decodes is turned upright by. It is the decoder's
/// own, or else the one in `exif`, the EXIF block `block_from_decoder` read of it, as the info
/// panel reads it. It is the one reader of it, so the view, a turned or copied file and the info
/// panel agree. The decoder finds none in a PNG's text chunk, nor in an EXIF block that starts with
/// "Exif\0\0", as some programs write a WebP's. Before, such a photo was shown unturned while the
/// panel said "Rotate 90 CW". The caller reads `exif` once for this and its other uses, because for
/// a PNG without an eXIf chunk `block_from_decoder` opens the file again.
pub fn stored_orientation(dec: &mut impl image::ImageDecoder, exif: Option<&[u8]>) -> Orientation {
    let own = dec.orientation().unwrap_or(Orientation::NoTransforms);
    if own != Orientation::NoTransforms {
        return own;
    }
    exif.and_then(orientation).unwrap_or(own)
}

/// Reads the EXIF block of a PNG with the png crate: its eXIf chunk, or else the text chunk
/// that exiv2 (digiKam, GIMP) and older ImageMagick write, named "Raw profile type exif" or
/// "Raw profile type APP1", plain, compressed (zTXt) or international (iTXt). Returns
/// `Some(None)` for a PNG without one, and `None` when the png crate cannot read the file.
fn png_exif(path: &Path) -> Option<Option<Vec<u8>>> {
    // shortcut: only the text before the pixels is read, where exiv2 and ImageMagick put it,
    // so the panel and the date sort do not read a whole PNG; read the chunks after IDAT too if
    // users miss EXIF there. A turn or copy reads them all (`png_metadata`), as it drops them.
    const KEYS: [&[u8]; 2] = [b"Raw profile type exif", b"Raw profile type APP1"];
    let file = std::io::BufReader::new(crate::images::decode::open_regular(path).ok()?);
    let reader = png::Decoder::new(file).read_info().ok()?;
    let info = reader.info();
    if let Some(exif) = &info.exif_metadata {
        return Some(Some(exif.to_vec()));
    }
    Some(png_texts(info, &KEYS).find_map(|text| raw_profile(&text)))
}

/// Reads the EXIF block, XMP packet and Photoshop resources of the PNG `data` from every chunk
/// of `PNG_METADATA_TEXT` and its eXIf, before the pixels or after them, plain or compressed.
/// The first of each kind is taken. What cannot be read is left `None`, as is all of it for a
/// file the png crate cannot read, and the chunks after a broken one are not read.
pub(crate) fn png_metadata(data: &[u8]) -> PngMetadata {
    let mut found = PngMetadata::default();
    let Ok(mut reader) = png::Decoder::new(std::io::Cursor::new(data)).read_info() else {
        return found;
    };
    // The pixels are passed over, not decoded, to reach the chunks after them. A chunk that
    // cannot be read ends the walk, and what was read before it is kept.
    let _ = reader.finish();
    let info = reader.info();
    found.exif = info.exif_metadata.as_deref().map(<[u8]>::to_vec);
    for (key, kind) in PNG_METADATA_TEXT {
        let keys = [key];
        let value = png_texts(info, &keys).find_map(|text| match kind {
            PngMetadataKind::Exif => raw_profile(&text),
            _ if key.starts_with(b"Raw profile type ") => raw_profile_bytes(&text),
            _ => Some(text.into_bytes()),
        });
        let slot = found.slot(kind);
        if slot.is_none() {
            *slot = value;
        }
    }
    found
}

/// Returns what the PNG chunk `name` with the data `data` holds of `PNG_METADATA_TEXT`, or
/// `None` when it is none of them.
pub(crate) fn png_metadata_kind(name: &[u8], data: &[u8]) -> Option<PngMetadataKind> {
    if name == b"eXIf" {
        return Some(PngMetadataKind::Exif);
    }
    if !matches!(name, b"tEXt" | b"zTXt" | b"iTXt") {
        return None;
    }
    let keyword = &data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())];
    (PNG_METADATA_TEXT.iter())
        .find(|(key, _)| *key == keyword)
        .map(|(_, kind)| *kind)
}

/// Returns the texts of the PNG's text chunks named one of `keys`, plain, compressed (zTXt) or
/// international (iTXt), as the png crate read them before the pixels.
pub(crate) fn png_texts<'a>(
    info: &'a png::Info,
    keys: &'a [&[u8]],
) -> impl Iterator<Item = String> + 'a {
    let key = |k: &String| keys.contains(&k.as_bytes());
    // Compressed text is unpacked with the png crate's limit of 2 MiB, as a block is 64 KB.
    let plain = (info.uncompressed_latin1_text.iter())
        .filter(move |t| key(&t.keyword))
        .map(|t| Some(t.text.clone()));
    let zipped = (info.compressed_latin1_text.iter())
        .filter(move |t| key(&t.keyword))
        .map(|t| {
            let mut t = t.clone();
            t.decompress_text().ok()?;
            t.get_text().ok()
        });
    let international = (info.utf8_text.iter())
        .filter(move |t| key(&t.keyword))
        .map(|t| {
            let mut t = t.clone();
            t.decompress_text().ok()?;
            t.get_text().ok()
        });
    (plain.chain(zipped).chain(international)).flatten()
}

/// Reads the EXIF block of an ImageMagick raw profile: a line with its type, a line with its
/// length in bytes, then the bytes in hex over several lines. exiv2 writes "Exif\0\0" before
/// the TIFF header, and so may APP1, so the block starts at the TIFF header, as exiv2 reads it
/// and as an eXIf chunk holds it. Returns `None` for a profile that is cut short, not hex, or
/// without a TIFF header.
fn raw_profile(text: &str) -> Option<Vec<u8>> {
    let bytes = raw_profile_bytes(text)?;
    let at = (bytes.windows(4)).position(|w| w == b"II*\0" || w == b"MM\0*")?;
    Some(bytes[at..].to_vec())
}

/// Reads the bytes of an ImageMagick raw profile of any type: a line with its type, a line with
/// its length in bytes, then the bytes in hex over several lines. Returns `None` for a profile
/// that is cut short or not hex.
pub(crate) fn raw_profile_bytes(text: &str) -> Option<Vec<u8>> {
    let mut words = text.split_ascii_whitespace();
    let _kind = words.next()?;
    let len: usize = words.next()?.parse().ok()?;
    let hex: Vec<u8> = words.flat_map(str::bytes).collect();
    if len > hex.len() / 2 {
        return None;
    }
    let digit = |b: u8| (b as char).to_digit(16);
    (hex[..len * 2].chunks(2))
        .map(|pair| Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8))
        .collect()
}

/// The size in bytes of one value of the TIFF type `kind`.
fn type_size(kind: u16) -> usize {
    match kind {
        SHORT | 8 => 2,
        LONG | SLONG | 11 | IFD => 4,
        RATIONAL | SRATIONAL | 12 => 8,
        _ => 1,
    }
}

/// The TIFF block with its IFD0, Exif and GPS directories.
fn ifds(raw: &[u8]) -> Option<(Tiff<'_>, Ifd, Ifd, Ifd)> {
    let data = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    // JPEG XL's Exif box starts with a 4-byte offset to the TIFF header.
    let data = match data.get(4..6) {
        Some(b"II" | b"MM") if !matches!(data.get(..2), Some(b"II" | b"MM")) => &data[4..],
        _ => data,
    };
    let little = match data.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return None,
    };
    let t = Tiff {
        data,
        is_little_endian: little,
    };
    let ifd0 = t.ifd(t.u32(4)? as usize);
    let sub = |tag: u16| {
        let e = ifd0.iter().find(|(t2, _)| *t2 == tag)?.1;
        Some(t.ifd(t.int(e)? as usize))
    };
    let (exif, gps) = (
        sub(EXIF_IFD).unwrap_or_default(),
        sub(GPS_IFD).unwrap_or_default(),
    );
    Some((t, ifd0, exif, gps))
}

fn parse(raw: &[u8]) -> Vec<(&'static str, String)> {
    let Some((t, ifd0, exif, gps)) = ifds(raw) else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    let make = find(&ifd0, 0x010f).and_then(|e| t.text(e));
    let model = find(&ifd0, 0x0110).and_then(|e| t.text(e));
    let camera = match (make, model) {
        // Most models already start with the make ("Canon EOS R6").
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (make, model) => make.or(model),
    };
    rows.extend(camera.map(|c| ("Camera", c)));
    rows.extend(
        find(&exif, 0xa434)
            .and_then(|e| t.text(e))
            .map(|l| ("Lens", l)),
    );
    let taken = find(&exif, 0x9003).or_else(|| find(&ifd0, 0x0132));
    rows.extend(taken.and_then(|e| t.text(e)).map(|d| ("Taken", d)));
    if let Some(s) = find(&exif, 0x829a).and_then(|e| t.rational(e, 0)) {
        rows.push(("Exposure", exposure(s)));
    }
    rows.extend(
        find(&exif, 0x829d)
            .and_then(|e| t.rational(e, 0))
            .map(|f| ("Aperture", format!("f/{f:.1}"))),
    );
    rows.extend(
        find(&exif, 0x8827)
            .and_then(|e| t.int(e))
            .map(|iso| ("ISO", iso.to_string())),
    );
    if let Some(mm) = find(&exif, 0x920a).and_then(|e| t.rational(e, 0)) {
        // One decimal, none for whole numbers: "2.8 mm", "24 mm".
        let mm = (mm * 10.0).round() / 10.0;
        let full_frame = find(&exif, 0xa405)
            .and_then(|e| t.int(e))
            .filter(|&f| f > 0);
        let text = match full_frame {
            Some(f) => format!("{mm} mm (35 mm: {f} mm)"),
            None => format!("{mm} mm"),
        };
        rows.push(("Focal length", text));
    }
    if let Some(ev) = find(&exif, 0x9204).and_then(|e| t.rational(e, 0)) {
        rows.push(("Exposure bias", format!("{ev:+.1} EV")));
    }
    if let Some(flash) = find(&exif, 0x9209).and_then(|e| t.int(e)) {
        rows.push(("Flash", if flash & 1 == 1 { "Fired" } else { "No" }.into()));
    }
    // Degrees, minutes, seconds; negative for south and west.
    let coord = |value: u16, reference: u16, negative: &str| -> Option<f64> {
        let e = find(&gps, value)?;
        // Minutes and seconds left out (one value only, against the standard): whole degrees.
        let part = |i| t.rational(e, i).unwrap_or(0.0);
        let deg = t.rational(e, 0)? + part(1) / 60.0 + part(2) / 3600.0;
        let sign = find(&gps, reference)
            .and_then(|r| t.text(r))
            .is_some_and(|r| r == negative);
        Some(if sign { -deg } else { deg })
    };
    if let (Some(lat), Some(lon)) = (coord(2, 1, "S"), coord(4, 3, "W")) {
        rows.push(("GPS", format!("{lat:.5}, {lon:.5}")));
    }
    rows
}

/// Returns a row's value as the panel shows it, with the flash's in the language in use. The
/// rows keep English, which the exif: search and the templates' %Flash% read.
pub fn shown_value<'a>(label: &str, value: &'a str) -> &'a str {
    match (label, value) {
        ("Flash", "Fired") => crate::support::lang::tr("Fired"),
        ("Flash", "No") => crate::support::lang::tr("No"),
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    /// A PNG's EXIF, XMP and Photoshop resources are read from every chunk of
    /// `PNG_METADATA_TEXT`, plain, compressed (zTXt) and international and compressed (iTXt),
    /// also after the pixels, where ExifTool adds them. A turn drops these chunks, so each one
    /// it drops must be read here.
    #[test]
    fn png_metadata_reads_every_form_wherever_it_stands() {
        use crate::support::test_support::{CANON_EXIF, png_with_texts, raw_profile_text};
        let xmp = "<x:xmpmeta>Oslo</x:xmpmeta>";
        for (key, kind) in super::PNG_METADATA_TEXT {
            let key = std::str::from_utf8(key).unwrap();
            let (text, want) = match key {
                "XML:com.adobe.xmp" => (xmp.to_string(), xmp.as_bytes()),
                _ => (raw_profile_text("x", CANON_EXIF), CANON_EXIF),
            };
            for chunk in ["tEXt", "zTXt", "iTXt"] {
                for before in [true, false] {
                    let png = png_with_texts(&[(chunk, key, &text)], before);
                    let mut found = super::png_metadata(&png);
                    let read = found.slot(kind).take();
                    assert_eq!(read.as_deref(), Some(want), "{key} in {chunk}, {before}");
                }
            }
        }
    }

    /// A damaged PNG's metadata is read or not, never a panic.
    #[test]
    fn survives_broken_png_metadata() {
        use crate::support::test_support::{CANON_EXIF, png_with_texts, raw_profile_text};
        let exif = raw_profile_text("exif", CANON_EXIF);
        let png = png_with_texts(&[("zTXt", "Raw profile type exif", &exif)], false);
        for bytes in crate::support::test_support::broken(&png) {
            super::png_metadata(&bytes);
        }
    }

    /// A damaged HEIC's EXIF block is found or not, never a panic.
    #[test]
    fn survives_broken_heif_heads() {
        let dir = std::env::temp_dir().join(format!("glimt-test-heif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.heic");
        let seed = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\x10Exif\0\0MM\0*\0\0\0\x08\0\0";
        for bytes in crate::support::test_support::broken(seed) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::read_exif_block(&file);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A PNG's EXIF in the text chunk that exiv2 and ImageMagick write is read for the panel
    /// and the date sort, in each kind of text chunk, with "Exif\0\0" before it or not.
    #[test]
    fn reads_exif_from_png_text() {
        use crate::support::test_support::{CANON_EXIF, png_with_raw_profile};
        let dir = std::env::temp_dir().join(format!("glimt-test-pngtext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let prefixed = [&b"Exif\0\0"[..], CANON_EXIF].concat();
        for (chunk, key, block) in [
            ("tEXt", "Raw profile type exif", CANON_EXIF),
            ("zTXt", "Raw profile type exif", &prefixed[..]),
            ("iTXt", "Raw profile type APP1", &prefixed[..]),
        ] {
            png_with_raw_profile(&file, chunk, key, block);
            assert_eq!(
                read_exif_block(&file).as_deref(),
                Some(CANON_EXIF),
                "{chunk}"
            );
            assert_eq!(date(&file, false).as_deref(), Some("2020:01:02 03:04:05"));
        }
        // An eXIf chunk is read by the png crate too.
        use image::ImageEncoder;
        let exif = b"II*\0\x08\0\0\0\0\0\0\0";
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::new(2, 2).write_with_encoder(enc).unwrap();
        assert_eq!(read_exif_block(&file).as_deref(), Some(&exif[..]));
        // Another key's text is not taken for EXIF.
        png_with_raw_profile(&file, "zTXt", "Raw profile type iptc", CANON_EXIF);
        assert_eq!(read_exif_block(&file), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A date written with dashes reads in EXIF's own form, so it sorts among the others.
    #[test]
    fn a_date_with_dashes_reads_with_colons() {
        use crate::support::test_support::CANON_EXIF;
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-dashes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        let mut exif = CANON_EXIF.to_vec();
        let at = exif.windows(10).position(|w| w == b"2020:01:02").unwrap();
        exif[at..at + 10].copy_from_slice(b"2020-01-02");
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif).unwrap();
        image::RgbImage::new(2, 2).write_with_encoder(enc).unwrap();
        assert_eq!(date(&file, false).as_deref(), Some("2020:01:02 03:04:05"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A damaged PNG raw profile, and a damaged PNG around one, give an EXIF block or none,
    /// never a panic.
    #[test]
    fn survives_broken_png_raw_profiles() {
        use crate::support::test_support::{CANON_EXIF, broken, png_with_raw_profile};
        let dir = std::env::temp_dir().join(format!("glimt-test-pngraw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        png_with_raw_profile(&file, "tEXt", "Raw profile type exif", CANON_EXIF);
        let png = std::fs::read(&file).unwrap();
        for bytes in broken(&png) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = read_exif_block(&file);
        }
        for text in broken(b"\nexif\n      14\n4d4d002a00000008\n00000000aabb\n") {
            let _ = raw_profile(&String::from_utf8_lossy(&text));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;

    #[test]
    fn exposure_times() {
        assert_eq!(exposure(1.0 / 250.0), "1/250 s");
        assert_eq!(exposure(0.25), "1/4 s");
        assert_eq!(exposure(0.4), "0.4 s");
        assert_eq!(exposure(0.8), "0.8 s");
        assert_eq!(exposure(2.0), "2 s");
    }

    /// The XMP's orientation upright in both forms; nothing else changed, and broken XMP is
    /// survived.
    #[test]
    fn sets_xmp_upright() {
        let mut xmp = br#"<rdf:Description tiff:Orientation="6" a="6"><tiff:Orientation>8</tiff:Orientation></rdf:Description>"#.to_vec();
        super::set_xmp_orientation(&mut xmp, 1);
        assert_eq!(
            xmp,
            br#"<rdf:Description tiff:Orientation="1" a="6"><tiff:Orientation>1</tiff:Orientation></rdf:Description>"#
        );
        // The orientation under another prefix bound to TIFF's namespace is set too.
        let mut other =
            br#"<rdf:Description xmlns:ns1="http://ns.adobe.com/tiff/1.0/" ns1:Orientation="6"/>"#
                .to_vec();
        super::set_xmp_orientation(&mut other, 1);
        assert!(other.ends_with(br#"ns1:Orientation="1"/>"#));
        let mut odd =
            b"<rdf:Description tiff:Orientation='12' x:y='1'><tiff:Orientation/>".to_vec();
        let before = odd.clone();
        super::set_xmp_orientation(&mut odd, 1);
        assert_eq!(odd, before);
    }

    #[test]
    fn survives_broken_xmp_orientation() {
        for mut xmp in crate::support::test_support::broken(
            br#"<rdf:Description tiff:Orientation="6"><tiff:Orientation>8</tiff:Orientation></rdf:Description>"#,
        ) {
            super::set_xmp_orientation(&mut xmp, 1);
        }
    }

    /// With Auto Rotate off the view showed the pixels as stored: a quarter turn saved on a
    /// photo stored at 6 is 6, as the user saw it upright; with Auto Rotate on it is 3.
    #[test]
    fn saves_the_turn_on_what_was_shown() {
        use image::metadata::Orientation as O;
        let file =
            std::env::temp_dir().join(format!("glimt-test-shown-{}.jpg", std::process::id()));
        image::RgbImage::new(4, 2).save(&file).unwrap();
        let stored = || {
            use image::ImageDecoder;
            let mut d = image::ImageReader::open(&file)
                .unwrap()
                .into_decoder()
                .unwrap();
            d.orientation().unwrap()
        };
        super::write_orientation(&file, 1, false, true).unwrap();
        assert_eq!(stored(), O::Rotate90);
        super::write_orientation(&file, 1, false, false).unwrap();
        assert_eq!(stored(), O::Rotate90);
        super::write_orientation(&file, 1, false, true).unwrap();
        assert_eq!(stored(), O::Rotate180);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn turns_orientations() {
        use super::turned;
        assert_eq!(turned(1, 1, false), 6);
        assert_eq!(turned(1, 0, true), 2);
        assert_eq!(turned(6, 1, false), 3);
        assert_eq!(turned(6, 3, false), 1);
        assert_eq!(turned(3, 2, false), 1);
        // Mirror then a quarter turn clockwise is the transverse.
        assert_eq!(turned(1, 1, true), 7);
    }

    /// Adobe RGB from ColorSpace 2, or uncalibrated with R03; sRGB and R98 are not.
    #[test]
    fn tells_adobe_rgb() {
        // IFD0 with a pointer to the Exif IFD (at 26): ColorSpace, and a pointer to Interop.
        let block = |space: u16, index: &[u8; 4]| {
            let mut d = b"II*\0\x08\0\0\0\x01\0\x69\x87\x04\0\x01\0\0\0\x1a\0\0\0\0\0\0\0".to_vec();
            d.extend(b"\x02\0\x01\xa0\x03\0\x01\0\0\0");
            d.extend(space.to_le_bytes());
            d.extend(b"\0\0\x05\xa0\x04\0\x01\0\0\0\x38\0\0\0\0\0\0\0");
            // Interop IFD at 56: InteroperabilityIndex, 4 ASCII bytes in the entry.
            d.extend(b"\x01\0\x01\0\x02\0\x04\0\0\0");
            d.extend(index);
            d.extend(b"\0\0\0\0");
            d
        };
        assert!(super::is_adobe_rgb(&block(2, b"R98\0")));
        assert!(super::is_adobe_rgb(&block(0xffff, b"R03\0")));
        assert!(!super::is_adobe_rgb(&block(0xffff, b"R98\0")));
        assert!(!super::is_adobe_rgb(&block(1, b"R03\0")));
        assert!(!super::is_adobe_rgb(b"junk"));
    }

    /// Bytes between a JPEG's segments that are no marker, which some phones and editors write,
    /// are skipped as decoders skip them, so the image data after them is found, as it is by
    /// Copy for Sharing's walk.
    #[test]
    fn finds_the_scan_after_bytes_between_segments() {
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        image::RgbImage::new(8, 8)
            .write_with_encoder(encoder)
            .unwrap();
        let comment = b"\xff\xfe\0\x08a note";
        let junk = b"\0\0JUNK\xff\0";
        let junky = [&jpeg[..2], comment, junk, &jpeg[2..]].concat();
        let scan = super::jpeg_scan_start(&jpeg).unwrap();
        let moved = comment.len() + junk.len();
        assert_eq!(super::jpeg_scan_start(&junky), Some(scan + moved));
    }

    /// The EXIF block from the head of a JPEG; past the head, as after big other segments, from
    /// the whole file; none if the image data comes first.
    #[test]
    fn reads_jpeg_exif_from_head() {
        let dir = std::env::temp_dir().join(format!("glimt-head-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut base = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut base),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let app1 =
            b"Exif\0\0II*\0\x08\0\0\0\x01\0\x0f\x01\x02\0\x06\0\0\0\x1a\0\0\0\0\0\0\0Canon\0";
        let segment = |marker: u8, data: &[u8]| {
            let mut s = vec![0xff, marker];
            s.extend(((data.len() + 2) as u16).to_be_bytes());
            s.extend(data);
            s
        };
        let jpeg = |before: usize| {
            let mut j = vec![0xff, 0xd8];
            for _ in 0..before {
                j.extend(segment(0xe2, &[0; 60_000]));
            }
            j.extend(segment(0xe1, app1));
            j.extend(&base[2..]);
            j
        };
        let camera = |data: &[u8]| {
            let file = dir.join("a.jpg");
            std::fs::write(&file, data).unwrap();
            let rows = super::read(&file);
            rows.iter().find(|r| r.0 == "Camera").map(|r| r.1.clone())
        };
        let from_head = || super::jpeg_app1(&dir.join("a.jpg"), b"Exif\0\0");
        assert_eq!(camera(&jpeg(0)), Some("Canon".into()));
        assert!(matches!(from_head(), Some(Some(_))));
        assert_eq!(camera(&jpeg(5)), Some("Canon".into()));
        assert_eq!(from_head(), None);
        // Fill bytes before a marker, which some cameras write.
        let mut filled = jpeg(0);
        filled.splice(2..2, [0xff; 3]);
        assert_eq!(camera(&filled), Some("Canon".into()));
        assert!(matches!(from_head(), Some(Some(_))));
        assert_eq!(camera(&base), None);
        assert_eq!(from_head(), Some(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An orientation field with several values points elsewhere, maybe out of the EXIF block
    /// into the picture: it is refused and the file left as it was.
    #[test]
    fn refuses_an_orientation_field_with_several_values() {
        let dir = std::env::temp_dir().join(format!("glimt-orient-many-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let mut base = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut base),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // Orientation: SHORT, 3 values at offset 64, past the 26 bytes of TIFF data.
        let app1 = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x03\0\0\0\x40\0\0\0\0\0\0\0";
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(app1);
        jpeg.extend(&base[2..]);
        std::fs::write(&file, &jpeg).unwrap();
        assert!(super::write_orientation(&file, 1, false, true).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), jpeg);
        // An entry at the very end of the block, with no room for its value; and IFD0 past the
        // block. Both refused, the file as it was.
        for app1 in [
            b"Exif\0\0MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".as_slice(),
            b"Exif\0\0MM\0*\0\0\0\x40\0\0".as_slice(),
        ] {
            let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
            jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
            jpeg.extend(app1);
            jpeg.extend(&base[2..]);
            std::fs::write(&file, &jpeg).unwrap();
            assert!(super::write_orientation(&file, 1, false, true).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), jpeg);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG without EXIF gets one; turning again changes the field in place.
    #[test]
    fn writes_orientation() {
        let dir = std::env::temp_dir().join(format!("glimt-orient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        image::RgbImage::new(4, 2).save(&file).unwrap();
        super::write_orientation(&file, 1, false, true).unwrap();
        let o = |f: &std::path::Path| {
            use image::ImageDecoder;
            let mut d = image::ImageReader::open(f).unwrap().into_decoder().unwrap();
            d.orientation().unwrap()
        };
        assert_eq!(o(&file), image::metadata::Orientation::Rotate90);
        super::write_orientation(&file, 1, false, true).unwrap();
        assert_eq!(o(&file), image::metadata::Orientation::Rotate180);
        assert_eq!(image::open(&file).unwrap().width(), 4);
        // EXIF with other fields but no orientation: the field is added, the rest kept.
        let mut base = Vec::new();
        image::RgbImage::new(4, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut base),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let app1 =
            b"Exif\0\0II*\0\x08\0\0\0\x01\0\x0f\x01\x02\0\x06\0\0\0\x1a\0\0\0\0\0\0\0Canon\0";
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(app1);
        jpeg.extend(&base[2..]);
        std::fs::write(&file, &jpeg).unwrap();
        super::write_orientation(&file, 3, false, true).unwrap();
        assert_eq!(o(&file), image::metadata::Orientation::Rotate270);
        let rows = super::read(&file);
        assert!(rows.contains(&("Camera", "Canon".to_string())), "{rows:?}");
        // An XMP segment with its own orientation turns along.
        let xmp = b"http://ns.adobe.com/xap/1.0/\0<rdf:Description tiff:Orientation=\"1\"/>";
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((xmp.len() + 2) as u16).to_be_bytes());
        jpeg.extend(xmp);
        jpeg.extend(&base[2..]);
        let with_xmp = dir.join("xmp.jpg");
        std::fs::write(&with_xmp, &jpeg).unwrap();
        super::write_orientation(&with_xmp, 1, false, true).unwrap();
        let text = std::fs::read(&with_xmp).unwrap();
        let want = b"tiff:Orientation=\"6\"";
        assert!(text.windows(want.len()).any(|w| w == want));
        // Through a symlink: the file it points to is turned, and the link stays a link.
        let link = dir.join("link.jpg");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        super::write_orientation(&link, 1, false, true).unwrap();
        assert!(link.is_symlink());
        assert_eq!(o(&file), image::metadata::Orientation::NoTransforms);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A little-endian TIFF block: IFD0 with Make, Model and an Exif IFD with exposure,
    /// aperture and ISO.
    #[test]
    fn parses_common_fields() {
        let d = sample();
        let rows = super::parse(&d);
        let rows: Vec<(&str, &str)> = rows.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(
            rows,
            [
                ("Camera", "Canon R6"),
                ("Exposure", "1/250 s"),
                ("Aperture", "f/2.8"),
                ("ISO", "400")
            ]
        );
        assert!(super::parse(b"garbage").is_empty());
        // Offsets pointing outside the block must not panic.
        let mut bad = d.clone();
        bad.truncate(60);
        super::parse(&bad);
    }

    /// An entry that says it has no values, or fewer than are read, shows nothing: an
    /// aperture of count 0 showed an "f/" made of the next entry's tag and type.
    #[test]
    fn reads_only_the_values_an_entry_has() {
        let mut d = sample();
        // FNumber (0x829d), RATIONAL, its count 1 made 0.
        let at = d.windows(4).position(|w| w == [0x9d, 0x82, 5, 0]).unwrap();
        d[at + 4..at + 8].copy_from_slice(&0u32.to_le_bytes());
        let rows = super::parse(&d);
        assert!(rows.iter().all(|(k, _)| *k != "Aperture"));
        assert!(rows.iter().any(|(k, _)| *k == "Exposure"));
    }

    /// A GPS position stored as degrees alone, one value where the standard has three: shown.
    #[test]
    fn reads_a_gps_position_of_degrees_alone() {
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(count.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // IFD0 at 8 with the GPS directory at 26; its four entries end at 80, the two
        // rationals (60/1 north, 10/1 east) at 80 and 88.
        d.extend(1u16.to_le_bytes());
        entry(&mut d, 0x8825, 4, 1, 26);
        d.extend(0u32.to_le_bytes());
        d.extend(4u16.to_le_bytes());
        entry(&mut d, 1, 2, 2, u32::from(b'N'));
        entry(&mut d, 2, 5, 1, 80);
        entry(&mut d, 3, 2, 2, u32::from(b'E'));
        entry(&mut d, 4, 5, 1, 88);
        d.extend(0u32.to_le_bytes());
        for v in [60u32, 1, 10, 1] {
            d.extend(v.to_le_bytes());
        }
        let rows = super::parse(&d);
        assert!(
            rows.contains(&("GPS", "60.00000, 10.00000".into())),
            "{rows:?}"
        );
    }

    /// A block with every field the panel shows gives a row for each of `LABELS`, in their
    /// order, so `--remote info`, which prints only those, misses none of `parse`'s rows.
    #[test]
    fn the_rows_are_the_labels() {
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(count.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // The offsets count from "II", 6 bytes into the block.
        let at = |d: &Vec<u8>| d.len() - 6;
        // IFD0 at 8, with the make at 50, the Exif directory at 56 and the GPS one at 215.
        d.extend(3u16.to_le_bytes());
        entry(&mut d, 0x010f, 2, 6, 50);
        entry(&mut d, 0x8769, 4, 1, 56);
        entry(&mut d, 0x8825, 4, 1, 215);
        d.extend(0u32.to_le_bytes());
        d.extend(b"Canon\0");
        assert_eq!(at(&d), 56);
        // The Exif directory, its values from 158: the date, the lens, and the rationals of
        // the exposure, aperture, focal length and exposure bias. ISO and flash are inline.
        d.extend(8u16.to_le_bytes());
        entry(&mut d, 0xa434, 2, 5, 178);
        entry(&mut d, 0x9003, 2, 20, 158);
        entry(&mut d, 0x829a, 5, 1, 183);
        entry(&mut d, 0x829d, 5, 1, 191);
        entry(&mut d, 0x8827, 3, 1, 400);
        entry(&mut d, 0x920a, 5, 1, 199);
        entry(&mut d, 0x9204, 10, 1, 207);
        entry(&mut d, 0x9209, 3, 1, 1);
        d.extend(0u32.to_le_bytes());
        assert_eq!(at(&d), 158);
        d.extend(b"2020:01:02 03:04:05\0Lens\0");
        for v in [1u32, 250, 28, 10, 50, 1, 1, 1] {
            d.extend(v.to_le_bytes());
        }
        assert_eq!(at(&d), 215);
        // The GPS directory, its two rationals at 269.
        d.extend(4u16.to_le_bytes());
        entry(&mut d, 1, 2, 2, u32::from(b'N'));
        entry(&mut d, 2, 5, 1, 269);
        entry(&mut d, 3, 2, 2, u32::from(b'E'));
        entry(&mut d, 4, 5, 1, 277);
        d.extend(0u32.to_le_bytes());
        assert_eq!(at(&d), 269);
        for v in [60u32, 1, 10, 1] {
            d.extend(v.to_le_bytes());
        }
        let rows = super::parse(&d);
        let labels: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(labels, super::LABELS, "{rows:?}");
    }

    /// An EXIF block with strings, rationals, a short and the Exif directory.
    fn sample() -> Vec<u8> {
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(count.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // IFD0 at 8: three entries (2 + 3*12 + 4 = 42 bytes), then strings at 50.
        d.extend(3u16.to_le_bytes());
        entry(&mut d, 0x010f, 2, 6, 50);
        entry(&mut d, 0x0110, 2, 9, 56);
        entry(&mut d, 0x8769, 4, 1, 65);
        d.extend(0u32.to_le_bytes());
        d.extend(b"Canon\0Canon R6\0");
        // Exif IFD at 65: exposure 1/250 at 107, f/2.8 at 115, ISO 400 inline.
        d.extend(3u16.to_le_bytes());
        entry(&mut d, 0x829a, 5, 1, 107);
        entry(&mut d, 0x829d, 5, 1, 115);
        entry(&mut d, 0x8827, 3, 1, 400);
        d.extend(0u32.to_le_bytes());
        for v in [1u32, 250, 28, 10] {
            d.extend(v.to_le_bytes());
        }
        d
    }

    /// An Exif directory named by a pointer of type IFD (13), as libtiff writes it, is read for
    /// the panel and kept in a copy. Before, only SHORT and LONG pointers were followed, so the
    /// panel showed no date or exposure and a copy lost the whole Exif directory.
    #[test]
    fn follows_a_pointer_of_type_ifd() {
        let mut block = crate::support::test_support::CANON_EXIF.to_vec();
        block[37] = 13;
        let rows = super::parse(&block);
        assert!(
            rows.contains(&("Taken", "2020:01:02 03:04:05".into())),
            "{rows:?}"
        );
        let copy = super::derived_block(&block).unwrap();
        assert!(copy.ends_with(b"03:04:05\0"));
    }

    /// A copy's block drops the fields of IFD0 that describe the stored pixels of a RAW, such
    /// as its size, its SubIFDs and the DNG version, and keeps the camera and the date.
    #[test]
    fn a_copy_drops_the_fields_of_the_raw_pixels() {
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(count.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // IFD0 at 8: five entries (2 + 5*12 + 4 = 66 bytes), then the make at 74.
        let mut d = b"II*\0\x08\0\0\0".to_vec();
        d.extend(5u16.to_le_bytes());
        entry(&mut d, 0x0100, 3, 1, 4000);
        entry(&mut d, 0x010f, 2, 6, 74);
        entry(&mut d, 0x014a, 4, 1, 118);
        entry(&mut d, 0x8769, 4, 1, 80);
        entry(&mut d, 0xc612, 1, 4, 0x0104);
        d.extend(0u32.to_le_bytes());
        d.extend(b"Canon\0");
        // The Exif IFD at 80 with the date at 98, then the SubIFD at 118.
        d.extend(1u16.to_le_bytes());
        entry(&mut d, 0x9003, 2, 20, 98);
        d.extend(0u32.to_le_bytes());
        d.extend(b"2020:01:02 03:04:05\0");
        d.extend(1u16.to_le_bytes());
        entry(&mut d, 0x0100, 3, 1, 6000);
        d.extend(0u32.to_le_bytes());
        let copy = super::derived_block(&d).unwrap();
        let (_, ifd0, _, _) = super::ifds(&copy).unwrap();
        let tags: Vec<u16> = ifd0.iter().map(|f| f.0).collect();
        assert_eq!(tags, [0x010f, 0x8769]);
        let rows = super::parse(&copy);
        assert!(rows.contains(&("Camera", "Canon".into())), "{rows:?}");
        assert!(
            rows.contains(&("Taken", "2020:01:02 03:04:05".into())),
            "{rows:?}"
        );
    }

    /// A PNG's or WebP's EXIF that cannot be read stays as it is when the image is saved turned.
    #[test]
    fn keeps_unreadable_exif() {
        let mut block = b"Exif\0\0not a TIFF header".to_vec();
        assert_eq!(super::set_upright(&mut block, (1, 1)), Ok(()));
        assert_eq!(block, b"Exif\0\0not a TIFF header");
    }

    /// Turned upright: orientation 1, and no EXIF thumbnail (IFD1) left the old way up.
    #[test]
    fn sets_upright_without_thumbnail() {
        // IFD0 at 8 with the orientation 6, then IFD1 at 26.
        let mut block = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\
                          \x1a\0\0\0\0\0"
            .to_vec();
        super::set_upright(&mut block, (1, 1)).unwrap();
        let upright = image::metadata::Orientation::NoTransforms;
        assert_eq!(super::orientation(&block), Some(upright));
        assert_eq!(block[28..32], [0, 0, 0, 0]);
    }

    /// Turned upright or cropped, a copy keeps no EXIF thumbnail of the whole frame: its bytes
    /// are cleared, not only unlinked, as a crop shared was found to hold the camera's thumbnail
    /// of what was cut away.
    #[test]
    fn clears_the_exif_thumbnail() {
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(1u32.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // IFD0 at 8 with the orientation 6, IFD1 at 26 with a JPEG thumbnail at 56, 6 bytes.
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        d.extend(1u16.to_le_bytes());
        entry(&mut d, 0x0112, 3, 6);
        d.extend(26u32.to_le_bytes());
        d.extend(2u16.to_le_bytes());
        entry(&mut d, 0x0201, 4, 56);
        entry(&mut d, 0x0202, 4, 6);
        d.extend(0u32.to_le_bytes());
        d.extend(b"\xff\xd8\xaa\xbb\xff\xd9");
        super::set_upright(&mut d, (1, 1)).unwrap();
        assert!(!d.windows(2).any(|w| w == b"\xff\xd8"));
        assert_eq!(d.len(), 6 + 62);
        // An uncompressed thumbnail in two strips: both cleared. IFD1 at 26 with StripOffsets
        // (two LONGs at 0x38) and StripByteCounts (two SHORTs, inline), the strips at 0x40, 0x44.
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        d.extend(1u16.to_le_bytes());
        entry(&mut d, 0x0112, 3, 6);
        d.extend(26u32.to_le_bytes());
        d.extend(2u16.to_le_bytes());
        d.extend(0x0111u16.to_le_bytes());
        d.extend(4u16.to_le_bytes());
        d.extend(2u32.to_le_bytes());
        d.extend(0x38u32.to_le_bytes());
        d.extend(0x0117u16.to_le_bytes());
        d.extend(3u16.to_le_bytes());
        d.extend(2u32.to_le_bytes());
        d.extend([4, 0, 4, 0]);
        d.extend(0u32.to_le_bytes());
        d.extend(0x40u32.to_le_bytes());
        d.extend(0x44u32.to_le_bytes());
        d.extend(b"AAAABBBB");
        super::set_upright(&mut d, (1, 1)).unwrap();
        assert_eq!(&d[6 + 0x40..], [0; 8]);
    }

    /// A copy's EXIF says its own pixel sizes, not the original's, in a SHORT or a LONG field.
    #[test]
    fn sets_the_pixel_sizes_of_a_copy() {
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, value: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(kind.to_le_bytes());
            d.extend(1u32.to_le_bytes());
            d.extend(value.to_le_bytes());
        };
        // IFD0 at 8 with the Exif IFD at 26, which has the width as a SHORT, the height a LONG.
        let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        d.extend(1u16.to_le_bytes());
        entry(&mut d, super::EXIF_IFD, 4, 26);
        d.extend(0u32.to_le_bytes());
        d.extend(2u16.to_le_bytes());
        entry(&mut d, 0xa002, 3, 4000);
        entry(&mut d, 0xa003, 4, 3000);
        d.extend(0u32.to_le_bytes());
        super::set_upright(&mut d, (400, 70_000)).unwrap();
        let (t, _, exif, _) = super::ifds(&d).unwrap();
        let size = |tag| t.int(super::find(&exif, tag).unwrap());
        assert_eq!((size(0xa002), size(0xa003)), (Some(400), Some(70_000)));
        // A width too large for its SHORT is left as it was.
        super::set_upright(&mut d, (70_000, 50)).unwrap();
        let (t, _, exif, _) = super::ifds(&d).unwrap();
        let size = |tag| t.int(super::find(&exif, tag).unwrap());
        assert_eq!((size(0xa002), size(0xa003)), (Some(400), Some(50)));
    }

    /// A block for sharing is made new of the tags kept: the exposure kept, the camera's make
    /// and model are nowhere in it, and the orientation, which every copy keeps, is. A thumbnail
    /// in IFD1 comes along only when it is kept, and is then where the new IFD1 says.
    #[test]
    fn a_shared_block_holds_only_what_is_kept() {
        let mut block = sample();
        // Orientation 6 in place of the model, and IFD1 after the Exif directory's values, with
        // a JPEG thumbnail of four bytes (0x0201 at 160, 0x0202 of 4).
        block[6 + 22..6 + 34].copy_from_slice(&[0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        let ifd1 = block.len() as u32 - 6;
        block[6 + 46..6 + 50].copy_from_slice(&ifd1.to_le_bytes());
        block.extend(2u16.to_le_bytes());
        for (tag, value) in [(0x0201u16, ifd1 + 30), (0x0202, 4)] {
            block.extend(tag.to_le_bytes());
            block.extend(4u16.to_le_bytes());
            block.extend(1u32.to_le_bytes());
            block.extend(value.to_le_bytes());
        }
        block.extend([0; 4]);
        block.extend(b"THMB");
        let names = super::shared_tag_names(&block);
        assert!(names.contains(&"Make".into()) && names.contains(&super::EXIF_THUMBNAIL.into()));
        let keeps = |name: &str| name == "ExposureTime";
        let shared = super::shared_block(&block, &keeps, false).unwrap();
        let has = |b: &[u8], text: &[u8]| b.windows(text.len()).any(|w| w == text);
        assert!(!has(&shared, b"Canon") && !has(&shared, b"THMB"));
        let rows = super::parse(&shared);
        assert!(rows.contains(&("Exposure", "1/250 s".into())), "{rows:?}");
        assert_eq!(super::orientation(&shared).map(|o| o.to_exif()), Some(6));
        let keeps = |name: &str| name == super::EXIF_THUMBNAIL;
        let shared = super::shared_block(&block, &keeps, false).unwrap();
        let (t, ..) = super::ifds(&shared).unwrap();
        assert_eq!(super::exif_thumbnail(&t), Some(&b"THMB"[..]));
    }

    /// A block for sharing takes only the Interop directory's own tags, the index and its
    /// version, and never an entry of it that points at the GPS directory's text.
    #[test]
    fn a_shared_block_takes_only_interops_own_tags() {
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_be_bytes());
            d.extend(kind.to_be_bytes());
            d.extend(count.to_be_bytes());
            d.extend(value.to_be_bytes());
        };
        // IFD0 at 8 to 38, Exif at 38 to 56, Interop at 56 to 98, GPS at 98 to 116, and the
        // map datum at 116.
        let mut d = b"MM\0*\0\0\0\x08".to_vec();
        d.extend(2u16.to_be_bytes());
        entry(&mut d, super::EXIF_IFD, 4, 1, 38);
        entry(&mut d, super::GPS_IFD, 4, 1, 98);
        d.extend(0u32.to_be_bytes());
        d.extend(1u16.to_be_bytes());
        entry(&mut d, super::INTEROP_IFD, 4, 1, 56);
        d.extend(0u32.to_be_bytes());
        d.extend(3u16.to_be_bytes());
        entry(&mut d, 0x0001, 2, 4, u32::from_be_bytes(*b"R98\0"));
        entry(&mut d, 0x0002, 7, 14, 116);
        entry(&mut d, 0x1000, 2, 14, 116);
        d.extend(0u32.to_be_bytes());
        d.extend(1u16.to_be_bytes());
        entry(&mut d, 0x0012, 2, 14, 116);
        d.extend(0u32.to_be_bytes());
        d.extend(b"WGS-84-SECRET\0");
        let shared = super::shared_block(&d, &|_| false, false).unwrap();
        let has = |text: &[u8]| shared.windows(text.len()).any(|w| w == text);
        assert!(has(b"R98\0") && !has(b"WGS-84-SECRET"));
    }

    /// Blocks and a JPEG broken in thousands of ways, as a damaged or crafted file has them:
    /// read without a panic, cut for a copy within the block, and turned upright without
    /// writing past the block.
    #[test]
    fn survives_broken_blocks() {
        // Many text fields that all name the same large part of the block: each is read only up
        // to MAX_TEXT, so they come to little, not to their count times the part's size.
        let n = 200u16;
        let blob = vec![b'a'; 100_000];
        let at = 8 + 2 + 12 * n as u32 + 4;
        let mut big = b"Exif\0\0MM\0*\0\0\0\x08".to_vec();
        big.extend(n.to_be_bytes());
        for tag in 0..n {
            big.extend((0x1000 + tag).to_be_bytes());
            big.extend(super::ASCII.to_be_bytes());
            big.extend((blob.len() as u32).to_be_bytes());
            big.extend(at.to_be_bytes());
        }
        big.extend([0; 4]);
        big.extend(&blob);
        let read: usize = super::fields(&big).iter().map(|f| f.2.len()).sum();
        assert!(read > 0 && read <= n as usize * super::MAX_TEXT, "{read}");
        // A copy for sharing takes no more value bytes than the block holds. Before, it copied
        // the part once for each field, which a block of 64 MB made 64 GB.
        let shared = super::shared_block(&big, &|_| true, false);
        assert!(shared.is_none_or(|b| b.len() <= big.len()));
        // A tag named twice is taken once, the first time.
        let twice = b"MM\0*\0\0\0\x08\0\x02\x01\x0f\0\x02\0\0\0\x02A\0\0\0\
                      \x01\x0f\0\x02\0\0\0\x02B\0\0\0\0\0\0\0";
        let shared = super::shared_block(twice, &|_| true, false).unwrap();
        let makes: Vec<_> = (super::fields(&shared).into_iter())
            .filter(|f| f.0 == "Image" && f.1 == "Make")
            .collect();
        assert_eq!(makes.len(), 1, "{makes:?}");
        let mm = b"Exif\0\0MM\0*\0\0\0\x08\0\x02\x01\x0f\0\x02\0\0\0\x06\0\0\0\x26\
                   \x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0Canon\0";
        for seed in [sample(), mm.to_vec()] {
            for mut block in crate::support::test_support::broken(&seed) {
                super::shared_tag_names(&block);
                super::shared_block(&block, &|_| true, true);
                super::shared_block(&block, &|_| false, true);
                super::parse(&block);
                super::fields(&block);
                super::orientation(&block);
                super::is_adobe_rgb(&block);
                let len = block.len();
                let copy = super::derived_block(&block);
                assert!(copy.is_none_or(|c| c.len() <= len));
                let _ = super::set_upright(&mut block, (1, 1));
                assert_eq!(block.len(), len);
            }
        }
        // An iPhone's maker note of only the HDR headroom and gain is made again as it is, and
        // a broken one gives at most as much.
        let apple = b"Apple iOS\0\0\x01MM\0\x02\0\x21\0\x0a\0\0\0\x01\0\0\0\x2c\
                      \0\x30\0\x0a\0\0\0\x01\0\0\0\x34\0\0\0\0\
                      \0\0\0\x03\0\0\0\x02\0\0\0\x01\0\0\0\x04";
        assert_eq!(
            super::apple_hdr_maker_note(apple).as_deref(),
            Some(&apple[..])
        );
        for note in crate::support::test_support::broken(apple) {
            let new = super::apple_hdr_maker_note(&note);
            assert!(new.is_none_or(|n| n.len() <= apple.len()));
        }
        let mut jpeg = Vec::new();
        let mut out = std::io::Cursor::new(&mut jpeg);
        image::RgbImage::new(4, 2)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        // Also with an EXIF segment holding the orientation, which is changed where it is.
        let mut with_exif = jpeg[..2].to_vec();
        with_exif.extend([0xff, 0xe1, 0, mm.len() as u8 + 2]);
        with_exif.extend(mm);
        with_exif.extend(&jpeg[2..]);
        assert!(super::oriented(with_exif.clone(), 1, false, true).is_ok());
        // An orientation of 9 or 257, which the view shows unturned, is turned from 1: a
        // quarter turn clockwise saves 6.
        for value in [9u16, 257] {
            let mut odd = with_exif.clone();
            let at = (odd.windows(4))
                .position(|w| w == b"\x01\x12\0\x03")
                .unwrap()
                + 8;
            odd[at..at + 2].copy_from_slice(&value.to_be_bytes());
            assert_eq!(
                super::oriented(odd, 1, false, true).unwrap().1,
                6,
                "{value}"
            );
        }
        for data in crate::support::test_support::broken(&jpeg)
            .chain(crate::support::test_support::broken(&with_exif))
        {
            if let Some((segments, _)) = super::jpeg_segments(&data) {
                segments.iter().for_each(|s| {
                    let _ = super::segment(&data, s);
                });
            }
            let _ = super::oriented(data, 1, false, true);
        }
    }
}
