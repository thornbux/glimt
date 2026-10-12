//! The EXIF panel's fields. The image decoder hands over the raw EXIF block (JPEG, PNG, WebP,
//! TIFF); this walks its TIFF structure for the handful of tags a photographer looks at.

use std::path::Path;

/// (label, value) rows for the panel, in display order; empty if the file has no EXIF.
pub fn read(path: &Path) -> Vec<(&'static str, String)> {
    raw(path).map_or_else(Vec::new, |raw| parse(&raw))
}

/// Every field of IFD0, the Exif and the GPS directory: (directory, name, value), for the EXIF
/// window. Unknown tags are shown by number.
pub fn all(path: &Path) -> Vec<(&'static str, String, String)> {
    let Some(raw) = raw(path) else {
        return Vec::new();
    };
    let Some((t, ifd0, exif, gps)) = ifds(&raw) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (group, ifd) in [("Image", &ifd0), ("Exif", &exif), ("GPS", &gps)] {
        for &(tag, e) in ifd.iter() {
            // The links to the other directories are not fields of their own.
            if group != "GPS" && matches!(tag, 0x8769 | 0x8825 | 0xa005) {
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
    let raw = raw(path)?;
    let (t, _, exif, _) = ifds(&raw)?;
    let tag = if digitized { 0x9004 } else { 0x9003 };
    t.text(exif.iter().find(|(t2, _)| *t2 == tag)?.1)
}

/// The Orientation tag of a TIFF block (a TIFF-based RAW file's header).
pub fn orientation(raw: &[u8]) -> Option<image::metadata::Orientation> {
    let (t, ifd0, _, _) = ifds(raw)?;
    let e = ifd0.iter().find(|(tag, _)| *tag == 0x0112)?.1;
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
/// file is replaced in one step (a new file renamed over it), keeping its permissions.
pub fn write_orientation(path: &Path, turns: u8, mirrored: bool) -> Result<(), String> {
    let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err("only JPEG files can store the orientation".into());
    }
    // The segments before the image data: (marker, start of its data, length of its data).
    let mut segments = Vec::new();
    let mut i = 2;
    while let (Some(&0xff), Some(&marker)) = (data.get(i), data.get(i + 1)) {
        if marker == 0xda {
            break;
        }
        let len = u16::from_be_bytes([
            *data.get(i + 2).ok_or("cut off")?,
            *data.get(i + 3).ok_or("cut off")?,
        ]) as usize;
        segments.push((marker, i + 4, len.saturating_sub(2)));
        i += 2 + len;
    }
    let exif = segments
        .iter()
        .find(|s| s.0 == 0xe1 && data[s.1..].starts_with(b"Exif\0\0"));
    match exif {
        Some(&(_, start, len)) => {
            let block = data.get(start..start + len).ok_or("cut off")?;
            let (t, ifd0, _, _) = ifds(block).ok_or("unreadable EXIF")?;
            let little = t.little;
            let found = ifd0.iter().find(|(tag, _)| *tag == 0x0112).map(|e| e.1);
            match found {
                Some(e) if e.kind == 3 => {
                    let new = turned(t.int(e).unwrap_or(1) as u8, turns, mirrored) as u16;
                    // Offsets in the block count from the TIFF header, after "Exif\0\0".
                    let at = start + 6 + e.at;
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
                    let new = turned(1, turns, mirrored) as u16;
                    add_orientation(&mut data, start, len, little, new)?;
                }
            }
        }
        None => {
            let new = turned(1, turns, mirrored) as u16;
            let mut app1 = vec![0xff, 0xe1, 0, 34];
            app1.extend(b"Exif\0\0MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
            app1.extend(new.to_be_bytes());
            app1.extend([0, 0, 0, 0, 0, 0]);
            // After a JFIF segment if there is one, else right after the start marker.
            let at = match segments.first() {
                Some(&(0xe0, start, len)) => start + len,
                _ => 2,
            };
            data.splice(at..at, app1);
        }
    }
    let tmp = crate::tmp_path(path);
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Set the orientation in an EXIF block, as PNG and WebP keep it, to 1: for pixels that were
/// turned upright. A block without the field is left as it is.
pub fn set_upright(block: &mut [u8]) -> Result<(), String> {
    let (t, ifd0, _, _) = ifds(block).ok_or("unreadable EXIF")?;
    let Some(e) = ifd0.iter().find(|(tag, _)| *tag == 0x0112).map(|e| e.1) else {
        return Ok(());
    };
    if e.kind != 3 {
        return Err("unusual orientation field".into());
    }
    // Offsets count from the TIFF header, after any "Exif\0\0" before it.
    let at = block.len() - t.data.len() + e.at;
    let one = if t.little {
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
    let read = Tiff { data, little };
    let ifd = tiff + read.u32(tiff + 4).ok_or("cut off")? as usize;
    let n = read.u16(ifd).ok_or("cut off")? as usize;
    let entries = data.get(ifd + 2..ifd + 2 + 12 * n).ok_or("cut off")?;
    let mut list: Vec<Vec<u8>> = entries.chunks(12).map(<[u8]>::to_vec).collect();
    let next = data
        .get(ifd + 2 + 12 * n..ifd + 6 + 12 * n)
        .ok_or("cut off")?
        .to_vec();
    let mut entry = Vec::new();
    entry.extend(p16(0x0112));
    entry.extend(p16(3));
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
    if crate::has_ext(path, crate::raw::EXTS) {
        return crate::raw::exif(path);
    }
    #[cfg(feature = "jxl")]
    if crate::has_ext(path, crate::JXL_EXTS) {
        let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
        let mut dec = jxl_oxide::integration::JxlDecoder::new(file).ok()?;
        return dec.exif_metadata().ok().flatten();
    }
    // HEIC and AVIF keep the EXIF block as an item of its own, "Exif\0\0" and a TIFF header.
    if crate::is_heif(path) {
        let data = std::fs::read(path).ok()?;
        let at = data
            .windows(8)
            .position(|w| w.starts_with(b"Exif\0\0") && matches!(&w[6..], b"II" | b"MM"))?;
        return Some(data[at..].to_vec());
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
        sub(0x8769).unwrap_or_default(),
        sub(0x8825).unwrap_or_default(),
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
                    3 => 2,
                    4 | 9 => 4,
                    5 | 10 => 8,
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
            2 => self.text(e).unwrap_or_default(),
            1 | 7 => {
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
            3 => list(
                (0..n)
                    .filter_map(|i| self.u16(e.at + 2 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            4 | 9 => list(
                (0..n)
                    .filter_map(|i| self.u32(e.at + 4 * i))
                    .map(|v| v.to_string())
                    .collect(),
            ),
            5 | 10 => list(
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
            3 => self.u16(e.at).map(u32::from),
            4 | 9 => self.u32(e.at),
            _ => None,
        }
    }

    /// Rational number `i` of the entry (signed for SRATIONAL).
    fn ratio(&self, e: Entry, i: usize) -> Option<f64> {
        let at = e.at + i * 8;
        let (n, d) = (self.u32(at)?, self.u32(at + 4)?);
        let (n, d) = if e.kind == 10 {
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
    let get = |ifd: &[(u16, Entry)], tag: u16| ifd.iter().find(|(t2, _)| *t2 == tag).map(|e| e.1);

    let mut rows = Vec::new();
    let make = get(&ifd0, 0x010f).and_then(|e| t.text(e));
    let model = get(&ifd0, 0x0110).and_then(|e| t.text(e));
    let camera = match (make, model) {
        // Most models already start with the make ("Canon EOS R6").
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (make, model) => make.or(model),
    };
    rows.extend(camera.map(|c| ("Camera", c)));
    rows.extend(
        get(&exif, 0xa434)
            .and_then(|e| t.text(e))
            .map(|l| ("Lens", l)),
    );
    let taken = get(&exif, 0x9003).or_else(|| get(&ifd0, 0x0132));
    rows.extend(taken.and_then(|e| t.text(e)).map(|d| ("Taken", d)));
    if let Some(s) = get(&exif, 0x829a).and_then(|e| t.ratio(e, 0)) {
        let text = if s > 0.0 && s < 1.0 {
            format!("1/{} s", (1.0 / s).round())
        } else {
            format!("{s} s")
        };
        rows.push(("Exposure", text));
    }
    rows.extend(
        get(&exif, 0x829d)
            .and_then(|e| t.ratio(e, 0))
            .map(|f| ("Aperture", format!("f/{f:.1}"))),
    );
    rows.extend(
        get(&exif, 0x8827)
            .and_then(|e| t.int(e))
            .map(|iso| ("ISO", iso.to_string())),
    );
    if let Some(mm) = get(&exif, 0x920a).and_then(|e| t.ratio(e, 0)) {
        // One decimal, none for whole numbers: "2.8 mm", "24 mm".
        let mm = (mm * 10.0).round() / 10.0;
        let full_frame = get(&exif, 0xa405).and_then(|e| t.int(e)).filter(|&f| f > 0);
        let text = match full_frame {
            Some(f) => format!("{mm} mm (35 mm: {f} mm)"),
            None => format!("{mm} mm"),
        };
        rows.push(("Focal length", text));
    }
    if let Some(ev) = get(&exif, 0x9204).and_then(|e| t.ratio(e, 0)) {
        rows.push(("Exposure bias", format!("{ev:+.1} EV")));
    }
    if let Some(flash) = get(&exif, 0x9209).and_then(|e| t.int(e)) {
        rows.push(("Flash", if flash & 1 == 1 { "Fired" } else { "No" }.into()));
    }
    // Degrees, minutes, seconds; negative for south and west.
    let coord = |value: u16, reference: u16, negative: &str| -> Option<f64> {
        let e = get(&gps, value)?;
        let deg = t.ratio(e, 0)? + t.ratio(e, 1)? / 60.0 + t.ratio(e, 2)? / 3600.0;
        let sign = get(&gps, reference)
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

    /// A JPEG without EXIF gets one; turning again changes the field in place.
    #[test]
    fn writes_orientation() {
        let dir = std::env::temp_dir().join(format!("glimt-orient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        image::RgbImage::new(4, 2).save(&file).unwrap();
        super::write_orientation(&file, 1, false).unwrap();
        let o = |f: &std::path::Path| {
            use image::ImageDecoder;
            let mut d = image::ImageReader::open(f).unwrap().into_decoder().unwrap();
            d.orientation().unwrap()
        };
        assert_eq!(o(&file), image::metadata::Orientation::Rotate90);
        super::write_orientation(&file, 1, false).unwrap();
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
        super::write_orientation(&file, 3, false).unwrap();
        assert_eq!(o(&file), image::metadata::Orientation::Rotate270);
        let rows = super::read(&file);
        assert!(rows.contains(&("Camera", "Canon".to_string())), "{rows:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A little-endian TIFF block: IFD0 with Make, Model and an Exif IFD with exposure,
    /// aperture and ISO.
    #[test]
    fn parses_common_fields() {
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
}
