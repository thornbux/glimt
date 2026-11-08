//! SVG and SVGZ through resvg (feature `svg`), with limits on what a file may make it do: the
//! text unpacked, the pictures it links to, and entities that grow into gigabytes. resvg draws
//! in a child process of Glimt's own program, which has a limit of memory and time, because
//! what resvg allocates and how long it takes are set by the file, not by Glimt.

use crate::images::decode::{MAX_DECODE_BYTES, read_limited};
use std::path::Path;

/// The environment variable that makes Glimt draw the SVG it names for the Glimt that started
/// it, write the picture on its output and end, instead of opening a window.
pub const DRAW_SVG: &str = "GLIMT_DRAW_SVG";

/// The environment variable with the box that the SVG of `DRAW_SVG` is drawn to fit, as
/// "WIDTH HEIGHT". Without it, the SVG is drawn at its own size.
const DRAW_SVG_BOX: &str = "GLIMT_DRAW_SVG_BOX";

/// The memory a drawing child may take, as much as one decode may. resvg draws each group with
/// opacity, a filter, a mask or a clip into a layer of its own, of up to 25 times the canvas,
/// and nested groups each keep theirs while the inner ones are drawn. So a file of 1 KB could
/// otherwise take gigabytes, and the system would end Glimt for it. Patterns, filters and a
/// `<use>` repeated a million times are more ways to the same end. A child that needs more is
/// ended, and the window only tells why.
const CHILD_MEMORY: u64 = MAX_DECODE_BYTES;

/// The longest side a drawing has, in pixels, because an SVG may claim any size.
const LONGEST_SIDE: u32 = 8192;

/// The mark in front of the picture on a drawing child's output.
const PICTURE_MARK: &[u8] = b"GLIMT-SVG-PICTURE";

/// The mark in front of the reason on a drawing child's output, when it has no picture.
const ERROR_MARK: &[u8] = b"GLIMT-SVG-ERROR";

/// The arguments a drawing child is started with, besides `DRAW_SVG`. Glimt's own program needs
/// none. In the tests, the program is the test binary, and these make it run only the test
/// `draws_for_the_parent`, which draws as Glimt's own child does.
#[cfg(not(test))]
const CHILD_ARGS: &[&str] = &[];
#[cfg(test)]
const CHILD_ARGS: &[&str] = &[
    "--exact",
    "images::svg::tests::draws_for_the_parent",
    "--test-threads=1",
    "--quiet",
];

/// usvg's own ways to fetch the pictures an SVG shows, with Glimt's limits first. A linked
/// picture is taken only if it is a file under 64 MB (not /dev/zero or a pipe), and no picture
/// whose header claims more pixels than an image Glimt decodes may have.
fn svg_images() -> resvg::usvg::ImageHrefResolver<'static> {
    use resvg::usvg::ImageHrefResolver as R;
    // One SVG may have at most 1 GB of decoded pictures, and 256 MB of them read. A small SVG
    // may name the same picture a thousand times, and resvg keeps each copy read and decodes
    // each again. A plain picture of 8000×8000 is 250 KB on disk but 256 MB decoded. A picture
    // that is refused counts for nothing.
    let spent = std::sync::Arc::new(std::sync::Mutex::new((0u64, 0u64)));
    let fits = move |data: &[u8]| {
        let reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format();
        let decoded = match reader.ok().and_then(|r| r.into_dimensions().ok()) {
            Some((w, h)) if !crate::images::decode::fits_decode_limit(w, h, 4) => return false,
            Some((w, h)) => u64::from(w) * u64::from(h) * 4,
            // Data that image does not read as a picture, such as an SVG, goes to usvg, which
            // reads it with these limits. But usvg unpacks a gzipped one (svgz) without a limit.
            None if data.starts_with(&[0x1f, 0x8b]) => return false,
            None if entities_too_large(data) => return false,
            None => data.len() as u64,
        };
        let mut spent = spent.lock().unwrap_or_else(|e| e.into_inner());
        let (pixels, bytes) = (
            spent.0.saturating_add(decoded),
            spent.1.saturating_add(data.len() as u64),
        );
        let ok = pixels <= 1 << 30 && bytes <= 256 << 20;
        if ok {
            *spent = (pixels, bytes);
        }
        ok
    };
    let fits_too = fits.clone();
    let (data, data_too) = (R::default_data_resolver(), R::default_data_resolver());
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
            if meta.len() > 64 << 20 {
                return None;
            }
            // The bytes checked are the bytes decoded. usvg's own string resolver would open
            // the file again, and a file swapped in between would get past the limits. As
            // "text/plain", usvg tells the format by the bytes, as it does for a data URL.
            let bytes = read_limited(&file).ok().filter(|b| fits_too(b))?;
            data_too("text/plain", std::sync::Arc::new(bytes), opts)
        }),
    }
}

/// Whether the entities an SVG defines may grow its text past 64 MB once usvg expands them.
/// 100 KB of a long entity used through a short one is gigabytes, which aborts the process.
/// The bound assumes every & is a reference to the longest entity, which may hold up to 256 of
/// them. An SVG with a few short ones (Illustrator's namespaces) passes.
///
/// The DTD's structure is not read, because a reader that differs from roxmltree's can be led
/// past an entity (by a `]` or a quote in another declaration, or a DOCTYPE in a comment).
/// Instead each `<!ENTITY` anywhere is measured. Its value is the first quoted text after it,
/// before any `<`, because a name cannot hold a quote. An entity in a comment counts too, which
/// only overestimates.
fn entities_too_large(data: &[u8]) -> bool {
    let (mut longest, mut nested) = (0u64, false);
    let mut rest = data;
    while let Some(at) = rest.windows(8).position(|w| w == b"<!ENTITY") {
        rest = &rest[at + 8..];
        let quote =
            (rest.iter().take_while(|&&b| b != b'<')).position(|&b| b == b'"' || b == b'\'');
        let Some(open) = quote else { continue };
        let value = &rest[open + 1..];
        let len = value
            .iter()
            .position(|&b| b == rest[open])
            .unwrap_or(value.len());
        longest = longest.max(len as u64);
        nested |= value[..len].contains(&b'&');
    }
    let refs = data.iter().filter(|&&b| b == b'&').count() as u64;
    let grown = refs.saturating_mul(longest);
    longest > 0 && grown.saturating_mul(if nested { 256 } else { 1 }) > 64 << 20
}

/// Gzipped `data` unpacked, or an error once it would be larger than `limit` bytes.
fn gunzip(data: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut reader = flate2::read::GzDecoder::new(data).take(limit + 1);
    #[allow(clippy::disallowed_methods)] // bounded by take
    reader.read_to_end(&mut out).map_err(|e| e.to_string())?;
    match out.len() as u64 > limit {
        true => Err("SVG too large once unpacked".into()),
        false => Ok(out),
    }
}

/// Draws an SVG fitted in `max`, or else at its own size but at least 2048 pixels on the longer
/// side, so that it stays sharp when fitted to the window. Returns the picture, and with `max`
/// also the size it has without `max`, which the view counts as its own. It is drawn in a
/// child process (`draw_in_child`), which is ended after `PROGRAM_TIME_LIMIT`.
pub fn load(
    path: &Path,
    max: Option<[u32; 2]>,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    draw_in_child(path, max, crate::files::external::PROGRAM_TIME_LIMIT)
}

/// Draws an SVG as `load` does, in a child process of Glimt's own program that may take
/// `CHILD_MEMORY` and is ended after `limit`. The child is `/proc/self/exe`, which is this
/// program also when its file has been replaced by an update since it started.
fn draw_in_child(
    path: &Path,
    max: Option<[u32; 2]>,
    limit: std::time::Duration,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    // The picture is at most the box, or the longest side squared, with a pixel more on each
    // side where the size is rounded up.
    let side = |s: u32| u64::from(s.min(LONGEST_SIDE)) + 1;
    let [w, h] = max.unwrap_or([LONGEST_SIDE; 2]);
    let picture = side(w) * side(h) * 4;
    let mut command = crate::files::external::stoppable_command("/proc/self/exe");
    command.args(CHILD_ARGS).env(DRAW_SVG, path);
    if let Some([w, h]) = max {
        command.env(DRAW_SVG_BOX, format!("{w} {h}"));
    }
    let mut child = command.spawn().map_err(|e| format!("SVG: {e}"))?;
    let (out, errors, _) = crate::files::external::output_of(&mut child, picture + 4096, limit);
    read_child_answer(out.ok_or("SVG too large")?, &errors)
}

/// Returns the picture that a drawing child wrote on its output `out`, with its own size, or
/// the reason the child gave. When it wrote neither, returns why it ended, from its `errors`.
/// The marks are looked for in the first 4 KB, because in the tests the child's output starts
/// with what the test binary itself says.
fn read_child_answer(
    mut out: Vec<u8>,
    errors: &str,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    let head = &out[..out.len().min(4096)];
    let find = |mark: &[u8]| head.windows(mark.len()).position(|w| w == mark);
    if let Some(at) = find(ERROR_MARK) {
        let text = &out[at + ERROR_MARK.len()..];
        let end = text.iter().position(|&b| b == b'\n').unwrap_or(text.len());
        return Err(String::from_utf8_lossy(&text[..end]).into_owned());
    }
    let Some(at) = find(PICTURE_MARK) else {
        use crate::files::external::TIMED_OUT;
        return Err(match errors {
            e if e.contains(TIMED_OUT) => format!("drawing the SVG {TIMED_OUT}"),
            // These are Rust's own words when an allocation fails, here past the limit.
            e if e.contains("memory allocation of") => "SVG needs too much memory to draw".into(),
            e => crate::files::external::last_line(e, "SVG could not be drawn"),
        });
    };
    let start = at + PICTURE_MARK.len();
    let number = |k: usize| {
        let bytes = out.get(start + 4 * k..start + 4 * k + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    };
    let (Some(w), Some(h), Some(own_w), Some(own_h)) = (number(0), number(1), number(2), number(3))
    else {
        return Err("SVG cut short".into());
    };
    // The picture's bytes are moved to the front of what was read, not copied.
    out.drain(..start + 16);
    out.truncate(w as usize * h as usize * 4);
    let img = image::RgbaImage::from_raw(w, h, out).ok_or("SVG cut short")?;
    let own = (own_w > 0).then_some([own_w as usize, own_h as usize]);
    Ok((img.into(), own))
}

/// Draws the SVG at `path` (from `DRAW_SVG`) for the Glimt that started this process, under the
/// limit of `CHILD_MEMORY`, and writes the picture, or why there is none, on the output.
/// Returns the exit code.
pub fn draw_for_parent(path: &std::ffi::OsStr) -> i32 {
    use std::io::Write;
    limit_this_process(CHILD_MEMORY);
    let max = std::env::var(DRAW_SVG_BOX).ok().and_then(|text| {
        let (w, h) = text.split_once(' ')?;
        Some([w.parse().ok()?, h.parse().ok()?])
    });
    let mut out = std::io::stdout().lock();
    let written = match draw(Path::new(path), max) {
        Ok((img, own)) => {
            let [own_w, own_h] = own.map_or([0, 0], |[w, h]| [w as u32, h as u32]);
            let numbers = [img.width(), img.height(), own_w, own_h].map(u32::to_le_bytes);
            (out.write_all(PICTURE_MARK))
                .and_then(|()| out.write_all(&numbers.concat()))
                .and_then(|()| out.write_all(img.as_raw()))
        }
        Err(e) => writeln!(out, "{}{e}", String::from_utf8_lossy(ERROR_MARK)),
    };
    i32::from(written.and_then(|()| out.flush()).is_err())
}

/// Limits this process's memory (its address space) to `bytes`, so that an allocation past it
/// fails and ends the process. Core dumps are turned off too, so such an end leaves no file.
fn limit_this_process(bytes: u64) {
    let limit = |max: u64| libc::rlimit {
        rlim_cur: max,
        rlim_max: max,
    };
    // SAFETY: setrlimit only reads the limit it is given, and it changes only this process.
    unsafe {
        libc::setrlimit(libc::RLIMIT_AS, &limit(bytes));
        libc::setrlimit(libc::RLIMIT_CORE, &limit(0));
    }
}

/// Draws an SVG with resvg in this process, as `load` describes. Only a drawing child calls it
/// (`draw_for_parent`), under its limit of memory.
fn draw(
    path: &Path,
    max: Option<[u32; 2]>,
) -> Result<(image::RgbaImage, Option<[usize; 2]>), String> {
    use resvg::{tiny_skia, usvg};
    let mut data = read_limited(path).map_err(|e| e.to_string())?;
    // usvg unpacks a .svgz without a limit, and 10 MB of zeros would be 10 GB.
    if data.starts_with(&[0x1f, 0x8b]) {
        data = gunzip(&data, 64 << 20)?;
        // A file gzipped twice is refused, because usvg would unpack the inner one without a
        // limit.
        if data.starts_with(&[0x1f, 0x8b]) {
            return Err("SVG gzipped twice".into());
        }
    }
    if entities_too_large(&data) {
        return Err("SVG whose entities would grow too large".into());
    }
    // The system fonts are read for text, which takes tens of milliseconds in every child.
    // shortcut: they are read only for an SVG with a text element, so text in an SVG that this
    // one links to as a picture is not drawn; read them always if such files turn up.
    let mut fonts = usvg::fontdb::Database::new();
    if data.windows(5).any(|w| w == b"<text" || w == b":text") {
        fonts.load_system_fonts();
    }
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: std::sync::Arc::new(fonts),
        image_href_resolver: svg_images(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(&data, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    // The longer side has at most `LONGEST_SIDE` pixels, because the pixmap is 4 bytes per
    // pixel (60000×60000 would be 14 GB).
    let longest = LONGEST_SIDE as f32;
    let longer = size.width().max(size.height());
    let own = (2048.0 / longer).max(1.0).min(longest / longer);
    // It is drawn at the size it is shown, not at 2048 and shrunk, because an SVG of a few
    // bytes may claim 100000×100000, which would be 256 MB for each image read ahead.
    let scale = match max {
        Some([w, h]) => (w as f32 / size.width()).min(h as f32 / size.height()),
        None => own,
    }
    .min(longest / longer);
    let own_size = |side: f32| (side * own).ceil().max(1.0) as usize;
    let own_size = max.map(|_| [own_size(size.width()), own_size(size.height())]);
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
    // tiny-skia keeps premultiplied alpha, and image wants it straight. It is changed in
    // place, so the picture is not held twice.
    let mut rgba = pixmap.take();
    for p in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(p[3]);
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let img = image::RgbaImage::from_raw(w.max(1), h.max(1), rgba).ok_or("SVG size mismatch")?;
    Ok((img, own_size))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// This is not a test of its own. The tests' `load` starts the test binary as its drawing
    /// child, which runs only this, and it draws there as Glimt's own child does.
    #[test]
    fn draws_for_the_parent() {
        if let Some(path) = std::env::var_os(super::DRAW_SVG) {
            super::draw_for_parent(&path);
        }
    }

    /// An SVG of 40 nested groups with opacity around a huge rectangle would make resvg keep 40
    /// layers of 25 times the canvas, 8 GB for a window of 1920×1080, and one whose `<use>`
    /// repeats a group 100,000 times would take minutes. Both are refused in seconds, by the
    /// child's limits, and the test process (as Glimt) goes on. An SVG with a few such groups
    /// is still drawn.
    #[test]
    fn refuses_an_svg_that_takes_too_much_memory_or_time() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-cost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = |body: &str| {
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" \
                 xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"100\" height=\"100\">\
                 {body}</svg>"
            )
        };
        let rect = "<rect x=\"-1e7\" y=\"-1e7\" width=\"2e7\" height=\"2e7\"/>";
        let nested = svg(&format!(
            "{}{rect}{}",
            "<g opacity=\"0.9\">".repeat(40),
            "</g>".repeat(40)
        ));
        let mut repeated = "<defs><rect id=\"g0\" width=\"100\" height=\"100\" \
                            opacity=\"0.5\"/>"
            .to_string();
        // usvg itself refuses a million elements, so this is 100,000, still minutes of layers.
        for level in 1..=5 {
            let uses = format!("<use xlink:href=\"#g{}\"/>", level - 1).repeat(10);
            repeated += &format!("<g id=\"g{level}\">{uses}</g>");
        }
        let repeated = svg(&(repeated + "</defs><use xlink:href=\"#g5\"/>"));
        let limit = Duration::from_secs(5);
        for (name, text) in [("nested.svg", nested), ("repeated.svg", repeated)] {
            let file = dir.join(name);
            std::fs::write(&file, text).unwrap();
            let started = Instant::now();
            let why = super::draw_in_child(&file, Some([1920, 1080]), limit).unwrap_err();
            let ended = why == "SVG needs too much memory to draw"
                || why.contains(crate::files::external::TIMED_OUT);
            assert!(ended, "{name}: {why}");
            assert!(
                started.elapsed() < 2 * limit,
                "{name}: {:?}",
                started.elapsed()
            );
        }
        // Red at a quarter of its strength, through two groups at half opacity.
        let few = svg("<g opacity=\"0.5\"><g opacity=\"0.5\">\
                       <rect width=\"100\" height=\"100\" fill=\"red\"/></g></g>");
        let file = dir.join("few.svg");
        std::fs::write(&file, few).unwrap();
        let (img, own) = super::draw_in_child(&file, Some([50, 50]), limit).unwrap();
        assert_eq!(
            (img.width(), img.height(), own),
            (50, 50, Some([2048, 2048]))
        );
        let [r, g, b, a] = img.to_rgba8().get_pixel(25, 25).0;
        assert_eq!([r, g, b], [255, 0, 0]);
        assert!((60..=68).contains(&a), "{a}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What a drawing child wrote is read as its picture, its reason or its end, also after
    /// other text in front, and a picture cut short is an error rather than a wrong image.
    #[test]
    fn reads_what_a_drawing_child_wrote() {
        let mut out = b"running 1 test\n".to_vec();
        out.extend(super::PICTURE_MARK);
        for n in [2u32, 1, 0, 0] {
            out.extend(n.to_le_bytes());
        }
        out.extend([1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let (img, own) = super::read_child_answer(out.clone(), "").unwrap();
        assert_eq!(img.to_rgba8().into_raw(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(own, None);
        out.truncate(out.len() - 2);
        assert!(super::read_child_answer(out, "").is_err());
        let mut out = super::ERROR_MARK.to_vec();
        out.extend(b"SVG gzipped twice\n");
        assert_eq!(
            super::read_child_answer(out, "").unwrap_err(),
            "SVG gzipped twice"
        );
        let timed_out = format!("x\n{}", crate::files::external::TIMED_OUT);
        let why = super::read_child_answer(Vec::new(), &timed_out).unwrap_err();
        assert!(why.contains(crate::files::external::TIMED_OUT), "{why}");
    }

    /// A .svgz is unpacked only up to the limit, so a few KB of gzipped zeros cannot take
    /// gigabytes.
    #[test]
    fn unpacks_svgz_within_a_limit() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&[b' '; 5000]).unwrap();
        let packed = gz.finish().unwrap();
        assert_eq!(super::gunzip(&packed, 5000).map(|d| d.len()), Ok(5000));
        assert!(super::gunzip(&packed, 4999).is_err());
        assert!(super::gunzip(&packed[..10], 5000).is_err());
        // A file gzipped twice is refused, not handed to usvg's own unpacking.
        let mut twice = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        twice.write_all(&packed).unwrap();
        let file = std::env::temp_dir().join(format!("glimt-twice-{}.svgz", std::process::id()));
        std::fs::write(&file, twice.finish().unwrap()).unwrap();
        assert_eq!(
            super::load(&file, None).err().as_deref(),
            Some("SVG gzipped twice")
        );
        std::fs::remove_file(file).unwrap();
    }

    /// An SVG that points at /dev/zero, or at a picture claiming 100,000×100,000 pixels, is
    /// drawn without them, at once.
    #[test]
    fn svg_pictures_have_limits() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A PNG header claims 100,000 × 100,000. Its CRC is right, so readers believe it.
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
        assert!(super::load(&svg, None).is_ok());
        // For a thumbnail, at the thumbnail's size.
        let (thumb, _) = super::load(&svg, Some([128, 128])).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (128, 128));
        // One picture of 4 MB decoded is named again and again, and it is taken until a budget
        // is spent.
        let noise: Vec<u8> = (0..1024 * 1024 * 4u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        image::RgbaImage::from_raw(1024, 1024, noise)
            .unwrap()
            .save(dir.join("big.png"))
            .unwrap();
        let resolver = super::svg_images();
        let opts = resvg::usvg::Options {
            resources_dir: Some(dir.clone()),
            ..resvg::usvg::Options::default()
        };
        let taken = (0..300)
            .filter(|_| (resolver.resolve_string)("big.png", &opts).is_some())
            .count();
        // As many are taken as fit in both budgets, 1 GB decoded and 256 MB read.
        let size = std::fs::metadata(dir.join("big.png")).unwrap().len();
        let both = ((1u64 << 30) / (1024 * 1024 * 4)).min((256 << 20) / size);
        assert_eq!(taken as u64, both);
        // A JPEG and an SVG linked by file are still drawn, told by their bytes from what was
        // read and checked, whatever their names say.
        let fresh = super::svg_images();
        image::RgbImage::new(4, 4).save(dir.join("p.jpg")).unwrap();
        std::fs::copy(dir.join("p.jpg"), dir.join("p.dat")).unwrap();
        let sub = "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'/>";
        crate::files::file_io::create_file(&dir.join("sub.svg"), sub).unwrap();
        let kind = |href| (fresh.resolve_string)(href, &opts);
        assert!(matches!(
            kind("p.jpg"),
            Some(resvg::usvg::ImageKind::JPEG(_))
        ));
        assert!(matches!(
            kind("p.dat"),
            Some(resvg::usvg::ImageKind::JPEG(_))
        ));
        assert!(matches!(
            kind("sub.svg"),
            Some(resvg::usvg::ImageKind::SVG(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An SVG whose entities would grow to gigabytes is refused before usvg expands them. One
    /// with a few short entities, as Illustrator writes, is drawn.
    #[test]
    fn refuses_an_svg_of_growing_entities() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-bomb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bomb = svg_bomb();
        let file = dir.join("bomb.svg");
        std::fs::write(&file, &bomb).unwrap();
        // The check refuses it, before usvg does. It is not refused by usvg after expanding it,
        // nor by a timing that a busy machine running every test at once can miss.
        let why = super::load(&file, None).err().unwrap();
        assert_eq!(why, "SVG whose entities would grow too large");
        let fine = "<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY ns \"http://www.w3.org/2000/svg\">]>\
                    <svg xmlns=\"&ns;\" width=\"10\" height=\"10\"><desc>a &amp; b</desc></svg>";
        let file = dir.join("fine.svg");
        std::fs::write(&file, fine).unwrap();
        assert!(super::load(&file, None).is_ok());
        // A quote hidden in a comment, a processing instruction or an attribute default before
        // the long entity does not stop it from being measured.
        let hides = [
            "<!-- <!ENTITY z ' -->",
            "<?x <!ENTITY z ' ?>",
            "<!ATTLIST svg a CDATA \"<!ENTITY z '\">",
        ];
        for hide in hides {
            let hidden = bomb.replacen("<!ENTITY b \"", &format!("{hide}<!ENTITY b \"'"), 1);
            assert!(super::entities_too_large(hidden.as_bytes()), "{hide}");
        }
        // These are read otherwise than roxmltree reads the DTD: a DOCTYPE in a comment before
        // it, a > in its SYSTEM literal, and a ] or a quote in another declaration around the
        // entities.
        let around = [
            (
                "<?xml version=\"1.0\"?>",
                "<?xml version=\"1.0\"?><!-- <!DOCTYPE x> -->",
            ),
            ("<!DOCTYPE svg [", "<!DOCTYPE svg SYSTEM \"a>b\" ["),
            ("<!DOCTYPE svg [", "<!DOCTYPE svg [<!ATTLIST svg a ]>"),
            ("<!DOCTYPE svg [", "<!DOCTYPE svg [<!ATTLIST svg a CDATA '>"),
        ];
        for (at, put) in around {
            let led = bomb.replacen(at, put, 1);
            assert!(super::entities_too_large(led.as_bytes()), "{put}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The start of an SVG of growing entities, broken in thousands of ways, is measured
    /// without a panic or a loop.
    #[test]
    fn survives_broken_svg_entities() {
        for broken in crate::support::test_support::broken(&svg_bomb().as_bytes()[..400]) {
            let _ = super::entities_too_large(&broken);
        }
    }

    /// An SVG whose entities would grow to gigabytes when expanded.
    fn svg_bomb() -> String {
        let long = "x".repeat(100_000);
        format!(
            "<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY b \"{long}\"><!ENTITY a \"{}\">]>\
             <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><desc>{}</desc></svg>",
            "&b;".repeat(250),
            "&a;".repeat(48)
        )
    }

    /// CRC-32 as PNG chunks have it.
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

    /// An SVG that claims a huge size is drawn at the size it is shown, not at 8192 pixels and
    /// shrunk, and its own size is still told. Its alpha is straight, so red stays red.
    #[test]
    fn draws_a_huge_svg_at_the_size_shown() {
        let dir = std::env::temp_dir().join(format!("glimt-test-svg-huge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("huge.svg");
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100000\" height=\"50000\">\
                   <rect width=\"100000\" height=\"50000\" fill=\"red\"/></svg>";
        std::fs::write(&file, svg).unwrap();
        let (frames, orig, _) = crate::images::decode::decode(&file, [64, 64], true, 0).unwrap();
        let [w, h] = frames[0].0.size;
        assert!(w <= 64 && h <= 33, "{w}×{h}");
        assert!(orig[0] >= 8192 && orig[1] >= 4096, "{orig:?}");
        let (img, _) = super::load(&file, Some([8, 8])).unwrap();
        assert_eq!(img.to_rgba8().get_pixel(2, 2).0, [255, 0, 0, 255]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
