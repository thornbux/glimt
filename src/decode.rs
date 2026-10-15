//! Decoding: an image file to pixels at the size it is shown (JPEG scaled while decoding, SVG
//! drawn to size), with its EXIF turn, colours and histogram; animations frame by frame.

use crate::colour::{ADOBE_RGB, to_display, to_srgb};
use crate::{exif, has_ext, human_size, raw};
use eframe::egui;
use image::ImageDecoder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Decoded frames kept of one animation (bytes); longer ones loop the first part.
pub static MAX_ANIMATION: AtomicUsize = AtomicUsize::new(128 << 20);
/// Frames of an animation kept at most, whatever their size.
const MAX_FRAMES: usize = 1000;

pub struct Decoded {
    /// The file the job was for; rejects results from a folder that has since been replaced.
    pub path: PathBuf,
    /// Decoded with EXIF rotation; rejected if the setting changed meanwhile.
    pub rotated: bool,
    /// The page of a multi-page TIFF; 0 for everything else.
    pub page: usize,
    /// The job's number, as in `App::pending`.
    pub job: u64,
    /// `None` = skipped because the user has already moved past it.
    pub result: Option<Result<DecodedImage, String>>,
}

/// The frames, the original size after EXIF rotation, the histogram and the number of pages.
pub type DecodedImage = (Frames, [usize; 2], Box<Hist>, usize);

/// Pixel counts per level of red, green, blue and brightness.
pub type Hist = [[u32; 256]; 4];

pub fn histogram(img: &egui::ColorImage) -> Box<Hist> {
    let mut h = Box::new([[0; 256]; 4]);
    for p in &img.pixels {
        let [r, g, b, _] = p.to_array();
        h[0][r as usize] += 1;
        h[1][g as usize] += 1;
        h[2][b as usize] += 1;
        // Rec. 709 luma.
        h[3][((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as usize] += 1;
    }
    h
}

/// The decoded frames with their delays in ms; one frame for a still image.
pub type Frames = Vec<(egui::ColorImage, u32)>;

/// Image `path` scaled to fit `max`, its original size and its number of pages; `page` picks a
/// page of a multi-page TIFF.
/// Whether `path` is a file (also through a link): a pipe would block a reader for good, and
/// a device such as /dev/zero would fill memory.
pub fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

pub fn decode(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
    page: usize,
) -> Result<(Frames, [usize; 2], usize), String> {
    // Also for a file from a collection or the command line, which the listing did not see.
    if !is_file(path) {
        return Err("not a regular file".into());
    }
    if let Some((frames, orig)) = animation(path, max) {
        return Ok((frames, orig, 1));
    }
    let pages = if has_ext(path, &["tif", "tiff"]) {
        tiff_pages(path)
    } else {
        1
    };
    let (img, icc, orig) = match scaled_jpeg(path, max, rotate) {
        Some(scaled) => scaled,
        None => {
            let (img, icc) = if page > 0 {
                (tiff_page(path, page)?, None)
            } else {
                load_icc(path, rotate)?
            };
            let orig = [img.width() as usize, img.height() as usize];
            (img, icc, orig)
        }
    };
    let img = to_display(shrink(img, max)?, icc.as_deref());
    Ok((vec![(color_image(img), 0)], orig, pages))
}

/// The number of pages (images) in a TIFF file; reads only their directories.
pub fn tiff_pages(path: &Path) -> usize {
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    // Each page's header can point at one shared table the tiff crate reads again for every
    // page: 100,000 pages would be minutes of work on each decode.
    // shortcut: pages after the 1,000th are not shown; count them lazily if such TIFFs turn up.
    let mut pages = 1;
    while pages < 1000 && dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    pages
}

/// Page `page` of a TIFF. The image crate reads only the first, so the tiff crate (which it
/// uses) decodes the others; 8- and 16-bit grey and colour, which covers scanned documents, and
/// grey of other kinds as science writes it (32-bit floats of a height model or a microscope,
/// signed or 32-bit integers), stretched from its lowest value to its highest.
pub fn tiff_page(path: &Path, page: usize) -> Result<image::DynamicImage, String> {
    use image::DynamicImage as D;
    use tiff::ColorType as C;
    use tiff::decoder::DecodingResult as R;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec =
        tiff::decoder::Decoder::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    dec.seek_to_image(page).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let color = dec.colortype().map_err(|e| e.to_string())?;
    let img = match (color, dec.read_image().map_err(|e| e.to_string())?) {
        (C::Gray(8), R::U8(v)) => image::GrayImage::from_raw(w, h, v).map(D::ImageLuma8),
        (C::GrayA(8), R::U8(v)) => image::GrayAlphaImage::from_raw(w, h, v).map(D::ImageLumaA8),
        (C::RGB(8), R::U8(v)) => image::RgbImage::from_raw(w, h, v).map(D::ImageRgb8),
        (C::RGBA(8), R::U8(v)) => image::RgbaImage::from_raw(w, h, v).map(D::ImageRgba8),
        (C::Gray(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageLuma16),
        (C::RGB(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgb16),
        (C::RGBA(16), R::U16(v)) => image::ImageBuffer::from_raw(w, h, v).map(D::ImageRgba16),
        (C::Gray(_), v) => {
            stretched(v).and_then(|v| image::GrayImage::from_raw(w, h, v).map(D::ImageLuma8))
        }
        _ => None,
    };
    img.ok_or_else(|| format!("page {} is in a colour format Glimt cannot show", page + 1))
}

/// Grey values of any kind as 0 to 255, from the lowest finite value to the highest; `None` for
/// the kinds the arms above take.
// shortcut: lowest to highest, so one stray value can leave the rest dark; stretch between
// percentiles if such files turn up.
fn stretched(values: tiff::decoder::DecodingResult) -> Option<Vec<u8>> {
    use tiff::decoder::DecodingResult as R;
    fn stretch<T: Copy>(v: &[T], f: impl Fn(T) -> f64) -> Vec<u8> {
        let finite = || v.iter().map(|&x| f(x)).filter(|x| x.is_finite());
        let low = finite().fold(f64::INFINITY, f64::min);
        let high = finite().fold(f64::NEG_INFINITY, f64::max);
        let scale = if high > low {
            255.0 / (high - low)
        } else {
            0.0
        };
        v.iter()
            .map(|&x| f(x))
            .map(|x| {
                if x.is_finite() {
                    ((x - low) * scale).round() as u8
                } else {
                    0
                }
            })
            .collect()
    }
    Some(match values {
        R::F32(v) => stretch(&v, f64::from),
        R::F64(v) => stretch(&v, |x| x),
        R::I8(v) => stretch(&v, f64::from),
        R::I16(v) => stretch(&v, f64::from),
        R::I32(v) => stretch(&v, f64::from),
        R::U32(v) => stretch(&v, f64::from),
        R::I64(v) => stretch(&v, |x| x as f64),
        R::U64(v) => stretch(&v, |x| x as f64),
        _ => return None,
    })
}

/// The whole image in sRGB.
pub fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    let (img, icc) = load_icc(path, rotate)?;
    Ok(to_srgb(img, icc.as_deref()))
}

/// The frames of an animated GIF or WebP; `None` for other files, which `load` decodes.
pub fn animation(path: &Path, max: [u32; 2]) -> Option<(Frames, [usize; 2])> {
    use image::AnimationDecoder;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    // Every frame is a whole canvas, made without image's limit, and the decoder keeps the last
    // one while it makes the next: two of them must fit. One too large goes to `load`, which
    // refuses it, instead of asking for gigabytes here.
    let too_large = |(w, h): (u32, u32)| u64::from(w) * u64::from(h) * 8 > MAX_DECODE;
    let frames = match ext.as_str() {
        "gif" => {
            let dec = image::codecs::gif::GifDecoder::new(file).ok()?;
            if too_large(dec.dimensions()) {
                return None;
            }
            dec.into_frames()
        }
        "webp" => {
            let dec = image::codecs::webp::WebPDecoder::new(file).ok()?;
            if !dec.has_animation() || too_large(dec.dimensions()) {
                return None;
            }
            dec.into_frames()
        }
        _ => return None,
    };
    let (mut out, mut orig, mut bytes) = (Vec::new(), [0, 0], 0);
    for frame in frames {
        let frame = frame.ok()?;
        let (n, d) = frame.delay().numer_denom_ms();
        // Browsers show delays under 20 ms as 100 ms; many GIFs rely on it.
        let ms = Some(n / d.max(1)).filter(|&ms| ms >= 20).unwrap_or(100);
        let img = frame.into_buffer();
        orig = [img.width() as usize, img.height() as usize];
        let img = color_image(to_display(shrink(img.into(), max).ok()?, None));
        bytes += img.pixels.len() * 4;
        out.push((img, ms));
        // Tiny frames pass the memory budget in their thousands, and each becomes a texture
        // when shown: 1,000 at most.
        if bytes > MAX_ANIMATION.load(Ordering::Relaxed) || out.len() >= MAX_FRAMES {
            break;
        }
    }
    (!out.is_empty()).then_some((out, orig))
}

/// The whole image (a preview frame for videos), turned upright by EXIF if `rotate`, and its
/// ICC profile if it has one.
pub fn load_icc(
    path: &Path,
    rotate: bool,
) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    #[cfg(feature = "video")]
    if crate::external::is_video(path) {
        return Ok((crate::video::preview(path)?, None));
    }
    if has_ext(path, raw::EXTS) {
        return Ok((raw::load(path, rotate)?, None));
    }
    #[cfg(any(feature = "heic", feature = "avif"))]
    if crate::external::is_heif(path) {
        return Ok((crate::external::heif(path)?, None));
    }
    #[cfg(feature = "svg")]
    if has_ext(path, crate::files::SVG_EXTS) {
        return Ok((svg(path)?, None));
    }
    #[cfg(feature = "jxl")]
    if has_ext(path, crate::files::JXL_EXTS) {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut dec = jxl_oxide::integration::JxlDecoder::new(std::io::BufReader::new(file))
            .map_err(|e| e.to_string())?;
        check_size(&dec)?;
        // jxl-oxide already renders the image upright.
        let icc = dec.icc_profile().ok().flatten();
        let img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
        return Ok((img, icc));
    }
    if let Some(decoder) = crate::decoders::for_path(path) {
        return Ok((decoder.decode(path)?, None));
    }
    let dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string());
    // Grey TIFFs the image crate does not take (floats, signed): through the tiff crate.
    let mut dec = match dec {
        Err(e) if has_ext(path, &["tif", "tiff"]) => {
            return tiff_page(path, 0).map(|img| (img, None)).map_err(|_| e);
        }
        dec => dec?,
    };
    // from_decoder allocates what the header claims, without the reader's limit.
    check_size(&dec)?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // Without a profile, the EXIF may say Adobe RGB (a camera set to it).
    let icc = dec.icc_profile().ok().flatten().or_else(|| {
        let exif = dec.exif_metadata().ok().flatten()?;
        exif::adobe_rgb(&exif).then(|| ADOBE_RGB.clone())
    });
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    if rotate {
        img.apply_orientation(orientation);
    }
    Ok((img, icc))
}

/// A JPEG, or the preview in a RAW file, decoded at 1/2, 1/4 or 1/8 of its size when that still
/// covers `max`, turned upright by EXIF if `rotate`, with its ICC profile (or Adobe RGB from EXIF)
/// and its full size upright. `None` where the whole image is needed or jpeg-decoder cannot help
/// (CMYK, an error): then it is decoded whole, as other images.
pub fn scaled_jpeg(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
) -> Option<(image::DynamicImage, Option<Vec<u8>>, [usize; 2])> {
    if has_ext(path, &["jpg", "jpeg"]) {
        return scaled(&std::fs::read(path).ok()?, max, rotate, None);
    }
    if has_ext(path, raw::EXTS) {
        let data = std::fs::read(path).ok()?;
        // As raw::load: the orientation from a TIFF-based RAW's own header, and no profile.
        let (img, _, orig) = scaled(raw::preview(&data)?, max, rotate, exif::orientation(&data))?;
        return Some((img, None, orig));
    }
    None
}

/// `scaled_jpeg` of the JPEG in `data`; `orientation` overrides the one in its EXIF.
pub fn scaled(
    data: &[u8],
    max: [u32; 2],
    rotate: bool,
    orientation: Option<image::metadata::Orientation>,
) -> Option<(image::DynamicImage, Option<Vec<u8>>, [usize; 2])> {
    use jpeg_decoder::PixelFormat;
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    dec.read_info().ok()?;
    let info = dec.info()?;
    if !matches!(info.pixel_format, PixelFormat::RGB24 | PixelFormat::L8) {
        return None;
    }
    // A progressive JPEG keeps a coefficient (2 bytes) of every pixel and channel at full size
    // while it decodes, and jpeg-decoder does not limit that: a file of a few KB that claims
    // 65535×65535 would ask for 25 GB. image's decoder, which has the limit, takes it instead.
    let coefficients = u64::from(info.width) * u64::from(info.height) * 2;
    if info.coding_process == jpeg_decoder::CodingProcess::DctProgressive
        && coefficients * info.pixel_format.pixel_bytes() as u64 > MAX_DECODE
    {
        return None;
    }
    let orientation = (orientation.or_else(|| dec.exif_data().and_then(exif::orientation)))
        .filter(|_| rotate)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let (w, h) = (info.width as f64, info.height as f64);
    // Upright size, which is what has to fit.
    use image::metadata::Orientation::{Rotate90, Rotate90FlipH, Rotate270, Rotate270FlipH};
    let turned = matches!(
        orientation,
        Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH
    );
    let (uw, uh) = if turned { (h, w) } else { (w, h) };
    let scale = (max[0] as f64 / uw).min(max[1] as f64 / uh);
    if scale > 0.5 {
        return None;
    }
    // jpeg-decoder takes the smallest of 1/8, 1/4, 1/2 and 1/1 at least this large.
    let (want_w, want_h) = ((w * scale).ceil() as u16, (h * scale).ceil() as u16);
    let (sw, sh) = dec.scale(want_w, want_h).ok()?;
    let pixels = dec.decode().ok()?;
    let (sw, sh) = (sw as u32, sh as u32);
    let mut img: image::DynamicImage = match info.pixel_format {
        PixelFormat::L8 => image::GrayImage::from_raw(sw, sh, pixels)?.into(),
        _ => image::RgbImage::from_raw(sw, sh, pixels)?.into(),
    };
    img.apply_orientation(orientation);
    let icc = dec.icc_profile().or_else(|| {
        let exif = dec.exif_data()?;
        exif::adobe_rgb(exif).then(|| ADOBE_RGB.clone())
    });
    Some((img, icc, [uw as usize, uh as usize]))
}

/// The size limit `ImageReader` applies, for decoders made directly: a header that claims a
/// huge image must not make Glimt allocate gigabytes (which aborts the process).
pub fn check_size(dec: &impl ImageDecoder) -> Result<(), String> {
    match dec.total_bytes() {
        n if n > MAX_DECODE => Err(format!("image too large ({})", human_size(n))),
        _ => Ok(()),
    }
}

/// image's default limit (`Limits::max_alloc`).
pub const MAX_DECODE: u64 = 512 << 20;

/// An SVG drawn by resvg, at its own size but at least 2048 pixels on the longer side, so it
/// stays sharp when fitted to the window.
/// usvg's own ways to fetch the pictures an SVG shows, with Glimt's limits first: a linked one
/// only if it is a file under 64 MB (not /dev/zero or a pipe), and none whose header claims
/// more pixels than an image Glimt decodes may have.
#[cfg(feature = "svg")]
fn svg_images() -> resvg::usvg::ImageHrefResolver<'static> {
    use resvg::usvg::ImageHrefResolver as R;
    let fits = |data: &[u8]| {
        let reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format();
        match reader.ok().and_then(|r| r.into_dimensions().ok()) {
            Some((w, h)) => u64::from(w) * u64::from(h) * 4 <= MAX_DECODE,
            // Not a picture image reads, such as an SVG: usvg reads it with these limits.
            None => true,
        }
    };
    let (data, string) = (R::default_data_resolver(), R::default_string_resolver());
    R {
        resolve_data: Box::new(move |mime, bytes, opts| {
            fits(bytes.as_slice())
                .then(|| data(mime, bytes, opts))
                .flatten()
        }),
        resolve_string: Box::new(move |href, opts| {
            let file = match Path::new(href) {
                p if p.is_absolute() => p.to_path_buf(),
                p => opts
                    .resources_dir
                    .as_deref()
                    .unwrap_or(Path::new("."))
                    .join(p),
            };
            let meta = std::fs::metadata(&file).ok().filter(|m| m.is_file())?;
            if meta.len() > 64 << 20 || !fits(&std::fs::read(&file).ok()?) {
                return None;
            }
            string(href, opts)
        }),
    }
}

#[cfg(feature = "svg")]
pub fn svg(path: &Path) -> Result<image::DynamicImage, String> {
    use resvg::{tiny_skia, usvg};
    use std::sync::Arc;
    // The system fonts, for text in SVGs, are read once: it takes tens of milliseconds.
    static FONTS: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: fonts.clone(),
        image_href_resolver: svg_images(),
        ..usvg::Options::default()
    };
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let tree = usvg::Tree::from_data(&data, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    // At most 8192 pixels on the longer side: an SVG may claim any size, and the pixmap is
    // 4 bytes per pixel (60000×60000 would be 14 GB).
    let longer = size.width().max(size.height());
    let scale = (2048.0 / longer).max(1.0).min(8192.0 / longer);
    let (w, h) = (
        (size.width() * scale).ceil() as u32,
        (size.height() * scale).ceil() as u32,
    );
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("SVG too large")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia keeps premultiplied alpha; image wants it straight.
    let rgba: Vec<u8> = (pixmap.pixels().iter())
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    image::RgbaImage::from_raw(w.max(1), h.max(1), rgba)
        .map(image::DynamicImage::from)
        .ok_or_else(|| "SVG size mismatch".into())
}

/// `work`, or an error if it panics: a decoder crashing on a broken file must not end the
/// worker thread, which would leave every later image or thumbnail waiting forever.
pub fn survive<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("the decoder crashed on this file".into()))
}

/// Scale down to fit within `max`.
pub fn shrink(img: image::DynamicImage, max: [u32; 2]) -> Result<image::DynamicImage, String> {
    if img.width() > max[0] || img.height() > max[1] {
        let scale = (max[0] as f64 / img.width() as f64).min(max[1] as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).max(1);
        let h = ((img.height() as f64 * scale).round() as u32).max(1);
        let mut small = image::DynamicImage::new(w, h, img.color());
        // SIMD downscaling; image's thumbnail took ~150–330 ms on 12 MP.
        let opts = fast_image_resize::ResizeOptions::new().resize_alg(
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Bilinear),
        );
        fast_image_resize::Resizer::new()
            .resize(&img, &mut small, &opts)
            .map_err(|e| e.to_string())?;
        return Ok(small);
    }
    Ok(img)
}

pub fn color_image(img: image::DynamicImage) -> egui::ColorImage {
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

#[cfg(test)]
mod tests {
    /// A small GIF of many tiny frames keeps 1,000 of them, not one texture each.
    #[test]
    fn keeps_a_thousand_frames() {
        let file =
            std::env::temp_dir().join(format!("glimt-test-frames-{}.gif", std::process::id()));
        let mut enc = image::codecs::gif::GifEncoder::new(std::fs::File::create(&file).unwrap());
        let frames = (0..1200).map(|_| image::Frame::new(image::RgbaImage::new(1, 1)));
        enc.encode_frames(frames).unwrap();
        drop(enc);
        let (frames, _) = super::animation(&file, [100, 100]).unwrap();
        assert_eq!(frames.len(), super::MAX_FRAMES);
        std::fs::remove_file(file).unwrap();
    }

    /// A JPEG far larger than the window is decoded scaled, turned upright by its EXIF, with
    /// its full upright size; one that needs more than half its pixels is left to image.
    #[test]
    fn decodes_jpeg_scaled_and_upright() {
        let dir = std::env::temp_dir().join(format!("glimt-scaled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("wide.jpg");
        // Red on the left half, blue on the right.
        let img = image::RgbImage::from_fn(400, 200, |x, _| match x < 200 {
            true => image::Rgb([255, 0, 0]),
            false => image::Rgb([0, 0, 255]),
        });
        img.save(&file).unwrap();
        // A quarter turn clockwise (EXIF orientation 6): the left half ends up on top.
        crate::exif::write_orientation(&file, 1, false).unwrap();
        let (img, _, orig) = super::scaled_jpeg(&file, [60, 100], true).unwrap();
        assert_eq!(orig, [200, 400]);
        assert_eq!((img.width(), img.height()), (50, 100));
        let rgb = img.to_rgb8();
        let (top, bottom) = (rgb.get_pixel(25, 10), rgb.get_pixel(25, 90));
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        // Not turned: the stored size; and no gain at more than half: None.
        let (_, _, orig) = super::scaled_jpeg(&file, [100, 60], false).unwrap();
        assert_eq!(orig, [400, 200]);
        assert!(super::scaled_jpeg(&file, [300, 300], true).is_none());
        assert!(super::scaled_jpeg(&dir.join("none.jpg"), [10, 10], true).is_none());
        // A RAW file's preview, scaled the same way.
        let mut raw = b"FUJIFILMCCD-RAW ".to_vec();
        raw.extend(std::fs::read(&file).unwrap());
        std::fs::write(dir.join("a.raf"), raw).unwrap();
        let (img, icc, orig) = super::scaled_jpeg(&dir.join("a.raf"), [60, 100], true).unwrap();
        assert_eq!((orig, img.width(), img.height()), ([200, 400], 50, 100));
        assert!(icc.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Grey TIFFs of floats, as science writes them, shown stretched from the lowest value to
    /// the highest; a float RGB one as the image crate reads it.
    #[test]
    fn shows_float_tiff() {
        use tiff::encoder::{TiffEncoder, colortype};
        let dir = std::env::temp_dir().join(format!("glimt-test-float-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gray: Vec<f32> = (0..60 * 30).map(|i| i as f32 * 3.7 - 500.0).collect();
        gray[1] = f32::NAN;
        let file = std::fs::File::create(dir.join("gray.tif")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray32Float>(60, 30, &gray)
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("gray.tif"), [100, 100], true, 0).unwrap();
        let px = &frames[0].0.pixels;
        assert_eq!(orig, [60, 30]);
        assert_eq!((px[0].r(), px[1].r(), px[px.len() - 1].r()), (0, 0, 255));
        let rgb: Vec<f32> = (0..60 * 30 * 3).map(|i| (i % 3) as f32 * 0.5).collect();
        let file = std::fs::File::create(dir.join("rgb.tif")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::RGB32Float>(60, 30, &rgb)
            .unwrap();
        assert!(super::decode(&dir.join("rgb.tif"), [100, 100], true, 0).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// BMP, ICO, QOI and PNM are listed and shown as the other everyday formats.
    #[test]
    fn lists_and_decodes_more_formats() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["c.bmp", "d.ico", "e.qoi"] {
            image::RgbaImage::new(60, 30).save(dir.join(name)).unwrap();
        }
        image::RgbImage::new(60, 30)
            .save(dir.join("f.ppm"))
            .unwrap();
        let types = crate::files::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (_, images) = crate::files::list_folder(&dir, types);
        assert_eq!(
            images,
            ["c.bmp", "d.ico", "e.qoi", "f.ppm"].map(std::ffi::OsString::from)
        );
        for name in images {
            let (frames, orig, _) = super::decode(&dir.join(name), [100, 100], true, 0).unwrap();
            assert_eq!((frames[0].0.size, orig), ([60, 30], [60, 30]));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join(format!("glimt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".b.png"), "").unwrap();
        let mut all = crate::files::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let (dirs, images) = crate::files::list_folder(&dir, all);
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        // Hidden files and folders, when shown.
        all.hidden = true;
        let (dirs, images) = crate::files::list_folder(&dir, all);
        assert_eq!(images, [".b.png", "a.png"].map(std::ffi::OsString::from));
        assert_eq!(dirs, vec![dir.join(".hidden"), dir.join("sub")]);
        std::fs::remove_file(dir.join(".b.png")).unwrap();
        let (frames, orig, _) = super::decode(&dir.join("a.png"), [100, 100], true, 0).unwrap();
        assert_eq!(
            (frames.len(), frames[0].0.size, orig),
            (1, [100, 50], [400, 200])
        );
        // An animated GIF gives every frame with its delay (5 ms counts as 100 ms).
        let gif = std::fs::File::create(dir.join("b.gif")).unwrap();
        let frame = |ms| {
            let delay = image::Delay::from_numer_denom_ms(ms, 1);
            image::Frame::from_parts(image::RgbaImage::new(40, 20), 0, 0, delay)
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(50), frame(5)])
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("b.gif"), [100, 100], true, 0).unwrap();
        let delays: Vec<u32> = frames.iter().map(|f| f.1).collect();
        assert_eq!((delays, orig), (vec![50, 100], [40, 20]));
        // A GIF whose canvas claims 65535×65535 is refused before a frame is made: each frame
        // would be a canvas of 17 GB.
        let huge = dir.join("huge.gif");
        let mut gif = b"GIF89a\xff\xff\xff\xff\0\0\0".to_vec();
        gif.extend(b",\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0;");
        std::fs::write(&huge, gif).unwrap();
        assert!(super::animation(&huge, [100, 100]).is_none());
        assert!(super::decode(&huge, [100, 100], true, 0).is_err());
        std::fs::remove_file(huge).unwrap();
        // The tree: a folder reached again through a link is read once.
        std::fs::copy(dir.join("a.png"), dir.join("sub/c.png")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/back")).unwrap();
        assert_eq!(
            crate::files::list_tree(
                &dir,
                crate::files::Types {
                    images: true,
                    raw: true,
                    videos: true,
                    hidden: false
                }
            ),
            ["a.png", "b.gif", "sub/c.png"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An SVG that points at /dev/zero, or at a picture claiming 100,000×100,000 pixels, is
    /// drawn without them, at once.
    #[cfg(feature = "svg")]
    #[test]
    fn svg_pictures_have_limits() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A PNG header claiming 100,000 × 100,000; its CRC is right, so readers believe it.
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend(100_000u32.to_be_bytes());
        ihdr.extend(100_000u32.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        let crc = crc32(&ihdr);
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0d".to_vec();
        png.extend(&ihdr);
        png.extend(crc.to_be_bytes());
        std::fs::write(dir.join("huge.png"), &png).unwrap();
        let svg = dir.join("a.svg");
        std::fs::write(
            &svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                 <image href="/dev/zero" width="10" height="10"/>
                 <image href="huge.png" width="10" height="10"/></svg>"#,
        )
        .unwrap();
        assert!(super::svg(&svg).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// CRC-32 as PNG chunks have it.
    #[cfg(feature = "svg")]
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn survives_a_panic() {
        assert_eq!(super::survive(|| Ok::<_, String>(1)), Ok(1));
        assert!(super::survive(|| -> Result<(), String> { panic!("broken file") }).is_err());
    }
}
