//! SVG and SVGZ through resvg (feature `svg`), with limits on what a file may make it do: the
//! text unpacked, the pictures it links to, and entities that grow into gigabytes.

use crate::images::decode::read_limited;
use std::path::Path;

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
            Some((w, h)) if !crate::images::decode::fits(w, h, 4) => return false,
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

/// Draws an SVG with resvg, fitted in `max`, or else at its own size but at least 2048 pixels
/// on the longer side, so that it stays sharp when fitted to the window. Returns the picture,
/// and with `max` also the size it has without `max`, which the view counts as its own.
pub fn load(
    path: &Path,
    max: Option<[u32; 2]>,
) -> Result<(image::DynamicImage, Option<[usize; 2]>), String> {
    use resvg::{tiny_skia, usvg};
    use std::sync::Arc;
    // The system fonts, for text in SVGs, are read once, because it takes tens of milliseconds.
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
    let tree = usvg::Tree::from_data(&data, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    // The longer side has at most 8192 pixels, because an SVG may claim any size, and the
    // pixmap is 4 bytes per pixel (60000×60000 would be 14 GB).
    let longer = size.width().max(size.height());
    let own = (2048.0 / longer).max(1.0).min(8192.0 / longer);
    // It is drawn at the size it is shown, not at 2048 and shrunk, because an SVG of a few
    // bytes may claim 100000×100000, which would be 256 MB for each image read ahead.
    let scale = match max {
        Some([w, h]) => (w as f32 / size.width()).min(h as f32 / size.height()),
        None => own,
    }
    .min(8192.0 / longer);
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
    Ok((image::DynamicImage::from(img), own_size))
}

#[cfg(test)]
mod tests {
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
