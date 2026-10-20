//! Camera RAW files, shown by the JPEG preview the camera stores in them, as Geeqie does by
//! default: fast, and it looks as the camera showed it. The largest JPEG in the file is used,
//! which works for TIFF-based RAW (CR2, NEF, ARW, DNG, ORF, PEF, RW2 …) as well as RAF and CR3
//! without parsing each format.

use image::ImageDecoder;
use std::path::Path;

pub const EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "rw2", "raf", "pef",
    "srw", "erf", "kdc", "dcr", "3fr", "fff", "iiq", "mef", "mos", "mrw", "rwl", "x3f",
    // Sony pixel shift, GoPro (DNG inside), Panasonic before RW2.
    "arq", "gpr", "raw",
];

/// The preview, turned upright if `rotate`.
pub fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    load_data(
        &crate::decode::read_limited(path).map_err(|e| e.to_string())?,
        rotate,
    )
}

/// `load` of a RAW file read already.
pub fn load_data(data: &[u8], rotate: bool) -> Result<image::DynamicImage, String> {
    let jpeg = preview(data).ok_or("no preview image in the RAW file")?;
    let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(jpeg))
        .map_err(|e| e.to_string())?;
    crate::decode::check_size(&dec)?;
    // TIFF-based RAW files carry the orientation in their own header; others in the JPEG.
    let orientation = crate::exif::orientation(data)
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
    let head = crate::decode::read_head(path, 1 << 20).ok()?;
    if matches!(head.get(..2), Some(b"II" | b"MM")) {
        return Some(head);
    }
    // The preview's headers come before its picture, so the first MB has them in most files;
    // the whole file (50 MB and more) only when not: sorting thousands by date reads the heads.
    let from = |data: &[u8]| {
        let jpeg = preview(data)?;
        let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(jpeg)).ok()?;
        dec.exif_metadata().ok().flatten()
    };
    from(&head).or_else(|| from(&crate::decode::read_limited(path).ok()?))
}

/// The largest JPEG in `data` that the decoder handles: baseline or progressive. Lossless JPEG
/// (raw sensor data in DNG and CR2) is passed over.
pub fn preview(data: &[u8]) -> Option<&[u8]> {
    // Find the start markers first, then look at them: one tight loop is ~7× faster through
    // a 20 MB file (15 ms) than reading the headers along the way.
    // A RAW file has a few; a file made of start markers would have millions, each walked to
    // its end: the time would grow with the square of the size.
    let starts: Vec<usize> = (1..data.len().saturating_sub(1))
        .filter(|&i| data[i] == 0xd8 && data[i - 1] == 0xff && data[i + 1] == 0xff)
        .take(1000)
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

/// Whether all of the JPEG at the start of `data` is there: its end marker after its image data,
/// not only the one of the thumbnail in its EXIF.
pub fn whole_jpeg(data: &[u8]) -> bool {
    crate::exif::jpeg_scan_start(data)
        .is_some_and(|scan| data[scan..].windows(2).any(|w| w == [0xff, 0xd9]))
}

/// Width and height of a baseline or progressive JPEG from its frame header, by walking the
/// segments after the start marker. Only a header as a camera writes it counts (8 bits, grey or
/// colour, its length fitting): in compressed sensor data a start marker and a frame marker
/// turn up by chance, followed by sizes far larger than the real preview's.
pub fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let be = |i: usize| Some(u16::from_be_bytes([*jpeg.get(i)?, *jpeg.get(i + 1)?]) as usize);
    let mut i = 2;
    // The frame header comes after a few segments; a crafted file could chain them to its end.
    for _ in 0..32 {
        if *jpeg.get(i)? != 0xff {
            return None;
        }
        match *jpeg.get(i + 1)? {
            0xc0..=0xc2 => {
                let parts = *jpeg.get(i + 9)? as usize;
                let real =
                    *jpeg.get(i + 4)? == 8 && matches!(parts, 1 | 3) && be(i + 2)? == 8 + 3 * parts;
                let (w, h) = (be(i + 7)? as u32, be(i + 5)? as u32);
                return (real && w > 0 && h > 0).then_some((w, h));
            }
            // Other frame types (lossless, arithmetic), or image data before any frame.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xda => return None,
            _ => i += 2 + be(i + 2)?,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    /// A JPEG cut off in the middle of its image data is not whole; one with its end is.
    #[test]
    fn tells_a_whole_jpeg() {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(64, 64, image::Rgb([200, 30, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert!(super::whole_jpeg(&jpeg));
        assert!(!super::whole_jpeg(&jpeg[..jpeg.len() - 40]));
    }

    /// A made-up RAW file: a header, a small JPEG, lossless sensor data, a larger JPEG and a
    /// frame header that sensor data holds by chance.
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
        // Sensor data that happens to hold a start and a frame marker, then random sizes.
        data.extend([
            0xff, 0xd8, 0xff, 0xc0, 0, 0x11, 8, 0xff, 0xff, 0xff, 0xff, 7, 1,
        ]);
        data.extend([7; 100]);
        let found = super::preview(&data).unwrap();
        let img = image::load_from_memory(found).unwrap();
        assert_eq!((img.width(), img.height()), (64, 32));
        assert!(super::preview(b"no jpeg here").is_none());
    }

    /// Sony's pixel-shift .arq, GoPro's .gpr and Panasonic's old .raw: listed with the RAW
    /// files and shown by their preview.
    #[test]
    fn lists_and_shows_more_raw() {
        let dir = std::env::temp_dir().join(format!("glimt-test-raw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut preview = Vec::new();
        image::RgbImage::new(60, 40)
            .write_to(
                &mut std::io::Cursor::new(&mut preview),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        for name in ["a.arq", "b.gpr", "c.raw"] {
            let mut data = b"II*\0\x08\0\0\0".to_vec();
            data.extend(&preview);
            std::fs::write(dir.join(name), data).unwrap();
        }
        let types = crate::files::Types {
            images: false,
            raw: true,
            videos: false,
            hidden: false,
        };
        let (_, listed) = crate::files::list_folder(&dir, types);
        assert_eq!(
            listed,
            ["a.arq", "b.gpr", "c.raw"].map(std::ffi::OsString::from)
        );
        for name in listed {
            let img = super::load(&dir.join(name), true).unwrap();
            assert_eq!((img.width(), img.height()), (60, 40));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// RAW files broken in thousands of ways, and one made of start markers: a preview found or
    /// not, without a panic, and soon.
    #[test]
    fn survives_broken_raw() {
        let mut out = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut out);
        image::RgbImage::new(16, 8)
            .write_to(&mut cursor, image::ImageFormat::Jpeg)
            .unwrap();
        let mut seed = b"FUJIFILMCCD-RAW ".to_vec();
        seed.extend(&out);
        for data in crate::broken(&seed) {
            super::preview(&data);
        }
        let markers = [0xff, 0xd8, 0xff, 0xe0, 0, 4].repeat(1 << 20);
        let started = std::time::Instant::now();
        super::preview(&markers);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
