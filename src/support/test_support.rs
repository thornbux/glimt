//! What tests start from. `app_on` makes an `App` on a folder without a window,
//! `run_background_until` runs its background work until a condition holds, and `contract_folder`
//! makes the folder the file operations' contract works on. `broken` gives damaged forms of a good
//! input for the parsers' `survives_broken_*` tests. `png_with_raw_profile` writes a PNG with its
//! EXIF only in a text chunk. `set_attribute` and `attribute` set and read a file's extended
//! attribute. `assert_panel_slides` checks that a panel slides out and then rests, after
//! `draw_until_answered` has waited for the background work that the panel asked for.

/// The EXIF block of a photo of a Canon taken on 2020:01:02 at 03:04:05, stored turned a
/// quarter (orientation 6), big-endian.
pub(crate) const CANON_EXIF: &[u8] = b"MM\0*\0\0\0\x08\0\x03\
    \x01\x0f\0\x02\0\0\0\x06\0\0\0\x32\
    \x01\x12\0\x03\0\0\0\x01\0\x06\0\0\
    \x87\x69\0\x04\0\0\0\x01\0\0\0\x38\
    \0\0\0\0Canon\0\0\x01\
    \x90\x03\0\x02\0\0\0\x14\0\0\0\x4a\
    \0\0\0\x002020:01:02 03:04:05\0";

/// An `App` for a test, on folder `dir`, listed: its own settings, no remote control,
/// nothing kept in the user's files (no recent list, thumbnails or last folder; marks are
/// not written in tests). The window's logic without a window.
pub(crate) fn app_on(dir: &std::path::Path) -> (crate::App, eframe::egui::Context) {
    let ctx = eframe::egui::Context::default();
    let config = crate::files::config::Config {
        remote_control: false,
        keep_recent: false,
        save_thumbnails: false,
        desktop_profile: false,
        remember_dir: false,
        // English, not the system's language: other tests read the texts and numbers it sets.
        language: "en".into(),
        ..Default::default()
    };
    let start = crate::cli::Start::default();
    // A new window sets the colour settings, which also empties the transforms a colour
    // test running beside it was looking at.
    let held = crate::support::threads::lock_even_if_poisoned(&crate::images::colour::COLOUR_TEST);
    let read = crate::ReadAtStart {
        colour_error: crate::images::colour::set_up_colour(&config),
        config,
        problems: Vec::new(),
        desktop_file_problem: None,
        old_desktop_files: Vec::new(),
        #[cfg(feature = "marks")]
        marks: crate::metadata::marks::read_marks(),
        window_state: None,
        recent: Vec::new(),
        sort_folders: Vec::new(),
        keyword_tree: Vec::new(),
    };
    let mut app = crate::App::with_config(&ctx, read, Some(dir.into()), start);
    drop(held);
    app.receive_listing(&ctx, std::time::Duration::from_secs(5));
    (app, ctx)
}

/// `paths` as files a write takes, none of them picked in the list, so no identity is checked.
pub(crate) fn unlisted<P: AsRef<std::path::Path>>(
    paths: impl IntoIterator<Item = P>,
) -> Vec<crate::files::file_io::ListedFile> {
    (paths.into_iter())
        .map(|p| crate::files::file_io::ListedFile::not_from_the_list(p.as_ref().into()))
        .collect()
}

/// `files` with what is written to each, none of them picked in the list (`unlisted`).
pub(crate) fn unlisted_with<T>(
    files: Vec<(std::path::PathBuf, T)>,
) -> Vec<(crate::files::file_io::ListedFile, T)> {
    (files.into_iter())
        .map(|(p, t)| (crate::files::file_io::ListedFile::not_from_the_list(p), t))
        .collect()
}

/// A folder for the contract: a.png (red) and b.png (blue), each with a sidecar saying whose
/// it is, a folder sub, and a collection naming both images.
pub(crate) fn contract_folder(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("glimt-test-contract-{name}-{}", std::process::id()));
    // Not told: a folder left by an earlier run, when there is one.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for (file, colour) in [("a", [255, 0, 0]), ("b", [0, 0, 255])] {
        let png = dir.join(format!("{file}.png"));
        image::RgbImage::from_pixel(8, 8, image::Rgb(colour))
            .save(&png)
            .unwrap();
        std::fs::write(crate::metadata::xmp::sidecar(&png), file).unwrap();
    }
    let gqv = format!(
        "#GQview collection\n\"{}\"\n\"{}\"\n",
        dir.join("a.png").display(),
        dir.join("b.png").display()
    );
    std::fs::write(dir.join("col.gqv"), gqv).unwrap();
    dir
}

/// Opens `path` as `open` does, and waits for its look-up in the background but not for its
/// listing, so a test sees the window as it is between the two.
pub(crate) fn open_now(
    app: &mut crate::App,
    ctx: &eframe::egui::Context,
    path: std::path::PathBuf,
) {
    app.open(ctx, path);
    app.receive_looked_up(ctx, std::time::Duration::from_secs(5));
}

/// Runs the window's background work until `done` holds, for at most five seconds. Returns
/// whether it held. Once it holds, what the work told is taken once more, as work tells before
/// it stops counting as work, and the window takes what was told after it looks whether work
/// runs (`answer_awaited_outcomes`).
pub(crate) fn run_background_until(
    app: &mut crate::App,
    ctx: &eframe::egui::Context,
    done: impl Fn(&crate::App) -> bool,
) -> bool {
    (0..250).any(|_| {
        app.poll_background(ctx);
        let now = done(app);
        match now {
            true => app.poll_background(ctx),
            false => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
        now
    })
}

/// Broken forms of `seed`, as a damaged or crafted file has them, for the parsers' tests: bits
/// flipped, bytes changed, cut short, a piece doubled. Always the same ones for a seed, so a
/// failure comes again; GLIMT_FUZZ=N for N of them instead of 2,000, GLIMT_FUZZ_SEED=S for
/// other ones.
pub(crate) fn broken(seed: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    let n = std::env::var("GLIMT_FUZZ")
        .ok()
        .and_then(|n| n.parse().ok());
    // Another seed (GLIMT_FUZZ_SEED) gives other mutations; never 0, which xorshift keeps.
    let seed_value = (std::env::var("GLIMT_FUZZ_SEED").ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s != 0);
    let mut x: u64 = seed_value.unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x as usize
    };
    (0..n.unwrap_or(2000)).map(move |_| {
        let mut v = seed.to_vec();
        for _ in 0..1 + next() % 4 {
            if v.is_empty() {
                break;
            }
            let at = next() % v.len();
            match next() % 4 {
                0 => v[at] ^= 1 << (next() % 8),
                1 => v[at] = next() as u8,
                2 => v.truncate(at),
                _ => {
                    let piece = v[at..(at + next() % 16).min(v.len())].to_vec();
                    v.splice(at..at, piece);
                }
            }
        }
        v
    })
}

/// Writes a red 8×4 PNG at `path` with the EXIF `block` only in a text chunk named `key`, as
/// ImageMagick and exiv2 write it: the type, the length and the bytes in hex lines. The chunk is
/// tEXt, zTXt or iTXt as `chunk` says.
pub(crate) fn png_with_raw_profile(path: &std::path::Path, chunk: &str, key: &str, block: &[u8]) {
    let text = raw_profile_text("exif", block);
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut enc = png::Encoder::new(file, 8, 4);
    enc.set_color(png::ColorType::Rgb);
    match chunk {
        "tEXt" => enc.add_text_chunk(key.into(), text),
        "zTXt" => enc.add_ztxt_chunk(key.into(), text),
        _ => enc.add_itxt_chunk(key.into(), text),
    }
    .unwrap();
    let pixels: Vec<u8> = [200, 30, 30].repeat(8 * 4);
    enc.write_header()
        .unwrap()
        .write_image_data(&pixels)
        .unwrap();
}

/// Returns `bytes` as the text of an ImageMagick raw profile of the type `kind`: the type, the
/// length and the bytes in hex lines of 36 bytes.
pub(crate) fn raw_profile_text(kind: &str, bytes: &[u8]) -> String {
    let hex: Vec<String> = (bytes.chunks(36))
        .map(|line| line.iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    format!("\n{kind}\n{:8}\n{}\n", bytes.len(), hex.join("\n"))
}

/// Returns a black 4×2 PNG with the text chunks `texts`, each (chunk, keyword, text). A chunk is
/// tEXt, zTXt or iTXt, and the iTXt is compressed too. They come before the pixels when `before`
/// says so, and after them otherwise, where ExifTool adds its chunks.
pub(crate) fn png_with_texts(texts: &[(&str, &str, &str)], before: bool) -> Vec<u8> {
    use png::text_metadata::{ITXtChunk, TEXtChunk, ZTXtChunk};
    let write = |writer: &mut png::Writer<&mut Vec<u8>>| {
        for &(chunk, key, text) in texts {
            match chunk {
                "tEXt" => writer.write_text_chunk(&TEXtChunk::new(key, text)),
                "zTXt" => writer.write_text_chunk(&ZTXtChunk::new(key, text)),
                _ => {
                    let mut itxt = ITXtChunk::new(key, text);
                    itxt.compressed = true;
                    writer.write_text_chunk(&itxt)
                }
            }
            .unwrap();
        }
    };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 4, 2);
    enc.set_color(png::ColorType::Rgb);
    let mut writer = enc.write_header().unwrap();
    if before {
        write(&mut writer);
    }
    writer.write_image_data(&[0; 4 * 2 * 3]).unwrap();
    if !before {
        write(&mut writer);
    }
    writer.finish().unwrap();
    out
}

/// Sets the extended attribute `name` of the file at `path` to `value`. Returns false where the
/// file system has no such attributes, so a test can say that it could not run.
pub(crate) fn set_attribute(path: &std::path::Path, name: &str, value: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new(name).unwrap();
    #[allow(unsafe_code)]
    // SAFETY: both C strings and `value`, of the length given, live through the call.
    let set = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    set == 0
}

/// Returns the extended attribute `name` of the file at `path`, or `None` when it has none.
pub(crate) fn attribute(path: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new(name).unwrap();
    let mut value = vec![0u8; 65_536];
    #[allow(unsafe_code)]
    // SAFETY: both C strings and `value`, of the length given, live through the call.
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    value.truncate(usize::try_from(size).ok()?);
    Some(value)
}

/// Draws the panel `draw` shown, and runs the window's background until `answered` holds, so
/// that no answer to the work the panel asked for (a thumbnail, a size, the EXIF read, the
/// image's decode) asks for a frame while `assert_panel_slides` counts them. Before, an answer
/// that came late, as under a loaded test run, made the last frame counted ask for another.
pub(crate) fn draw_until_answered(
    app: &mut crate::App,
    ctx: &eframe::egui::Context,
    mut draw: impl FnMut(&mut crate::App, &mut eframe::egui::Ui),
    answered: impl Fn(&crate::App) -> bool,
) {
    let is_answered = (0..250).any(|_| {
        let _ = ctx.run_ui(eframe::egui::RawInput::default(), |ui| draw(app, ui));
        app.poll_background(ctx);
        let now = answered(app);
        if !now {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        now
    });
    assert!(
        is_answered,
        "the panel's work was not answered in five seconds"
    );
}

/// Checks that the panel `draw` draws slides out over a few frames once it is no longer
/// shown, then asks for no frame at all, and goes at once when the animation time is zero.
/// `draw` is given whether the panel is shown.
pub(crate) fn assert_panel_slides(
    ctx: &eframe::egui::Context,
    mut draw: impl FnMut(&mut eframe::egui::Ui, bool),
) {
    use eframe::egui;
    // Draws one frame at `seconds` in a window of 800 points, and returns the width left
    // beside the panel and how long egui may wait before the next frame.
    let mut frame_at = |seconds: f64, is_shown: bool| {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let input = egui::RawInput {
            time: Some(seconds),
            screen_rect: Some(screen),
            ..Default::default()
        };
        let mut left = 0.0;
        let out = ctx.run_ui(input, |ui| {
            draw(ui, is_shown);
            left = ui.available_rect_before_wrap().width();
        });
        (
            left,
            out.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
        )
    };
    // Frames 1/60 s apart, as egui moves an animation by at most one frame's time.
    let frame = |n: u32| f64::from(n) / 60.0;
    let (shown, _) = frame_at(frame(0), true);
    assert!(shown < 800.0, "the panel takes no room: {shown}");
    frame_at(frame(1), true);
    frame_at(frame(2), false);
    let (sliding, wait) = frame_at(frame(3), false);
    assert!(
        shown < sliding && sliding < 800.0,
        "no slide: {shown} {sliding}"
    );
    assert_eq!(
        wait,
        std::time::Duration::ZERO,
        "the slide asks for no next frame"
    );
    for n in 4..60 {
        frame_at(frame(n), false);
    }
    let (out, wait) = frame_at(frame(60), false);
    assert_eq!(out, 800.0);
    assert_eq!(
        wait,
        std::time::Duration::MAX,
        "the hidden panel still asks for frames"
    );
    // A reduced motion: the theme's animation time of zero shows and hides it at once.
    ctx.all_styles_mut(|style| style.animation_time = 0.0);
    frame_at(frame(61), true);
    assert_eq!(frame_at(frame(62), false).0, 800.0);
}

/// Returns an EXIF block, after "Exif\0\0" as in a JPEG, with a value for every row of the info
/// panel's `exif::FIELDS`: the camera, the lens, the date taken, the exposure, the aperture,
/// the ISO, the focal length, the exposure bias, the flash and the GPS position.
pub(crate) fn exif_with_every_row() -> Vec<u8> {
    let mut d = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
    let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
        d.extend(tag.to_le_bytes());
        d.extend(kind.to_le_bytes());
        d.extend(count.to_le_bytes());
        d.extend(value.to_le_bytes());
    };
    // The offsets count from "II", 6 bytes into the block.
    let at = |d: &Vec<u8>| d.len() - 6;
    // IFD0 at 8, with the make at 50, the Exif directory at 56 and the GPS one at 215.
    d.extend(3u16.to_le_bytes());
    entry(&mut d, 0x010f, 2, 6, 50);
    entry(&mut d, 0x8769, 4, 1, 56);
    entry(&mut d, 0x8825, 4, 1, 215);
    d.extend(0u32.to_le_bytes());
    d.extend(b"Canon\0");
    assert_eq!(at(&d), 56);
    // The Exif directory, its values from 158: the date, the lens, and the rationals of
    // the exposure, aperture, focal length and exposure bias. ISO and flash are inline.
    d.extend(8u16.to_le_bytes());
    entry(&mut d, 0xa434, 2, 5, 178);
    entry(&mut d, 0x9003, 2, 20, 158);
    entry(&mut d, 0x829a, 5, 1, 183);
    entry(&mut d, 0x829d, 5, 1, 191);
    entry(&mut d, 0x8827, 3, 1, 400);
    entry(&mut d, 0x920a, 5, 1, 199);
    entry(&mut d, 0x9204, 10, 1, 207);
    entry(&mut d, 0x9209, 3, 1, 1);
    d.extend(0u32.to_le_bytes());
    assert_eq!(at(&d), 158);
    d.extend(b"2020:01:02 03:04:05\0Lens\0");
    for v in [1u32, 250, 28, 10, 50, 1, 1, 1] {
        d.extend(v.to_le_bytes());
    }
    assert_eq!(at(&d), 215);
    // The GPS directory, its two rationals at 269.
    d.extend(4u16.to_le_bytes());
    entry(&mut d, 1, 2, 2, u32::from(b'N'));
    entry(&mut d, 2, 5, 1, 269);
    entry(&mut d, 3, 2, 2, u32::from(b'E'));
    entry(&mut d, 4, 5, 1, 277);
    d.extend(0u32.to_le_bytes());
    assert_eq!(at(&d), 269);
    for v in [60u32, 1, 10, 1] {
        d.extend(v.to_le_bytes());
    }
    d
}

#[cfg(test)]
mod tests {
    /// A test's window speaks English whatever the system's language. With LANG=de_DE it took
    /// German, and a test of the sizes beside it read "850,0 kB".
    #[test]
    fn a_window_for_a_test_speaks_english() {
        let dir = std::env::temp_dir().join(format!("glimt-test-english-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (app, _) = super::app_on(&dir);
        assert_eq!(crate::support::lang::tr("Open Folder"), "Open Folder");
        assert_eq!(app.config.language, "en");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What background work told just before it ended is taken before `run_background_until`
    /// returns. Before, work that ended between the last poll and the look at `done` left its
    /// notice in the channel, and New Collection's test now and then read the notice before it.
    #[test]
    fn what_work_told_before_it_ended_is_taken() {
        let id = std::process::id();
        let dir = std::env::temp_dir().join(format!("glimt-test-told-before-end-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = super::app_on(&dir);
        let (go, wait) = std::sync::mpsc::channel::<()>();
        assert!(app.run_in_background(&ctx, Vec::new(), move || {
            wait.recv().unwrap();
            Some(crate::support::threads::Told::Notice(
                "told at the end".into(),
            ))
        }));
        // The work ends after the poll and before `done` looks, as it may in any run.
        let ended = super::run_background_until(&mut app, &ctx, |app| {
            go.send(()).unwrap_or_default();
            (0..500).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                !app.working_besides_image()
            })
        });
        assert!(ended);
        assert_eq!(app.notice.text(), Some("told at the end"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
