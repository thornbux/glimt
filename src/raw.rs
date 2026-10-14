//! Camera RAW files, shown by the JPEG preview the camera stores in them, as Geeqie does by
//! default: fast, and it looks as the camera showed it. The largest JPEG in the file is used,
//! which works for TIFF-based RAW (CR2, NEF, ARW, DNG, ORF, PEF, RW2 …) as well as RAF and CR3
//! without parsing each format.

use image::ImageDecoder;
use std::path::Path;

pub const EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "rw2", "raf", "pef",
    "srw", "erf", "kdc", "dcr", "3fr", "fff", "iiq", "mef", "mos", "mrw", "rwl", "x3f",
];

/// The preview, turned upright if `rotate`.
pub fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let jpeg = preview(&data).ok_or("no preview image in the RAW file")?;
    let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(jpeg))
        .map_err(|e| e.to_string())?;
    crate::decode::check_size(&dec)?;
    // TIFF-based RAW files carry the orientation in their own header; others in the JPEG.
    let orientation = crate::exif::orientation(&data)
        .or_else(|| dec.orientation().ok())
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    if rotate {
        img.apply_orientation(orientation);
    }
    Ok(img)
}

/// The EXIF block: the file itself if it is TIFF-based (its header is enough), else the one
/// in the preview JPEG.
pub fn exif(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut head = Vec::new();
    let file = std::fs::File::open(path).ok()?;
    file.take(1 << 20).read_to_end(&mut head).ok()?;
    if matches!(head.get(..2), Some(b"II" | b"MM")) {
        return Some(head);
    }
    let data = std::fs::read(path).ok()?;
    let jpeg = preview(&data)?;
    let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(jpeg)).ok()?;
    dec.exif_metadata().ok().flatten()
}

/// The largest JPEG in `data` that the decoder handles: baseline or progressive. Lossless JPEG
/// (raw sensor data in DNG and CR2) is passed over.
pub fn preview(data: &[u8]) -> Option<&[u8]> {
    // Find the start markers first, then look at them: one tight loop is ~7× faster through
    // a 20 MB file (15 ms) than reading the headers along the way.
    let starts: Vec<usize> = (1..data.len().saturating_sub(1))
        .filter(|&i| data[i] == 0xd8 && data[i - 1] == 0xff && data[i + 1] == 0xff)
        .collect();
    starts
        .into_iter()
        .filter_map(|i| {
            let (w, h) = jpeg_size(&data[i - 1..])?;
            Some((w as u64 * h as u64, &data[i - 1..]))
        })
        .max_by_key(|f| f.0)
        .map(|f| f.1)
}

/// Width and height of a baseline or progressive JPEG from its frame header, by walking the
/// segments after the start marker.
pub fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let be = |i: usize| Some(u16::from_be_bytes([*jpeg.get(i)?, *jpeg.get(i + 1)?]) as usize);
    let mut i = 2;
    loop {
        if *jpeg.get(i)? != 0xff {
            return None;
        }
        match *jpeg.get(i + 1)? {
            0xc0..=0xc2 => return Some((be(i + 7)? as u32, be(i + 5)? as u32)),
            // Other frame types (lossless, arithmetic), or image data before any frame.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xda => return None,
            _ => i += 2 + be(i + 2)?,
        }
    }
}

#[cfg(test)]
mod tests {
    /// A made-up RAW file: a header, a small JPEG, lossless sensor data and a larger JPEG.
    #[test]
    fn finds_the_largest_preview() {
        let jpeg = |w, h| {
            let mut out = Vec::new();
            image::RgbImage::new(w, h)
                .write_to(
                    &mut std::io::Cursor::new(&mut out),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
            out
        };
        let mut data = b"FUJIFILMCCD-RAW ".to_vec();
        data.extend(jpeg(16, 8));
        data.extend([0xff, 0xd8, 0xff, 0xc3, 0, 11, 8, 1, 0, 1, 0]);
        data.extend(jpeg(64, 32));
        data.extend([7; 100]);
        let found = super::preview(&data).unwrap();
        let img = image::load_from_memory(found).unwrap();
        assert_eq!((img.width(), img.height()), (64, 32));
        assert!(super::preview(b"no jpeg here").is_none());
    }
}
