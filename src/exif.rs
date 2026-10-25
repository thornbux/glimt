//! EXIF: a minimal reader of its TIFF structure (from JPEG, PNG, WebP and TIFF) for the panel's
//! fields a photographer looks at, every field for the EXIF window, the dates that sorting by
//! EXIF date reads, and the orientation, which turning a JPEG writes.

use std::path::Path;

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
    raw(path).map_or_else(Vec::new, |raw| parse(&raw))
}

/// A field of the EXIF window: (directory, name, value).
pub type Field = (&'static str, String, String);

/// `read` of `path`, and every field of IFD0, the Exif and the GPS directory for the EXIF
/// window (unknown tags by number), from one reading of its EXIF block: on a network share
/// each reading of a large photo costs.
pub fn read_and_all(path: &Path) -> (Vec<(&'static str, String)>, Vec<Field>) {
    raw(path).map_or_else(Default::default, |raw| (parse(&raw), fields(&raw)))
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
        0x0213 => "YCbCrPositioning",
        0x8298 => "Copyright",
        0x829a => "ExposureTime",
        0x829d => "FNumber",
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
        _ => return None,
    })
}

/// DateTimeOriginal (or DateTimeDigitized) as "YYYY:MM:DD HH:MM:SS", which sorts as text.
pub fn date(path: &Path, digitized: bool) -> Option<String> {
    // A decoder panicking on one file must not end the sort or the Pan view that asked.
    std::panic::catch_unwind(|| {
        let raw = raw(path)?;
        let (t, _, exif, _) = ifds(&raw)?;
        let tag = if digitized { 0x9004 } else { 0x9003 };
        t.text(exif.iter().find(|(t2, _)| *t2 == tag)?.1)
    })
    .ok()
    .flatten()
}

/// Whether the EXIF block says the pixels are Adobe RGB without an ICC profile, as cameras set
/// to Adobe RGB do: ColorSpace 2, or "uncalibrated" with the Interoperability index R03 (R98 is
/// sRGB). Read as Geeqie reads it.
pub fn adobe_rgb(raw: &[u8]) -> bool {
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

fn mul(a: [i8; 4], b: [i8; 4]) -> [i8; 4] {
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
        m = mul([-1, 0, 0, 1], m);
    }
    for _ in 0..turns % 4 {
        m = mul([0, -1, 1, 0], m);
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
/// (`rotate`), else the pixels as stored.
pub fn write_orientation(
    path: &Path,
    turns: u8,
    mirrored: bool,
    rotate: bool,
) -> Result<(), String> {
    // Through a symlink, the file it points to: the new file is renamed over that, not the link.
    let path = &std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let data = crate::decode::read_limited(path).map_err(|e| e.to_string())?;
    crate::config::replace_existing(path, oriented(data, turns, mirrored, rotate)?)
        .map_err(|e| e.to_string())
}

/// The JPEG `data` with the view's rotation as its EXIF orientation, as `write_orientation`
/// writes it.
fn oriented(mut data: Vec<u8>, turns: u8, mirrored: bool, rotate: bool) -> Result<Vec<u8>, String> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err("only JPEG files can store the orientation".into());
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
            let little = t.little;
            let found = find(&ifd0, ORIENTATION);
            match found {
                // One value, so it sits in the entry itself, inside the block.
                Some(e) if e.kind == SHORT && e.count == 1 => {
                    // Read first, which keeps the write inside the block: an entry at its very end
                    // has no room for its value.
                    let old = t.u16(e.at).ok_or("cut off")?;
                    let shown = if rotate { old as u8 } else { 1 };
                    new = turned(shown, turns, mirrored) as u16;
                    // Offsets count from the TIFF header, which ends the block as `t.data` does.
                    let at = start + len - t.data.len() + e.at;
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
        set_xmp_orientation(&mut data[start..end], new as u8);
    }
    Ok(data)
}

/// The XMP's `tiff:Orientation` set to `value` (1–8), as an attribute or an element; the length
/// stays, as a value is one digit.
pub fn set_xmp_orientation(xmp: &mut [u8], value: u8) {
    let key = b"tiff:Orientation";
    let mut from = 0;
    while let Some(at) = xmp[from..].windows(key.len()).position(|w| w == key) {
        let rest = from + at + key.len();
        let spot = match xmp.get(rest..rest + 2) {
            Some([b'=', b'"' | b'\'']) => rest + 2,
            Some([b'>', _]) => rest + 1,
            _ => rest,
        };
        let digit = xmp.get(spot).is_some_and(u8::is_ascii_digit);
        if digit && !xmp.get(spot + 1).is_some_and(u8::is_ascii_digit) {
            xmp[spot] = b'0' + value.min(9);
        }
        from = rest;
    }
}

/// Set the orientation in an EXIF block, as PNG and WebP keep it, to 1: for pixels that were
/// turned upright. A block without the field is left as it is, and so is one without a TIFF
/// header: no program finds an orientation in it to turn the pixels again. Also clears the EXIF
/// thumbnail (IFD1), which a copy must not keep: of a crop it shows the whole frame, so every
/// copy goes through here, upright or not.
pub fn set_upright(block: &mut [u8]) -> Result<(), String> {
    let Some((t, ifd0, _, _)) = ifds(block) else {
        return Ok(());
    };
    let found = find(&ifd0, ORIENTATION);
    if found.is_some_and(|e| e.kind != SHORT) {
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
            let size = if e.kind == SHORT { 2 } else { 4 };
            t.int(Entry {
                at: e.at + i * size,
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
    let little = t.little;
    for (at, len) in thumbnail.into_iter().flatten() {
        let start = head + at as usize;
        if let Some(bytes) = block.get_mut(start..start.saturating_add(len as usize)) {
            bytes.fill(0);
        }
    }
    if let Some(n) = next.and_then(|n| block.get_mut(head + n..head + n + 4)) {
        n.fill(0);
    }
    let Some(e) = found else {
        return Ok(());
    };
    let at = head + e.at;
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

/// JPEG segments: (marker, start of its data, length of its data).
type Segments = Vec<(u8, usize, usize)>;

/// The segments of a JPEG before its image data, and whether the image data was reached; `None`
/// if a segment's length is cut off.
fn jpeg_segments(data: &[u8]) -> Option<(Segments, bool)> {
    let mut segments = Vec::new();
    let mut i = 2;
    while let (Some(&0xff), Some(&marker)) = (data.get(i), data.get(i + 1)) {
        // Fill bytes: a marker may follow any number of 0xff.
        if marker == 0xff {
            i += 1;
            continue;
        }
        if marker == 0xda {
            return Some((segments, true));
        }
        let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
        segments.push((marker, i + 4, len.saturating_sub(2)));
        i += 2 + len;
    }
    Some((segments, false))
}

/// Where a JPEG's image data begins (its first SOS marker), past the segments before it and
/// what they hold, such as the EXIF thumbnail, which is a JPEG of its own; `None` if not reached.
pub fn jpeg_scan_start(data: &[u8]) -> Option<usize> {
    let (segments, reached) = jpeg_segments(data)?;
    reached.then(|| segments.last().map_or(2, |&(_, start, len)| start + len))
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
    let head = crate::decode::read_head(path, 1 << 18).ok()?;
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
        little,
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
    list.sort_by_key(|e| Tiff { data: e, little }.u16(0).unwrap_or(0));
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

fn raw(path: &Path) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    if crate::files::has_ext(path, crate::raw::EXTS) {
        return crate::raw::exif(path);
    }
    #[cfg(feature = "jxl")]
    if crate::files::has_ext(path, crate::files::JXL_EXTS) {
        let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
        let mut dec = jxl_oxide::integration::JxlDecoder::new(file).ok()?;
        return dec.exif_metadata().ok().flatten();
    }
    // HEIC and AVIF keep the EXIF block as an item of its own, "Exif\0\0" and a TIFF header.
    // From the first MB, where it usually is, before the whole file; a MB of it at most.
    if crate::external::is_heif(path) {
        let find = |data: &[u8]| {
            let at = (data.windows(8))
                .position(|w| w.starts_with(b"Exif\0\0") && matches!(&w[6..], b"II" | b"MM"))?;
            Some(data[at..data.len().min(at + (1 << 20))].to_vec())
        };
        let head = crate::decode::read_head(path, 1 << 20).ok()?;
        // Found too near the head's end, it may be cut short (EXIF blocks reach 64 KB).
        let whole = head.len() < 1 << 20;
        let in_head = find(&head).filter(|e| whole || e.len() > 1 << 16);
        // Read whole only when the head was not all of it, and never more than a decode would.
        let rest = || find(&crate::decode::read_limited(path).ok()?);
        return in_head.or_else(|| (!whole).then(rest).flatten());
    }
    if crate::files::is_jpeg(path)
        && let Some(exif) = jpeg_app1(path, b"Exif\0\0")
    {
        return exif;
    }
    image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()?
        .into_decoder()
        .ok()?
        .exif_metadata()
        .ok()
        .flatten()
}

type Ifd = Vec<(u16, Entry)>;

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
    let t = Tiff { data, little };
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

struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}

/// A tag's type, count and where its value starts.
#[derive(Clone, Copy)]
struct Entry {
    kind: u16,
    count: u32,
    at: usize,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
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
                let size = match kind {
                    SHORT => 2,
                    LONG | SLONG => 4,
                    RATIONAL | SRATIONAL => 8,
                    _ => 1,
                };
                // Values of up to four bytes are stored in the entry itself.
                let at = if size * count as usize <= 4 {
                    e + 8
                } else {
                    self.u32(e + 8)? as usize
                };
                Some((tag, Entry { kind, count, at }))
            })
            .collect()
    }

    fn text(&self, e: Entry) -> Option<String> {
        let bytes = self.data.get(e.at..e.at + e.count as usize)?;
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
        match e.kind {
            ASCII => self.text(e).unwrap_or_default(),
            BYTE | UNDEFINED => {
                let bytes = self
                    .data
                    .get(e.at..e.at + (e.count as usize).min(256))
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
                    .filter_map(|i| self.u16(e.at + 2 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            LONG | SLONG => list(
                (0..n)
                    .filter_map(|i| self.u32(e.at + 4 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            RATIONAL | SRATIONAL => list(
                (0..n)
                    .filter_map(|i| self.ratio(e, i))
                    .map(|v| format!("{}", (v * 10000.0).round() / 10000.0))
                    .collect(),
            ),
            _ => format!("({} values)", e.count),
        }
    }

    fn int(&self, e: Entry) -> Option<u32> {
        match e.kind {
            SHORT => self.u16(e.at).map(u32::from),
            LONG | SLONG => self.u32(e.at),
            _ => None,
        }
    }

    /// Rational number `i` of the entry (signed for SRATIONAL).
    fn ratio(&self, e: Entry, i: usize) -> Option<f64> {
        let at = e.at + i * 8;
        let (n, d) = (self.u32(at)?, self.u32(at + 4)?);
        let (n, d) = if e.kind == SRATIONAL {
            (n as i32 as f64, d as i32 as f64)
        } else {
            (n as f64, d as f64)
        };
        (d != 0.0).then(|| n / d)
    }
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
    if let Some(s) = find(&exif, 0x829a).and_then(|e| t.ratio(e, 0)) {
        rows.push(("Exposure", exposure(s)));
    }
    rows.extend(
        find(&exif, 0x829d)
            .and_then(|e| t.ratio(e, 0))
            .map(|f| ("Aperture", format!("f/{f:.1}"))),
    );
    rows.extend(
        find(&exif, 0x8827)
            .and_then(|e| t.int(e))
            .map(|iso| ("ISO", iso.to_string())),
    );
    if let Some(mm) = find(&exif, 0x920a).and_then(|e| t.ratio(e, 0)) {
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
    if let Some(ev) = find(&exif, 0x9204).and_then(|e| t.ratio(e, 0)) {
        rows.push(("Exposure bias", format!("{ev:+.1} EV")));
    }
    if let Some(flash) = find(&exif, 0x9209).and_then(|e| t.int(e)) {
        rows.push(("Flash", if flash & 1 == 1 { "Fired" } else { "No" }.into()));
    }
    // Degrees, minutes, seconds; negative for south and west.
    let coord = |value: u16, reference: u16, negative: &str| -> Option<f64> {
        let e = find(&gps, value)?;
        let deg = t.ratio(e, 0)? + t.ratio(e, 1)? / 60.0 + t.ratio(e, 2)? / 3600.0;
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

#[cfg(test)]
mod tests {
    /// A damaged HEIC's EXIF block is found or not, never a panic.
    #[test]
    fn survives_broken_heif_heads() {
        let dir = std::env::temp_dir().join(format!("glimt-test-heif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.heic");
        let seed = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\x10Exif\0\0MM\0*\0\0\0\x08\0\0";
        for bytes in crate::broken(seed) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::raw(&file);
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
        let mut xmp =
            br#"<x tiff:Orientation="6" a="6"/><tiff:Orientation>8</tiff:Orientation>"#.to_vec();
        super::set_xmp_orientation(&mut xmp, 1);
        assert_eq!(
            xmp,
            br#"<x tiff:Orientation="1" a="6"/><tiff:Orientation>1</tiff:Orientation>"#
        );
        let mut odd = b"tiff:Orientation='12' tiff:Orientation".to_vec();
        super::set_xmp_orientation(&mut odd, 1);
        assert_eq!(odd, b"tiff:Orientation='12' tiff:Orientation");
    }

    #[test]
    fn survives_broken_xmp_orientation() {
        for mut xmp in
            crate::broken(br#"<x tiff:Orientation="6"/><tiff:Orientation>8</tiff:Orientation>"#)
        {
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
        assert!(super::adobe_rgb(&block(2, b"R98\0")));
        assert!(super::adobe_rgb(&block(0xffff, b"R03\0")));
        assert!(!super::adobe_rgb(&block(0xffff, b"R98\0")));
        assert!(!super::adobe_rgb(&block(1, b"R03\0")));
        assert!(!super::adobe_rgb(b"junk"));
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
        let xmp = b"http://ns.adobe.com/xap/1.0/\0<x tiff:Orientation=\"1\"/>";
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

    /// A PNG's or WebP's EXIF that cannot be read stays as it is when the image is saved turned.
    #[test]
    fn keeps_unreadable_exif() {
        let mut block = b"Exif\0\0not a TIFF header".to_vec();
        assert_eq!(super::set_upright(&mut block), Ok(()));
        assert_eq!(block, b"Exif\0\0not a TIFF header");
    }

    /// Turned upright: orientation 1, and no EXIF thumbnail (IFD1) left the old way up.
    #[test]
    fn sets_upright_without_thumbnail() {
        // IFD0 at 8 with the orientation 6, then IFD1 at 26.
        let mut block = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\
                          \x1a\0\0\0\0\0"
            .to_vec();
        super::set_upright(&mut block).unwrap();
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
        super::set_upright(&mut d).unwrap();
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
        super::set_upright(&mut d).unwrap();
        assert_eq!(&d[6 + 0x40..], [0; 8]);
    }

    /// Blocks and a JPEG broken in thousands of ways, as a damaged or crafted file has them:
    /// read without a panic, and turned upright without writing past the block.
    #[test]
    fn survives_broken_blocks() {
        let mm = b"Exif\0\0MM\0*\0\0\0\x08\0\x02\x01\x0f\0\x02\0\0\0\x06\0\0\0\x26\
                   \x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0Canon\0";
        for seed in [sample(), mm.to_vec()] {
            for mut block in crate::broken(&seed) {
                super::parse(&block);
                super::fields(&block);
                super::orientation(&block);
                super::adobe_rgb(&block);
                let len = block.len();
                let _ = super::set_upright(&mut block);
                assert_eq!(block.len(), len);
            }
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
        for data in crate::broken(&jpeg).chain(crate::broken(&with_exif)) {
            if let Some((segments, _)) = super::jpeg_segments(&data) {
                segments.iter().for_each(|s| drop(super::segment(&data, s)));
            }
            let _ = super::oriented(data, 1, false, true);
        }
    }
}
