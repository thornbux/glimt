//! What tests start from. `app_on` makes an `App` on a folder without a window, `settle` runs
//! its background work until a condition holds, and `contract_folder` makes the folder the file
//! operations' contract works on. `broken` gives damaged forms of a good input for the parsers'
//! `survives_broken_*` tests.

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
        ..Default::default()
    };
    let start = crate::cli::Start::default();
    // A new window sets the colour settings, which also empties the transforms a colour
    // test running beside it was looking at.
    let held = crate::images::colour::COLOUR_TEST
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut app = crate::App::with(&ctx, config, &[], Some(dir.into()), start);
    drop(held);
    app.receive_listing(&ctx, std::time::Duration::from_secs(5));
    (app, ctx)
}

/// A folder for the contract: a.png (red) and b.png (blue), each with a sidecar saying whose
/// it is, a folder sub, and a collection naming both images.
pub(crate) fn contract_folder(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("glimt-test-contract-{name}-{}", std::process::id()));
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

/// Run the window's background work until `done` holds, at most five seconds.
pub(crate) fn settle(
    app: &mut crate::App,
    ctx: &eframe::egui::Context,
    done: impl Fn(&crate::App) -> bool,
) -> bool {
    (0..250).any(|_| {
        app.poll_background(ctx);
        let now = done(app);
        if !now {
            std::thread::sleep(std::time::Duration::from_millis(20));
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
