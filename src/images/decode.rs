//! Decoding turns an image file into pixels at the size it is shown (a JPEG scaled while it is
//! decoded, an SVG drawn to size), with its EXIF turn, its colours and its histogram.
//! Animations are decoded frame by frame.

use crate::images::colour::{ADOBE_RGB, to_display, to_srgb};
use crate::model::listing::has_ext;
use crate::support::format::human_size;
use crate::{images::raw, metadata::exif};
use eframe::egui;
use image::ImageDecoder;
use image::metadata::Orientation;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many bytes of decoded frames are kept of one animation. A longer one loops its first
/// part.
pub static MAX_ANIMATION: AtomicUsize = AtomicUsize::new(128 << 20);

/// Frames of an animation kept at most, whatever their size.
const MAX_FRAMES: usize = 1000;

/// image's default limit (`Limits::max_alloc`).
pub const MAX_DECODE: u64 = 512 << 20;

pub struct Decoded {
    /// The file the job was for. It rejects results from a folder that has since been replaced.
    pub path: PathBuf,
    /// Whether it was decoded with the EXIF rotation. It is rejected if the setting changed
    /// meanwhile.
    pub rotated: bool,
    /// The page of a multi-page TIFF. It is 0 for everything else.
    pub page: usize,
    /// The job's number, as in `App::pending`.
    pub job: u64,
    /// `None` = skipped because the user has already moved past it.
    pub result: Option<Result<DecodedImage, String>>,
}

/// The frames, the original size after EXIF rotation, the histogram and the number of pages.
pub type DecodedImage = (Frames, [usize; 2], Box<Hist>, usize);

/// Pixel counts per level of red, green, blue, brightness and alpha.
pub type Hist = [[u32; 256]; 5];

/// The decoded frames with their delays in ms. A still image has one frame.
pub type Frames = Vec<(egui::ColorImage, u32)>;

/// An image, its ICC profile if it has one, and its size upright before any shrinking.
pub type Small = (image::DynamicImage, Option<Vec<u8>>, [usize; 2]);

/// An image as read, its ICC profile, the EXIF turn still to do, and its real size where it
/// came smaller.
type Unturned = (
    image::DynamicImage,
    Option<Vec<u8>>,
    Orientation,
    Option<[usize; 2]>,
);

pub fn histogram(img: &egui::ColorImage) -> Box<Hist> {
    let mut h = Box::new([[0; 256]; 5]);
    for p in &img.pixels {
        let [r, g, b, a] = p.to_array();
        h[4][a as usize] += 1;
        h[0][r as usize] += 1;
        h[1][g as usize] += 1;
        h[2][b as usize] += 1;
        // Rec. 709 luma.
        h[3][((54 * r as u32 + 183 * g as u32 + 19 * b as u32) >> 8) as usize] += 1;
    }
    h
}

/// Whether `path` is a file (also through a link). A pipe would block a reader for good, and a
/// device such as /dev/zero would fill memory.
pub fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// Decodes image `path` scaled to fit `max`. Returns it with its original size and its number
/// of pages. `page` picks a page of a multi-page TIFF.
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
    let pages = pages(path);
    let (img, icc, orig) = match scaled_jpeg(path, max, rotate) {
        Some(scaled) => scaled,
        None => {
            // HEIC, AVIF and video frames come from ffmpeg no larger than the box, with their
            // real size from the probe, and an SVG is drawn at the box's size. The other
            // formats come whole and are shrunk here.
            let svg = has_ext(path, crate::model::listing::SVG_EXTS);
            let to_box = (crate::files::external::ffmpeg_decodes(path) || svg).then_some(max);
            let (img, icc, turn, size) = if page > 0 {
                (page_of(path, page)?, None, Orientation::NoTransforms, None)
            } else {
                load_unturned(path, rotate, to_box)?
            };
            // The image is shrunk first and then turned, because a 100 MP photo with
            // orientation 6 turned whole would need a second 400 MB buffer for a moment.
            let quarter = quarter_turn(turn);
            let [w, h] = [img.width() as usize, img.height() as usize];
            let orig = size.unwrap_or(if quarter { [h, w] } else { [w, h] });
            let mut img = shrink(img, if quarter { [max[1], max[0]] } else { max })?;
            img.apply_orientation(turn);
            (img, icc, orig)
        }
    };
    let img = to_display(shrink(img, max)?, icc.as_deref());
    Ok((vec![(color_image(img), 0)], orig, pages))
}

/// The number of pages of a multi-page TIFF, or of sizes in an ICO. It is 1 for other files.
pub fn pages(path: &Path) -> usize {
    if has_ext(path, &["tif", "tiff"]) {
        tiff_pages(path)
    } else if has_ext(path, &["ico"]) {
        ico_sizes(path).map_or(1, |sizes| sizes.len().max(1))
    } else {
        1
    }
}

/// Page `page` (from 1) of a multi-page TIFF, or the next size of an ICO.
pub fn page_of(path: &Path, page: usize) -> Result<image::DynamicImage, String> {
    match has_ext(path, &["ico"]) {
        true => ico_size(path, page),
        false => tiff_page(path, page),
    }
}

/// An ICO's images, as their directory entries, in the order they are shown. The first is the
/// one the image crate shows first (most bits per pixel, then the largest, and the last of
/// equals), and the others follow in the same way. It reads only the directory.
fn ico_sizes(path: &Path) -> Result<Vec<[u8; 16]>, String> {
    // It reads as many entries as the count can say (1 MB at most), because the image crate
    // picks among all of them.
    let head = read_head(path, 6 + 16 * u64::from(u16::MAX)).map_err(|e| e.to_string())?;
    let le16 = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    if head.len() < 6 || le16(&head, 0) != 0 || le16(&head, 2) != 1 {
        return Err("not an ICO".into());
    }
    let n = (le16(&head, 4) as usize).min((head.len() - 6) / 16);
    let mut sizes: Vec<(u16, u32, usize, [u8; 16])> = (0..n)
        .map(|k| {
            let e: [u8; 16] = head[6 + 16 * k..][..16].try_into().unwrap();
            let side = |b: u8| if b == 0 { 256 } else { b as u32 };
            (le16(&e, 6), side(e[0]) * side(e[1]), k, e)
        })
        .collect();
    sizes.sort_by_key(|&(bits, area, k, _)| std::cmp::Reverse((bits, area, k)));
    Ok(sizes.into_iter().map(|s| s.3).collect())
}

/// Image `k` of an ICO in the order of `ico_sizes`, given to the image crate (which shows only
/// one) as an ICO of that image alone.
fn ico_size(path: &Path, k: usize) -> Result<image::DynamicImage, String> {
    use std::io::{Read, Seek};
    let mut entry = *ico_sizes(path)?.get(k).ok_or("no such size in the ICO")?;
    let le32 = |e: &[u8; 16], at: usize| u32::from_le_bytes(e[at..at + 4].try_into().unwrap());
    let (len, offset) = (le32(&entry, 8), le32(&entry, 12));
    let mut file = open(path).map_err(|e| e.to_string())?;
    file.seek(std::io::SeekFrom::Start(offset.into()))
        .map_err(|e| e.to_string())?;
    let mut one = vec![0, 0, 1, 0, 1, 0];
    entry[12..].copy_from_slice(&22u32.to_le_bytes());
    one.extend_from_slice(&entry);
    // The length is the file's word, so it reads no more than is there, and no more than
    // 64 MB, which a 256×256 image is far from.
    #[allow(clippy::disallowed_methods)] // bounded by take
    file.take(u64::from(len).min(1 << 26))
        .read_to_end(&mut one)
        .map_err(|e| e.to_string())?;
    image::load_from_memory_with_format(&one, image::ImageFormat::Ico).map_err(|e| e.to_string())
}

/// The number of pages (images) in a TIFF file. It reads only their directories.
pub fn tiff_pages(path: &Path) -> usize {
    let Ok(file) = open(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    // Each page's header can point at one shared table, which the tiff crate reads again for
    // every page, so 100,000 pages would be minutes of work on each decode.
    // shortcut: pages after the 1,000th are not shown; count them lazily if such TIFFs turn up.
    let mut pages = 1;
    while pages < 1000 && dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    pages
}

/// Page `page` of a TIFF. The image crate reads only the first page, so the tiff crate (which
/// it uses) decodes the others. It takes 8- and 16-bit grey and colour, which covers scanned
/// documents, and grey of other kinds as science writes it (32-bit floats of a height model or
/// a microscope, signed or 32-bit integers), stretched from its lowest value to its highest.
pub fn tiff_page(path: &Path, page: usize) -> Result<image::DynamicImage, String> {
    use image::DynamicImage as D;
    use tiff::ColorType as C;
    use tiff::decoder::DecodingResult as R;
    let file = open(path).map_err(|e| e.to_string())?;
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

/// Grey values of any kind as 0 to 255, from the lowest finite value to the highest. Returns
/// `None` for the kinds the arms above take.
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

/// Linear light of any brightness as an sRGB image. It uses Reinhard's global operator, which
/// makes the average brightness middle grey and rolls the highlights off instead of cutting
/// them.
fn tone_mapped(img: image::Rgb32FImage) -> image::DynamicImage {
    let luminance = |p: &image::Rgb<f32>| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let n = (img.pixels().len().max(1)) as f32;
    let log_mean = (img.pixels())
        .map(|p| (1e-4 + luminance(p).max(0.0)).ln())
        .filter(|l| l.is_finite())
        .sum::<f32>()
        / n;
    let exposure = 0.18 / log_mean.exp();
    let encode = |v: f32| {
        let v = v * exposure;
        let v = (v / (1.0 + v)).clamp(0.0, 1.0);
        let srgb = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    };
    let (w, h) = img.dimensions();
    let pixels = img.pixels().flat_map(|p| p.0.map(encode)).collect();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, pixels).unwrap_or_default())
}

/// The whole image in sRGB.
pub fn load(path: &Path, rotate: bool) -> Result<image::DynamicImage, String> {
    let (img, icc) = load_icc(path, rotate)?;
    Ok(to_srgb(img, icc.as_deref()))
}

/// The frames of a GIF, an animated WebP or an animated PNG (APNG), decoded as they are taken, and
/// its size. Returns `None` for any other file, a still WebP or PNG among them.
fn animated_frames(path: &Path) -> Option<(image::Frames<'static>, (u32, u32))> {
    use image::{AnimationDecoder, ImageDecoder};
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let file = std::io::BufReader::new(open(path).ok()?);
    // The decoders get their own limit on what they allocate (512 MB, as `MAX_DECODE`), so a
    // damaged frame header that claims gigabytes is an error, not a failed allocation that ends
    // Glimt.
    let limits = image::Limits::default();
    Some(match ext.as_str() {
        "gif" => {
            let mut dec = image::codecs::gif::GifDecoder::new(file).ok()?;
            dec.set_limits(limits).ok()?;
            let size = dec.dimensions();
            (dec.into_frames(), size)
        }
        "webp" => {
            let mut dec = image::codecs::webp::WebPDecoder::new(file).ok()?;
            dec.set_limits(limits).ok()?;
            let size = dec.dimensions();
            (dec.has_animation().then(|| dec.into_frames())?, size)
        }
        "png" | "apng" => {
            let dec = image::codecs::png::PngDecoder::with_limits(file, limits).ok()?;
            let size = dec.dimensions();
            if !dec.is_apng().ok()? {
                return None;
            }
            (dec.apng().ok()?.into_frames(), size)
        }
        _ => return None,
    })
}

/// Whether an animation of this size is too large. Every frame is a whole canvas, made without
/// image's limit, and the decoder keeps the last one while it makes the next, so two of them
/// must fit.
fn too_large_animation((w, h): (u32, u32)) -> bool {
    u64::from(w) * u64::from(h) * 8 > MAX_DECODE
}

/// The frames of an animated GIF, WebP or PNG (APNG). Returns `None` for other files, which `load`
/// decodes.
pub fn animation(path: &Path, max: [u32; 2]) -> Option<(Frames, [usize; 2])> {
    let (frames, size) = animated_frames(path)?;
    // One too large goes to `load`, which refuses it, instead of asking for gigabytes here.
    if too_large_animation(size) {
        return None;
    }
    let (mut out, mut orig, mut bytes) = (Vec::new(), [0, 0], 0);
    for frame in frames {
        // A GIF cut short in a download keeps the frames that came, as browsers show them.
        let Ok(frame) = frame else { break };
        let (n, d) = frame.delay().numer_denom_ms();
        // Browsers show delays under 20 ms as 100 ms, and many GIFs rely on it.
        let ms = Some(n / d.max(1)).filter(|&ms| ms >= 20).unwrap_or(100);
        let img = frame.into_buffer();
        orig = [img.width() as usize, img.height() as usize];
        let img = color_image(to_display(shrink(img.into(), max).ok()?, None));
        bytes += img.pixels.len() * 4;
        out.push((img, ms));
        // Tiny frames pass the memory budget in their thousands, and each becomes a texture
        // when shown, so there are 1,000 at most.
        if bytes > MAX_ANIMATION.load(Ordering::Relaxed) || out.len() >= MAX_FRAMES {
            break;
        }
    }
    (!out.is_empty()).then_some((out, orig))
}

/// Gives every frame of an animation, or every page of a TIFF, at full size and in sRGB, one at
/// a time to `each` (its number from 0 and the image), or only frame `only`. Returns how many
/// were given. The first page is turned upright by EXIF if `rotate`, as the view shows it. It
/// is for File → Save This Frame and Export Frames.
pub fn each_frame(
    path: &Path,
    only: Option<usize>,
    rotate: bool,
    mut each: impl FnMut(usize, image::DynamicImage) -> Result<(), String>,
) -> Result<usize, String> {
    let wanted = |k: usize| only.is_none_or(|o| o == k);
    let mut done = 0;
    // Each frame is decoded once, as it is given, rather than all first to learn that the
    // image is animated. An animation too large for its frames may still have its first frame
    // read alone, as a still image by `load`.
    let animated = animated_frames(path);
    if let Some((_, size)) = &animated
        && too_large_animation(*size)
        && only != Some(0)
    {
        return Err("image too large".into());
    }
    if let Some((frames, _)) = animated.filter(|(_, size)| !too_large_animation(*size)) {
        for (k, frame) in frames.enumerate() {
            if only.is_some_and(|o| k > o) {
                break;
            }
            let frame = frame.map_err(|e| e.to_string())?;
            if wanted(k) {
                each(k, frame.into_buffer().into())?;
                done += 1;
            }
        }
        return Ok(done);
    }
    let pages = pages(path);
    for k in (0..pages).filter(|&k| wanted(k)) {
        let img = if k == 0 {
            load(path, rotate)?
        } else {
            page_of(path, k)?
        };
        each(k, img)?;
        done += 1;
    }
    Ok(done)
}

/// An image no larger than `max`, with its ICC profile and its upright size before it was
/// shrunk. A JPEG is decoded at a fraction, and other images whole (HEIC, AVIF and video frames
/// at `max` from ffmpeg) and shrunk after. This is the one way for thumbnails, the print and its
/// preview. The view (`decode`) has its own, which shrinks before it turns.
pub fn small(path: &Path, max: [u32; 2], rotate: bool) -> Result<Small, String> {
    let (img, icc, size) = match scaled_jpeg(path, max, rotate) {
        Some(found) => found,
        None => {
            let (img, icc, size) = load_for(path, rotate, Some(max))?;
            (img, icc, size)
        }
    };
    Ok((shrink(img, max)?, icc, size))
}

/// Returns the ICC profile inside `path`, read by the same decoder the view reads it with, or
/// `None` when it has none. The info panel names it.
pub fn icc_profile(path: &Path) -> Option<Vec<u8>> {
    #[cfg(feature = "jxl")]
    if has_ext(path, crate::model::listing::JXL_EXTS) {
        let file = std::io::BufReader::new(open(path).ok()?);
        let mut dec = jxl_oxide::integration::JxlDecoder::new(file).ok()?;
        return dec.icc_profile().ok().flatten();
    }
    image_reader(path)
        .ok()?
        .into_decoder()
        .ok()?
        .icc_profile()
        .ok()
        .flatten()
}

/// The whole image (a preview frame for videos), turned upright by EXIF if `rotate`, and its
/// ICC profile if it has one.
pub fn load_icc(
    path: &Path,
    rotate: bool,
) -> Result<(image::DynamicImage, Option<Vec<u8>>), String> {
    let (img, icc, _) = load_for(path, rotate, None)?;
    Ok((img, icc))
}

/// As `load_icc`, for a thumbnail no larger than `max`, with the image's real size, upright.
/// HEIC, AVIF and video frames come at that size from ffmpeg, while other formats are shrunk
/// after.
pub fn load_for(path: &Path, rotate: bool, max: Option<[u32; 2]>) -> Result<Small, String> {
    let (mut img, icc, turn, size) = load_unturned(path, rotate, max)?;
    img.apply_orientation(turn);
    let size = size.unwrap_or([img.width() as usize, img.height() as usize]);
    Ok((img, icc, size))
}

/// As `load_for`, but the EXIF turn the image crate reads is returned rather than done, so the
/// view shrinks first and turns the small image, not a copy of the whole one. Other formats come
/// upright already (their turn is `NoTransforms`). The size is given only where the image came
/// smaller than it is (ffmpeg's formats at `max`).
fn load_unturned(path: &Path, rotate: bool, max: Option<[u32; 2]>) -> Result<Unturned, String> {
    let upright = Orientation::NoTransforms;
    // Only ffmpeg's formats and SVG take `max`, so without them it has no use.
    #[cfg(not(any(feature = "video", feature = "heic", feature = "avif", feature = "svg")))]
    let _ = max;
    #[cfg(feature = "video")]
    if crate::files::external::is_video(path) {
        let (img, size) = crate::images::video::preview(path, max)?;
        return Ok((img, None, upright, size));
    }
    if has_ext(path, raw::EXTS) {
        return Ok((raw::load(path, rotate)?, None, upright, None));
    }
    #[cfg(any(feature = "heic", feature = "avif"))]
    if crate::files::external::is_heif(path) {
        let (img, size) = crate::files::external::heif(path, max)?;
        return Ok((img, None, upright, size));
    }
    #[cfg(feature = "svg")]
    if has_ext(path, crate::model::listing::SVG_EXTS) {
        let (img, size) = crate::images::svg::load(path, max)?;
        return Ok((img, None, upright, size));
    }
    #[cfg(feature = "jxl")]
    if has_ext(path, crate::model::listing::JXL_EXTS) {
        let file = open(path).map_err(|e| e.to_string())?;
        let mut dec = jxl_oxide::integration::JxlDecoder::new(std::io::BufReader::new(file))
            .map_err(|e| e.to_string())?;
        // jxl-oxide starts with no limit on what it allocates. A lossy JPEG XL needs some 24
        // bytes a pixel while it renders, so a camera photo of 24 MP takes about 600 MB. Four
        // times a decode's limit lets such photos through and stops a render of some 90 MP or
        // more. A header that claims a larger picture than a decode may hold is refused before
        // that, by `from_decoder`.
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(4 * MAX_DECODE);
        dec.set_limits(limits).map_err(|e| e.to_string())?;
        // jxl-oxide already renders the image upright.
        let icc = dec.icc_profile().ok().flatten();
        let img = from_decoder(dec)?;
        return Ok((img, icc, upright, None));
    }
    if let Some(decoder) = crate::images::decoder_plugins::for_path(path) {
        return Ok((decoder.decode(path)?, None, upright, None));
    }
    let dec = image_reader(path)
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string());
    // Grey TIFFs that the image crate does not take (floats, signed) go through the tiff crate.
    let mut dec = match dec {
        Err(e) if has_ext(path, crate::model::listing::TIFF_LIKE) => {
            return tiff_page(path, 0)
                .map(|img| (img, None, upright, None))
                .map_err(|_| e);
        }
        dec => dec?,
    };
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // Without a profile, the EXIF may say Adobe RGB (a camera set to it).
    let icc = dec.icc_profile().ok().flatten().or_else(|| {
        let exif = dec.exif_metadata().ok().flatten()?;
        exif::adobe_rgb(&exif).then(|| ADOBE_RGB.clone())
    });
    let mut img = from_decoder(dec)?;
    let turn = if rotate { orientation } else { upright };
    // Radiance HDR holds light as it was, far brighter than a screen, so it is brought into the
    // screen's range.
    if has_ext(path, &["hdr"]) {
        img = tone_mapped(img.into_rgb32f());
    }
    Ok((img, icc, turn, None))
}

/// A JPEG, or the preview in a RAW file, decoded at 1/2, 1/4 or 1/8 of its size when that still
/// covers `max`. It is turned upright by EXIF if `rotate`, and comes with its ICC profile (or
/// Adobe RGB from EXIF) and its full upright size. Returns `None` where the whole image is
/// needed or jpeg-decoder cannot help (CMYK, an error), and then it is decoded whole, as other
/// images are.
pub fn scaled_jpeg(
    path: &Path,
    max: [u32; 2],
    rotate: bool,
) -> Option<(image::DynamicImage, Option<Vec<u8>>, [usize; 2])> {
    if crate::model::listing::is_jpeg(path) {
        return scaled(&read_limited(path).ok()?, max, rotate, None);
    }
    if has_ext(path, raw::EXTS) {
        // As in raw::load, the orientation comes from a TIFF-based RAW's own header, and there
        // is no profile.
        let from = |data: &[u8]| {
            let (img, _, orig) = scaled(raw::preview(data)?, max, rotate, exif::orientation(data))?;
            Some((img, None, orig))
        };
        // The first 8 MB are read first. Cameras put the preview before the sensor data, so a
        // folder of 50 MB RAW files on a share is not read whole for its thumbnails.
        let head = read_head(path, 8 << 20).ok()?;
        let covers = |p: &[u8]| raw::jpeg_size(p).is_some_and(|(w, h)| w >= max[0] || h >= max[1]);
        if raw::preview(&head).is_some_and(|p| covers(p) && raw::whole_jpeg(p)) {
            if let Some(shown) = from(&head) {
                return Some(shown);
            }
            // When it cannot be decoded at a fraction (shown larger than half the preview), it
            // is decoded whole from the head, which holds all of it, rather than from the whole
            // file read after all.
            if let Ok(img) = raw::load_data(&head, rotate) {
                let orig = [img.width() as usize, img.height() as usize];
                return Some((img, None, orig));
            }
        }
        let data = read_limited(path).ok()?;
        // When it cannot be decoded at a fraction (shown larger than half the preview), it is
        // decoded whole from what was read, rather than read a second time.
        return from(&data).or_else(|| {
            let img = raw::load_data(&data, rotate).ok()?;
            let orig = [img.width() as usize, img.height() as usize];
            Some((img, None, orig))
        });
    }
    None
}

/// The first `max` bytes of `path`, if it is a file (`open_file`).
pub(crate) fn read_head(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut head = Vec::new();
    #[allow(clippy::disallowed_methods)] // bounded by take
    open_file(path)?.0.take(max).read_to_end(&mut head)?;
    Ok(head)
}

/// `scaled_jpeg` of the JPEG in `data`. `orientation` overrides the one in its EXIF.
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
    // while it decodes, and jpeg-decoder does not limit that. A file of a few KB that claims
    // 65535×65535 would ask for 25 GB, so image's decoder, which has the limit, takes it
    // instead.
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
    let (uw, uh) = if quarter_turn(orientation) {
        (h, w)
    } else {
        (w, h)
    };
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

/// Makes `mb` megabytes the most an animation's decoded frames may take, from the setting
/// `animation_mb`, at start and when it changes in Preferences.
pub fn set_animation_limit(mb: usize) {
    MAX_ANIMATION.store(mb << 20, Ordering::Relaxed);
}

/// Decodes the image `dec` reads, after the size limit that `ImageReader` applies. The image
/// crate's own `from_decoder` allocates whatever the header claims, so a few bytes that claim
/// a huge image would make Glimt allocate gigabytes, which aborts the process. This is the one
/// way a decoder made directly is decoded; clippy refuses the image crate's.
pub fn from_decoder(dec: impl ImageDecoder) -> Result<image::DynamicImage, String> {
    match dec.total_bytes() {
        n if n > MAX_DECODE => Err(format!("image too large ({})", human_size(n))),
        #[allow(clippy::disallowed_methods)] // the one place, after the size limit
        _ => image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string()),
    }
}

/// Whether Glimt can show `path`, for Check Images. The file is decoded small, as for the list's
/// thumbnails. A JPEG is also looked at for its end marker, which a file cut off in copying
/// lacks, though it may still decode with grey at the bottom. Returns why not, if it cannot.
pub fn check(path: &Path) -> Result<(), String> {
    survive(|| decode(path, [64, 64], false, 0).map(drop))?;
    if crate::model::listing::is_jpeg(path) && !jpeg_ends(path)? {
        return Err("cut off: the JPEG has no end".into());
    }
    Ok(())
}

/// Whether a JPEG's image data reaches its end marker (FF D9), which the data itself never
/// holds (an FF in it is followed by 00 or a restart marker). The marker is looked for from the
/// image data on, not in the last bytes, because phones put a video of megabytes after it in a
/// motion photo.
// shortcut: reads the whole file, as Check Images does each JPEG once; read from the image
// data's start in blocks if motion photos of hundreds of MB make it slow.
fn jpeg_ends(path: &Path) -> Result<bool, String> {
    let data = read_limited(path).map_err(|e| e.to_string())?;
    Ok(crate::images::raw::whole_jpeg(&data))
}

/// Whether the EXIF orientation turns the image a quarter, so its width and height swap. It is
/// the one list of those orientations, for the view, the JPEG scaling and the save.
pub fn quarter_turn(o: Orientation) -> bool {
    use Orientation::{Rotate90, Rotate90FlipH, Rotate270, Rotate270FlipH};
    matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
}

/// Runs `work`, and returns an error if it panics. A decoder crashing on a broken file must not
/// end the worker thread, because that would leave every later image or thumbnail waiting for
/// ever.
pub fn survive<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    crate::support::threads::caught(work)
        .unwrap_or_else(|| Err("the decoder crashed on this file".into()))
}

/// As `read_limited`, as UTF-8 text.
pub fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    String::from_utf8(read_limited(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The whole of `path`, read only up to `MAX_DECODE` bytes. A larger file (a sparse one, or one
/// unpacked from a small zip) is refused before it fills memory, rather than read whole first.
/// So is what is not a file (a pipe would block for good, and /dev/zero would fill memory), also
/// through a link. This is the one way Glimt reads a file whole, whoever wrote it, and clippy
/// (clippy.toml) refuses every other read.
pub fn read_limited(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let too_large =
        || std::io::Error::other(format!("file too large ({})", human_size(MAX_DECODE)));
    let (file, meta) = open_file(path.as_ref())?;
    // The size is looked at first, so a large file is not read at all. `take` is for a file
    // that grows meanwhile.
    if meta.len() > MAX_DECODE {
        return Err(too_large());
    }
    let mut data = Vec::new();
    #[allow(clippy::disallowed_methods)] // bounded by take
    file.take(MAX_DECODE + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_DECODE {
        return Err(too_large());
    }
    Ok(data)
}

/// `path` opened to be read, if it is a file, with what it is. It is opened without waiting
/// (O_NONBLOCK, which a file does not heed), and what is open is looked at rather than the
/// name, because a pipe put there between a look and an open would block that open.
pub(crate) fn open_file(path: &Path) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NONBLOCK: i32 = 0o4000;
    #[allow(clippy::disallowed_methods)] // the one opening of a file to read it whole
    let file = (std::fs::OpenOptions::new().read(true))
        .custom_flags(O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

/// `path` opened to be read, if it is a file (`open_file`). This is the one way Glimt opens a
/// file, so a pipe or device in its place is refused, not waited on for good.
pub(crate) fn open(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    open_file(path.as_ref()).map(|(file, _)| file)
}

/// Returns an image's width and height as stored, read from its header where it can be: a
/// JPEG's from its first part, a camera RAW's from the preview it is shown by (its own header
/// gives the size of a small thumbnail), others from the decoder's header. An EXIF orientation
/// is not applied, so a photo turned upright reports the size the camera wrote. Returns `None`
/// for a file whose size cannot be read without decoding it (HEIC, AVIF, SVG, video).
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    if crate::model::listing::is_jpeg(path)
        && let Some(size) = jpeg_head_size(path)
    {
        return Some(size);
    }
    if has_ext(path, raw::EXTS) {
        // The head first, as for a thumbnail. Find Duplicates over a folder of 50 MB RAWs then
        // reads the whole of one only when its first part has no preview.
        if let Some(p) = raw::preview(&read_head(path, 8 << 20).ok()?) {
            return raw::jpeg_size(p);
        }
        let data = read_limited(path).ok()?;
        return raw::jpeg_size(raw::preview(&data)?);
    }
    image_reader(path).ok()?.into_dimensions().ok()
}

/// Returns a JPEG's width and height from its first 256 KB, where its frame header is, because
/// the decoder would read the whole file. Returns `None` if it is not there, so the decoder is
/// asked after all.
fn jpeg_head_size(path: &Path) -> Option<(u32, u32)> {
    let head = read_head(path, 1 << 18).ok()?;
    head.starts_with(&[0xff, 0xd8])
        .then(|| raw::jpeg_size(&head))?
}

/// An image crate reader on `path`, opened with `open` (no pipe or device). Its format is
/// guessed from the file's first bytes, and from the extension when they say nothing, as
/// `image::ImageReader::open` and `with_guessed_format` do. Clippy refuses `open`, because the
/// image crate opens the file itself, where clippy cannot look.
pub(crate) fn image_reader(
    path: impl AsRef<Path>,
) -> std::io::Result<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    let path = path.as_ref();
    let (file, meta) = open_file(path)?;
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file));
    if let Ok(format) = image::ImageFormat::from_path(path) {
        reader.set_format(format);
    }
    let reader = reader.with_guessed_format()?;
    // The image crate's JPEG decoder reads the whole file before any of its limits apply, so a
    // JPEG larger than a decode may take is refused here, as read_limited refuses it. The format
    // counts as the bytes tell it, so a JPEG named .png is refused too. Other formats are read by
    // seeking, so a large TIFF or PNG still opens. A file of no known format is refused as well.
    let whole = matches!(reader.format(), Some(image::ImageFormat::Jpeg) | None);
    if whole && meta.len() > MAX_DECODE {
        return Err(std::io::Error::other(format!(
            "file too large ({})",
            human_size(MAX_DECODE)
        )));
    }
    Ok(reader)
}

/// Scale down to fit within `max`.
pub fn shrink(img: image::DynamicImage, max: [u32; 2]) -> Result<image::DynamicImage, String> {
    if img.width() > max[0] || img.height() > max[1] {
        let scale = (max[0] as f64 / img.width() as f64).min(max[1] as f64 / img.height() as f64);
        let w = ((img.width() as f64 * scale).round() as u32).max(1);
        let h = ((img.height() as f64 * scale).round() as u32).max(1);
        let mut small = image::DynamicImage::new(w, h, img.color());
        // This uses SIMD downscaling, because image's thumbnail took about 150–330 ms on 12 MP.
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
    /// What is no file is refused before it is read, a pipe without blocking on it. That every
    /// whole-file read goes through `read_limited` is clippy's to hold (clippy.toml).
    #[test]
    fn read_limited_refuses_what_is_no_file() {
        // What is no file is refused, a pipe without blocking on it.
        let refused = super::read_limited("/dev/zero").unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        let fifo = std::env::temp_dir().join(format!("glimt-test-fifo-{}", std::process::id()));
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let refused = super::read_limited(&fifo).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        // Not through the image crate either, because a collection naming a pipe x.png held
        // Find Duplicates.
        let fifo_png = fifo.with_extension("png");
        std::fs::rename(&fifo, &fifo_png).unwrap();
        assert!(super::image_reader(&fifo_png).is_err());
        let fifo = fifo_png;
        std::fs::remove_file(fifo).unwrap();
    }

    /// The histogram counts alpha too, which tells a transparent image from an opaque one.
    #[test]
    fn counts_alpha() {
        let mut img = eframe::egui::ColorImage::new([2, 1], vec![eframe::egui::Color32::BLACK; 2]);
        assert_eq!(super::histogram(&img)[4][255], 2);
        img.pixels[1] = eframe::egui::Color32::TRANSPARENT;
        let h = super::histogram(&img);
        assert_eq!((h[4][0], h[4][255]), (1, 1));
    }

    /// A damaged JPEG is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_jpegs() {
        let mut jpeg = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        for bytes in crate::support::test_support::broken(&jpeg) {
            let _ = super::scaled(&bytes, [8, 8], true, None);
        }
    }

    /// A damaged TIFF or ICO is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_tiffs() {
        let dir = std::env::temp_dir().join(format!("glimt-test-btiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tiff = Vec::new();
        image::RgbImage::new(16, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut tiff),
                image::ImageFormat::Tiff,
            )
            .unwrap();
        let file = dir.join("a.tif");
        for bytes in crate::support::test_support::broken(&tiff) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::tiff_pages(&file);
            let _ = super::tiff_page(&file, 0);
        }
        let file = dir.join("a.ico");
        for bytes in crate::support::test_support::broken(&two_sizes_ico()) {
            std::fs::write(&file, &bytes).unwrap();
            for k in 0..super::pages(&file) {
                let _ = super::page_of(&file, k);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An ICO of a red 32×32 image and a blue 16×16.
    fn two_sizes_ico() -> Vec<u8> {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let frame = |side: u32, rgba: [u8; 4]| {
            let img = image::RgbaImage::from_pixel(side, side, image::Rgba(rgba));
            IcoFrame::as_png(&img, side, side, image::ExtendedColorType::Rgba8).unwrap()
        };
        let mut out = Vec::new();
        IcoEncoder::new(&mut out)
            .encode_images(&[frame(32, [255, 0, 0, 255]), frame(16, [0, 0, 255, 255])])
            .unwrap();
        out
    }

    /// An ICO of more than 256 sizes, with the best one last, shows it first, and each size is
    /// a page.
    #[test]
    fn reads_every_size_of_a_large_ico() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        let frame = |side: u32| {
            let img = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 255, 255]));
            IcoFrame::as_png(&img, side, side, image::ExtendedColorType::Rgba8).unwrap()
        };
        let mut frames: Vec<_> = (0..256).map(|_| frame(16)).collect();
        frames.push(frame(32));
        let mut out = Vec::new();
        IcoEncoder::new(&mut out).encode_images(&frames).unwrap();
        let dir = std::env::temp_dir().join(format!("glimt-test-bigico-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ico = dir.join("many.ico");
        std::fs::write(&ico, out).unwrap();
        assert_eq!(super::pages(&ico), 257);
        assert_eq!(super::page_of(&ico, 0).unwrap().width(), 32);
        assert_eq!(super::page_of(&ico, 256).unwrap().width(), 16);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A damaged GIF is refused or shown as far as it goes, never a panic.
    #[test]
    fn survives_broken_gifs() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bgif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gif = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut gif);
            for k in 0..2u8 {
                let frame = image::RgbaImage::from_pixel(4, 4, image::Rgba([k * 100, 0, 0, 255]));
                enc.encode_frame(image::Frame::new(frame)).unwrap();
            }
        }
        let file = dir.join("a.gif");
        for bytes in crate::support::test_support::broken(&gif) {
            std::fs::write(&file, &bytes).unwrap();
            let _ = super::animation(&file, [8, 8]);
        }
        // One cut off inside its last frame keeps the frames before it.
        let mut cut = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut cut);
            for k in 0..3u32 {
                let noise =
                    (0..32 * 32 * 4).map(|i: u32| (i.wrapping_mul(2654435761 + k) >> 24) as u8);
                let frame = image::RgbaImage::from_raw(32, 32, noise.collect()).unwrap();
                enc.encode_frame(image::Frame::new(frame)).unwrap();
            }
        }
        cut.truncate(cut.len() - 100);
        std::fs::write(&file, &cut).unwrap();
        let (frames, _) = super::animation(&file, [32, 32]).unwrap();
        assert_eq!(frames.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// There is one small decode for thumbnails and prints. It fits within the box, brings the
    /// profile along, and gives the size as the image stands upright, before it was shrunk.
    #[test]
    fn small_decodes_within_the_box() {
        let dir = std::env::temp_dir().join(format!("glimt-test-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        image::RgbImage::new(400, 300).save(&png).unwrap();
        let (img, icc, size) = super::small(&png, [100, 100], true).unwrap();
        assert!(img.width() <= 100 && img.height() <= 100 && icc.is_none());
        assert_eq!(size, [400, 300]);
        let jpeg = dir.join("b.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        crate::metadata::exif::write_orientation(&jpeg, 1, false, true).unwrap();
        let (img, _, size) = super::small(&jpeg, [100, 100], true).unwrap();
        assert_eq!((img.width(), img.height(), size), (20, 40, [20, 40]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A photo turned by EXIF is shown turned, at its turned size, when it is shrunk before the
    /// turn. For orientation 6, the left of the stored pixels is at the top.
    #[test]
    fn shows_turned_photos_shrunk_first() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::images::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-turned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.png");
        // 400×200, red on the left and blue on the right, with EXIF orientation 6.
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";
        let img = image::RgbImage::from_fn(400, 200, |x, _| match x < 200 {
            true => image::Rgb([255, 0, 0]),
            false => image::Rgb([0, 0, 255]),
        });
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&file).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        img.write_with_encoder(enc).unwrap();
        let (frames, orig, _) = super::decode(&file, [50, 50], true, 0).unwrap();
        let shown = &frames[0].0;
        assert_eq!(orig, [200, 400]);
        assert_eq!(shown.size, [25, 50]);
        let (top, bottom) = (shown.pixels[0], shown.pixels[shown.pixels.len() - 1]);
        assert!(top.r() > 200 && top.b() < 50, "{top:?}");
        assert!(bottom.b() > 200 && bottom.r() < 50, "{bottom:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Every frame of a GIF and every page of a TIFF, one at a time, or only the one asked for.
    #[test]
    fn gives_each_frame() {
        // The ICO's colours go through the colour settings, which another test changes.
        let _held = crate::images::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glimt-test-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gif = std::fs::File::create(dir.join("a.gif")).unwrap();
        let frame = |v| {
            let delay = image::Delay::from_numer_denom_ms(100, 1);
            image::Frame::from_parts(
                image::RgbaImage::from_pixel(8, 4, image::Rgba([v, 0, 0, 255])),
                0,
                0,
                delay,
            )
        };
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frames([frame(10), frame(120), frame(250)])
            .unwrap();
        let mut seen = Vec::new();
        let n = super::each_frame(&dir.join("a.gif"), None, true, |k, img| {
            seen.push((k, img.to_rgb8().get_pixel(0, 0)[0] / 100));
            Ok(())
        });
        assert_eq!((n, seen), (Ok(3), vec![(0, 0), (1, 1), (2, 2)]));
        let mut only = Vec::new();
        let n = super::each_frame(&dir.join("a.gif"), Some(1), true, |k, _| {
            only.push(k);
            Ok(())
        });
        assert_eq!((n, only), (Ok(1), vec![1]));
        // A TIFF of two pages.
        let file = std::fs::File::create(dir.join("b.tif")).unwrap();
        let mut tiff = tiff::encoder::TiffEncoder::new(file).unwrap();
        for v in [0u8, 200] {
            tiff.write_image::<tiff::encoder::colortype::Gray8>(4, 4, &[v; 16])
                .unwrap();
        }
        let mut pages = Vec::new();
        let n = super::each_frame(&dir.join("b.tif"), None, true, |k, img| {
            pages.push((k, img.to_luma8().get_pixel(0, 0)[0]));
            Ok(())
        });
        assert_eq!((n, pages), (Ok(2), vec![(0, 0), (1, 200)]));
        // An ICO of two sizes shows the largest first, as the image crate shows it, and then
        // the other.
        let ico = dir.join("b.ico");
        std::fs::write(&ico, two_sizes_ico()).unwrap();
        assert_eq!(super::pages(&ico), 2);
        let mut sizes = Vec::new();
        let n = super::each_frame(&ico, None, true, |k, img| {
            sizes.push((k, img.width(), img.to_rgba8().get_pixel(0, 0).0));
            Ok(())
        });
        assert_eq!(
            (n, sizes),
            (
                Ok(2),
                vec![(0, 32, [255, 0, 0, 255]), (1, 16, [0, 0, 255, 255])]
            )
        );
        let (_, orig, pages) = super::decode(&ico, [100, 100], true, 1).unwrap();
        assert_eq!((orig, pages), ([16, 16], 2));
        // A picture with EXIF orientation 6 is turned upright with Auto Rotate on, and stays as
        // it is with it off.
        use image::ImageEncoder;
        let exif = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";
        let turned = dir.join("c.png");
        let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(&turned).unwrap());
        enc.set_exif_metadata(exif.to_vec()).unwrap();
        image::RgbImage::new(4, 2).write_with_encoder(enc).unwrap();
        for (rotate, size) in [(true, (2, 4)), (false, (4, 2))] {
            let mut got = (0, 0);
            super::each_frame(&turned, Some(0), rotate, |_, img| {
                got = (img.width(), img.height());
                Ok(())
            })
            .unwrap();
            assert_eq!(got, size, "rotate {rotate}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
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
    /// its full upright size. One that needs more than half its pixels is left to image.
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
        // With a quarter turn clockwise (EXIF orientation 6), the left half ends up on top.
        crate::metadata::exif::write_orientation(&file, 1, false, true).unwrap();
        let (img, _, orig) = super::scaled_jpeg(&file, [60, 100], true).unwrap();
        assert_eq!(orig, [200, 400]);
        assert_eq!((img.width(), img.height()), (50, 100));
        let rgb = img.to_rgb8();
        let (top, bottom) = (rgb.get_pixel(25, 10), rgb.get_pixel(25, 90));
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        // Not turned, it has the stored size, and at more than half there is no gain, so None.
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

    /// Grey TIFFs of floats, as science writes them, are shown stretched from the lowest value
    /// to the highest, and a float RGB one as the image crate reads it.
    #[test]
    fn shows_float_tiff() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::images::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

    /// A Radiance HDR far brighter than a screen is shown with its shadows dark and its
    /// highlights rolled off, and nothing but the brightest is cut to white.
    #[test]
    fn tone_maps_radiance_hdr() {
        // The test reads pixel values, so no other test may change the colour settings
        // meanwhile.
        let _held = crate::images::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glimt-test-hdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::Rgb32FImage::from_fn(100, 10, |x, _| {
            let v = 0.01 * 1.1f32.powi(x as i32); // 0.01 up to about 125
            image::Rgb([v, v, v])
        });
        image::DynamicImage::ImageRgb32F(img)
            .save(dir.join("a.hdr"))
            .unwrap();
        let (frames, orig, _) = super::decode(&dir.join("a.hdr"), [100, 100], true, 0).unwrap();
        let px = &frames[0].0.pixels;
        assert_eq!(orig, [100, 10]);
        let (dark, mid, bright) = (px[0].r(), px[50].r(), px[99].r());
        assert!(
            dark < 40 && (60..230).contains(&mid) && bright > 230,
            "{dark} {mid} {bright}"
        );
        assert!(px[90].r() < bright);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// TIFFs under other names, as microscopes (Zeiss .lsm) and gel scanners (.gel) write them,
    /// are listed and shown, also grey floats.
    #[test]
    fn shows_lsm_and_gel() {
        use tiff::encoder::{TiffEncoder, colortype};
        let dir = std::env::temp_dir().join(format!("glimt-test-lsm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let grey: Vec<u16> = (0..60 * 30).map(|i| i as u16 * 30).collect();
        let file = std::fs::File::create(dir.join("a.gel")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray16>(60, 30, &grey)
            .unwrap();
        let floats: Vec<f32> = (0..60 * 30).map(|i| i as f32).collect();
        let file = std::fs::File::create(dir.join("b.lsm")).unwrap();
        (TiffEncoder::new(file).unwrap())
            .write_image::<colortype::Gray32Float>(60, 30, &floats)
            .unwrap();
        let types = crate::model::listing::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (_, images) = crate::model::listing::list_folder(&dir, types);
        assert_eq!(images, ["a.gel", "b.lsm"].map(std::ffi::OsString::from));
        for name in images {
            let (frames, orig, _) = super::decode(&dir.join(name), [100, 100], true, 0).unwrap();
            assert_eq!((frames[0].0.size, orig), ([60, 30], [60, 30]));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Check Images passes a whole photo. One cut off in copying, a picture that is no picture
    /// and an empty file are named broken.
    #[test]
    fn checks_images() {
        let dir = std::env::temp_dir().join(format!("glimt-test-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut jpeg = Vec::new();
        let img = image::RgbImage::from_fn(300, 200, |x, y| image::Rgb([x as u8, y as u8, 7]));
        (img.write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        ))
        .unwrap();
        std::fs::write(dir.join("whole.jpg"), &jpeg).unwrap();
        std::fs::write(dir.join("cut.jpg"), &jpeg[..jpeg.len() * 2 / 3]).unwrap();
        // A motion photo has a video after the JPEG's end, longer than 64 KB and without FF D9.
        let motion = [&jpeg[..], &vec![0x42; 200_000]].concat();
        std::fs::write(dir.join("motion.jpg"), motion).unwrap();
        assert_eq!(super::check(&dir.join("motion.jpg")), Ok(()));
        std::fs::write(dir.join("text.png"), b"not a picture").unwrap();
        std::fs::write(dir.join("empty.webp"), b"").unwrap();
        assert_eq!(super::check(&dir.join("whole.jpg")), Ok(()));
        for broken in ["cut.jpg", "text.png", "empty.webp"] {
            assert!(super::check(&dir.join(broken)).is_err(), "{broken}");
        }
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
        let types = crate::model::listing::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (_, images) = crate::model::listing::list_folder(&dir, types);
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
        let mut all = crate::model::listing::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let (dirs, images) = crate::model::listing::list_folder(&dir, all);
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        // Hidden files and folders, when shown.
        all.hidden = true;
        let (dirs, images) = crate::model::listing::list_folder(&dir, all);
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
        // A GIF whose canvas claims 65535×65535 is refused before a frame is made, because each
        // frame would be a canvas of 17 GB.
        let huge = dir.join("huge.gif");
        let mut gif = b"GIF89a\xff\xff\xff\xff\0\0\0".to_vec();
        gif.extend(b",\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0;");
        std::fs::write(&huge, gif).unwrap();
        assert!(super::animation(&huge, [100, 100]).is_none());
        assert!(super::decode(&huge, [100, 100], true, 0).is_err());
        std::fs::remove_file(huge).unwrap();
        // In the tree, a folder reached again through a link is read once.
        std::fs::copy(dir.join("a.png"), dir.join("sub/c.png")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/back")).unwrap();
        assert_eq!(
            crate::model::listing::list_tree(
                &dir,
                crate::model::listing::Types {
                    images: true,
                    raw: true,
                    videos: true,
                    hidden: false
                },
                &Default::default(),
                |_| {},
                |_| {}
            ),
            (
                vec![dir.join("sub")],
                ["a.png", "b.gif", "sub/c.png"].map(Into::into).to_vec()
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file larger than Glimt decodes is refused before it is read whole, so a sparse one of
    /// 8 GB takes no memory.
    #[test]
    fn refuses_huge_files() {
        let file = std::env::temp_dir().join(format!("glimt-test-huge-{}.jpg", std::process::id()));
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(8 << 30).unwrap();
        assert!(super::read_limited(&file).is_err());
        // Check Images and the EXIF of a RAW read through it too, never the whole 8 GB.
        assert!(super::jpeg_ends(&file).is_err());
        // And every decode through the image crate, whose JPEG decoder reads the file whole.
        assert!(super::image_reader(&file).is_err());
        // A TIFF is read by seeking, so a large one still opens.
        let tif = file.with_extension("tif");
        std::fs::hard_link(&file, &tif).unwrap();
        assert!(super::image_reader(&tif).is_ok());
        std::fs::remove_file(&tif).unwrap();
        // A JPEG named .png is told by its bytes, and refused as a JPEG.
        let png = file.with_extension("png");
        let mut g = std::fs::File::create(&png).unwrap();
        std::io::Write::write_all(&mut g, &[0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        g.set_len(8 << 30).unwrap();
        assert!(super::image_reader(&png).is_err());
        std::fs::remove_file(&png).unwrap();
        let raf = file.with_extension("raf");
        std::fs::hard_link(&file, &raf).unwrap();
        assert!(crate::images::raw::exif(&raf).is_none());
        std::fs::remove_file(&raf).unwrap();
        f.set_len(10).unwrap();
        assert_eq!(super::read_limited(&file).unwrap().len(), 10);
        std::fs::remove_file(file).unwrap();
    }

    /// A JPEG's size comes from its frame header, width first. A camera RAW's size is its
    /// preview's, not that of the small thumbnail its TIFF header describes, else a folder of
    /// NEFs would be one group of the same dimensions in Find Duplicates. It is read from the
    /// head of the file, so one of 8 GB, too large to read whole, still has a size.
    #[test]
    fn reads_dimensions_from_headers() {
        let dir = std::env::temp_dir().join(format!("glimt-test-dims-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("e.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        assert_eq!(super::dimensions(&jpeg), Some((40, 20)));
        let encoded = |w, h, format| {
            let mut bytes = Vec::new();
            (image::RgbImage::new(w, h))
                .write_to(&mut std::io::Cursor::new(&mut bytes), format)
                .unwrap();
            bytes
        };
        let mut nef = encoded(16, 12, image::ImageFormat::Tiff);
        nef.extend(encoded(64, 48, image::ImageFormat::Jpeg));
        let raw = dir.join("f.nef");
        std::fs::write(&raw, nef).unwrap();
        assert_eq!(super::dimensions(&raw), Some((64, 48)));
        #[allow(clippy::disallowed_methods)] // a test's own file, made sparse
        let file = std::fs::File::options().write(true).open(&raw).unwrap();
        file.set_len(8 << 30).unwrap();
        assert_eq!(super::dimensions(&raw), Some((64, 48)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The profile inside a file is found for the info panel, and a file without one has none.
    #[test]
    fn finds_the_profile_inside() {
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("glimt-test-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (with, without) = (dir.join("with.png"), dir.join("without.png"));
        let mut out = Vec::new();
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        enc.set_icc_profile(super::ADOBE_RGB.to_vec()).unwrap();
        enc.write_image(&[0u8; 2 * 2 * 3], 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        std::fs::write(&with, out).unwrap();
        image::RgbImage::new(2, 2).save(&without).unwrap();
        assert_eq!(
            super::icc_profile(&with).as_deref(),
            Some(&super::ADOBE_RGB[..])
        );
        assert_eq!(super::icc_profile(&without), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn survives_a_panic() {
        assert_eq!(super::survive(|| Ok::<_, String>(1)), Ok(1));
        assert!(super::survive(|| -> Result<(), String> { panic!("broken file") }).is_err());
    }
}
