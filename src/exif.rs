//! The EXIF panel's fields. The image decoder hands over the raw EXIF block (JPEG, PNG, WebP,
//! TIFF); this walks its TIFF structure for the handful of tags a photographer looks at.

use std::path::Path;

/// (label, value) rows for the panel, in display order; empty if the file has no EXIF.
pub fn read(path: &Path) -> Vec<(&'static str, String)> {
    raw(path).map_or_else(Vec::new, |raw| parse(&raw))
}

/// DateTimeOriginal (or DateTimeDigitized) as "YYYY:MM:DD HH:MM:SS", which sorts as text.
pub fn date(path: &Path, digitized: bool) -> Option<String> {
    let raw = raw(path)?;
    let (t, _, exif, _) = ifds(&raw)?;
    let tag = if digitized { 0x9004 } else { 0x9003 };
    t.text(exif.iter().find(|(t2, _)| *t2 == tag)?.1)
}

fn raw(path: &Path) -> Option<Vec<u8>> {
    use image::ImageDecoder;
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
