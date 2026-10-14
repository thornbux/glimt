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

pub struct Decoded {
    pub idx: usize,
    pub full: bool,
    /// The file the job was for; rejects results from a folder that has since been replaced.
    pub path: PathBuf,
    /// Decoded with EXIF rotation; rejected if the setting changed meanwhile.
    pub rotated: bool,
    /// The page of a multi-page TIFF; 0 for everything else.
    pub page: usize,
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
pub fn decode(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
    page: usize,
) -> Result<(Frames, [usize; 2], usize), String> {
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
    let mut pages = 1;
    while dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    pages
}

/// Page `page` of a TIFF. The image crate reads only the first, so the tiff crate (which it
/// uses) decodes the others; 8- and 16-bit grey and colour, which covers scanned documents.
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
        _ => None,
    };
    img.ok_or_else(|| format!("page {} is in a colour format Glimt cannot show", page + 1))
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
    // Every frame is a whole canvas, made without image's limit: one larger than it goes to
    // `load`, which refuses it, instead of asking for gigabytes here.
    let too_large = |(w, h): (u32, u32)| u64::from(w) * u64::from(h) * 4 > MAX_DECODE;
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
        if bytes > MAX_ANIMATION.load(Ordering::Relaxed) {
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
    let mut dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
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
