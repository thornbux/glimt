//! Colour management: images from their own ICC profile (or Adobe RGB from EXIF, or the
//! profile chosen for images without one) to sRGB or to the screen's profile, with moxcms. The
//! screen profile can come from colord.

use crate::exif;
use crate::external::command;
use eframe::egui;
use image::ImageDecoder;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// Colour management on (Preferences); read by the decode and thumbnail threads.
pub static COLOR_MANAGEMENT: AtomicBool = AtomicBool::new(true);

/// Colours from the image's own ICC profile (Adobe RGB, Display P3 …), or from the profile set
/// for images without one, converted to sRGB: what the screen is taken to show without a screen
/// profile, and what files and the shared thumbnail cache get. Without it such photos look dull.
/// Done after scaling down, where it costs a few milliseconds.
pub fn to_srgb(img: image::DynamicImage, icc: Option<&[u8]>) -> image::DynamicImage {
    convert(img, icc, false, false)
}

/// The image as the screen is to get it: from its profile straight to the screen profile, so
/// that colours outside sRGB which the screen has are kept; without a screen profile as
/// `to_srgb`. For what is shown, not for files or the shared thumbnail cache.
pub fn to_display(img: image::DynamicImage, icc: Option<&[u8]>) -> image::DynamicImage {
    convert(img, icc, true, false)
}

/// An image that is sRGB already (a thumbnail, made by `to_srgb` or read from the shared cache)
/// to the screen profile, not through the profile for images without one a second time.
pub fn srgb_to_display(img: image::DynamicImage) -> image::DynamicImage {
    convert(img, None, true, true)
}

/// The colour settings of Preferences, for the decoding threads.
pub struct Colour {
    /// `None`: the screen is taken to be sRGB.
    pub screen: Option<moxcms::ColorProfile>,
    /// The file `screen` was read from; empty for none.
    pub screen_path: String,
    /// For images without a profile of their own, or all with `own` off; `None`: sRGB.
    pub input: Option<moxcms::ColorProfile>,
    /// An image's own profile counts (Geeqie's "Use image's color profile").
    pub own: bool,
    /// Geeqie's rendering intent.
    pub intent: moxcms::RenderingIntent,
    /// Made once for each source and target: some profiles cost more to make a transform from
    /// than the transform itself. By (source, to the screen): 0 is sRGB, 1 the input profile,
    /// the rest a hash of the image's profile.
    pub transforms: Mutex<Transforms>,
}

/// Colour transforms by (source, to the screen); `None` where none can be made.
pub type Transforms = HashMap<(u64, bool), Option<Arc<moxcms::Transform8BitExecutor>>>;

pub static COLOUR: std::sync::LazyLock<std::sync::RwLock<Colour>> =
    std::sync::LazyLock::new(|| {
        std::sync::RwLock::new(Colour {
            screen: None,
            screen_path: String::new(),
            input: None,
            own: true,
            intent: moxcms::RenderingIntent::Perceptual,
            transforms: Mutex::default(),
        })
    });

/// The pixels of `img` from `icc` (when it counts, else the input profile or sRGB; sRGB with
/// `srgb`) to the screen profile with `screen`, else to sRGB. Unchanged when both are sRGB, with
/// colour management off, or for a profile that does not fit (CMYK, grey).
pub fn convert(
    img: image::DynamicImage,
    icc: Option<&[u8]>,
    screen: bool,
    srgb: bool,
) -> image::DynamicImage {
    use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions};
    if !COLOR_MANAGEMENT.load(Ordering::Relaxed) {
        return img;
    }
    if screen {
        wait_for_screen();
    }
    let c = COLOUR.read().unwrap_or_else(|e| e.into_inner());
    let icc = icc.filter(|_| c.own);
    let screen = screen && c.screen.is_some();
    let from = match icc {
        Some(bytes) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            bytes.hash(&mut h);
            h.finish().max(2)
        }
        None if c.input.is_some() && !srgb => 1,
        None => 0,
    };
    if from == 0 && !screen {
        return img;
    }
    let mut transforms = c.transforms.lock().unwrap_or_else(|e| e.into_inner());
    // One for each profile the images have, and profiles that differ only in the date written
    // into them (exports of one editor) count as others: a few dozen kept, then made again.
    if transforms.len() >= 64 && !transforms.contains_key(&(from, screen)) {
        transforms.clear();
    }
    let t = transforms.entry((from, screen)).or_insert_with(|| {
        let source = match icc {
            // One that cannot be read: sRGB on the way to the screen, left as it is for sRGB.
            Some(bytes) => match ColorProfile::new_from_slice(bytes) {
                Ok(p) => p,
                Err(_) if screen => ColorProfile::new_srgb(),
                Err(_) => return None,
            },
            None if from == 1 => c.input.clone().unwrap_or_else(ColorProfile::new_srgb),
            None => ColorProfile::new_srgb(),
        };
        let target = match (&c.screen, screen) {
            (Some(p), true) => p.clone(),
            _ => ColorProfile::new_srgb(),
        };
        let make = |rendering_intent| {
            let options = TransformOptions {
                rendering_intent,
                ..TransformOptions::default()
            };
            source.create_transform_8bit(Layout::Rgba, &target, Layout::Rgba, options)
        };
        // A profile without tables for the intent has the perceptual ones, as ICC says.
        (make(c.intent).or_else(|_| make(RenderingIntent::Perceptual))).ok()
    });
    let t = t.clone();
    drop(transforms);
    drop(c);
    match t {
        Some(t) => transformed(&*t, img),
        None => img,
    }
}

/// The RGB profile in the ICC file `path`, or `None` when it shows sRGB as it is (so that it
/// costs nothing); an error for one that cannot be read or is not RGB.
pub fn rgb_profile(path: &str) -> Result<Option<moxcms::ColorProfile>, String> {
    use moxcms::{ColorProfile, DataColorSpace};
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let profile = ColorProfile::new_from_slice(&bytes)
        .ok()
        .filter(|p| p.color_space == DataColorSpace::Rgb)
        .ok_or(format!("{path}: not an RGB colour profile"))?;
    Ok((!like_srgb(&profile)).then_some(profile))
}

/// A few colours from sRGB through `profile` come out unchanged.
pub fn like_srgb(profile: &moxcms::ColorProfile) -> bool {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    let samples = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 128, 128, 128, 255, 30, 60, 90, 255,
    ];
    let mut out = [0; 20];
    let srgb = ColorProfile::new_srgb();
    let options = TransformOptions::default();
    srgb.create_transform_8bit(Layout::Rgba, profile, Layout::Rgba, options)
        .is_ok_and(|t| t.transform(&samples, &mut out).is_ok())
        && samples.iter().zip(out).all(|(a, b)| a.abs_diff(b) <= 1)
}

/// Change the colour settings with `change`, and make the transforms again.
pub fn set_colour(change: impl FnOnce(&mut Colour)) {
    let mut c = COLOUR.write().unwrap_or_else(|e| e.into_inner());
    change(&mut c);
    c.transforms = Mutex::default();
}

/// The screen profile in `path` (empty: none, sRGB). A file that is not an RGB profile is an
/// error, and the old one stays. False when it was sRGB and still is, so nothing need be shown
/// again.
pub fn set_screen(path: &str) -> Result<bool, String> {
    let was = {
        let c = COLOUR.read().unwrap_or_else(|e| e.into_inner());
        if c.screen_path == path {
            return Ok(false);
        }
        c.screen.is_some()
    };
    let profile = match path {
        "" => None,
        path => rgb_profile(path)?,
    };
    let changed = was || profile.is_some();
    set_colour(|c| {
        c.screen = profile;
        c.screen_path = path.to_owned();
    });
    Ok(changed)
}

/// Until when images for the screen wait for the desktop's screen profile at start, so the first
/// ones are not shown in sRGB and then again in the screen's colours; `None`: no waiting.
pub static SCREEN_WAIT: (Mutex<Option<Instant>>, std::sync::Condvar) =
    (Mutex::new(None), std::sync::Condvar::new());

/// How long images wait at start for colord, which usually answers in about 50 ms.
pub const SCREEN_WAIT_MS: u64 = 150;

/// Wait while the desktop's screen profile is being read at start, at most until the deadline.
pub fn wait_for_screen() {
    let (lock, ready) = &SCREEN_WAIT;
    let mut until = lock.lock().unwrap_or_else(|e| e.into_inner());
    while let Some(deadline) = *until {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            *until = None;
            break;
        };
        until = ready
            .wait_timeout(until, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// The profile for images without one: "srgb" (or empty), "adobe", "p3", or an ICC file.
pub fn set_input(name: &str) -> Result<(), String> {
    use moxcms::ColorProfile;
    let profile = match name {
        "" | "srgb" => None,
        "adobe" => Some(ColorProfile::new_adobe_rgb()),
        "p3" => Some(ColorProfile::new_display_p3()),
        path => rgb_profile(path)?,
    };
    set_colour(|c| c.input = profile);
    Ok(())
}

/// The rendering intents: (config value, label, intent).
pub const INTENTS: [(&str, &str, moxcms::RenderingIntent); 4] = [
    (
        "perceptual",
        "Perceptual",
        moxcms::RenderingIntent::Perceptual,
    ),
    (
        "relative",
        "Relative colorimetric",
        moxcms::RenderingIntent::RelativeColorimetric,
    ),
    (
        "saturation",
        "Saturation",
        moxcms::RenderingIntent::Saturation,
    ),
    (
        "absolute",
        "Absolute colorimetric",
        moxcms::RenderingIntent::AbsoluteColorimetric,
    ),
];

/// The rendering intent named `name` in `INTENTS`; another name is perceptual.
pub fn set_intent(name: &str) {
    let intent = INTENTS
        .iter()
        .find(|i| i.0 == name)
        .map_or_else(Default::default, |i| i.2);
    set_colour(|c| c.intent = intent);
}

/// The built-in input profiles: (config value, label).
pub const INPUT_PROFILES: [(&str, &str); 3] = [
    ("srgb", "sRGB"),
    ("adobe", "Adobe RGB"),
    ("p3", "Display P3"),
];

/// The profile an image is shown from, for the EXIF panel: its own (by name), Adobe RGB from
/// its EXIF, or the one taken for images without a profile.
pub fn colour_source(path: &Path) -> String {
    use moxcms::{ColorProfile, ProfileText};
    let c = COLOUR.read().unwrap_or_else(|e| e.into_inner());
    let input = c.input.as_ref().map_or("sRGB".to_owned(), name_of);
    let dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_decoder().ok());
    let own = dec.and_then(|mut d| {
        if let Some(icc) = d.icc_profile().ok().flatten() {
            let p = ColorProfile::new_from_slice(&icc).ok()?;
            return Some(format!("{}, in the file", name_of(&p)));
        }
        let exif = d.exif_metadata().ok().flatten()?;
        exif::adobe_rgb(&exif).then(|| "Adobe RGB, from its EXIF".to_owned())
    });
    fn name_of(p: &ColorProfile) -> String {
        match &p.description {
            Some(ProfileText::PlainString(s)) => s.clone(),
            Some(ProfileText::Localizable(l)) => {
                l.first().map_or(String::new(), |s| s.value.clone())
            }
            Some(ProfileText::Description(d)) => d.ascii_string.clone(),
            None => "unnamed".to_owned(),
        }
    }
    match own {
        Some(own) if c.own => own,
        Some(own) => format!("{own}; {input} used instead"),
        None => format!("none: taken as {input}"),
    }
}

/// The desktop's screen profile file, read from colord in the background.
/// At start (`SCREEN_WAIT` set), the profile is used at once if it came in time, and the images
/// waiting for it go on.
pub fn read_desktop_screen(ctx: &egui::Context) -> mpsc::Receiver<Option<String>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let path = desktop_screen_profile();
        let (lock, ready) = &SCREEN_WAIT;
        let mut until = lock.lock().unwrap_or_else(|e| e.into_inner());
        if until.is_some_and(|t| Instant::now() < t) {
            // An error is shown when the window takes it up.
            if let Some(path) = &path {
                let _ = set_screen(path);
            }
        }
        *until = None;
        ready.notify_all();
        drop(until);
        let _ = tx.send(path);
        ctx.request_repaint();
    });
    rx
}

/// The file of the default profile of the first screen in colord, which GNOME and KDE set, on
/// X and Wayland alike. Runs `gdbus`, so it belongs in a background thread.
/// shortcut: the first screen's for every screen; per screen once Glimt knows which it is on.
pub fn desktop_screen_profile() -> Option<String> {
    let call = |object: &str, method: &[&str]| {
        let out = (command("gdbus").args(["call", "--system", "--dest"]))
            .args([
                "org.freedesktop.ColorManager",
                "--object-path",
                object,
                "--method",
            ])
            .args(method)
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        first_quoted(&String::from_utf8_lossy(&out.stdout)).map(str::to_owned)
    };
    let get = "org.freedesktop.DBus.Properties.Get";
    let manager = ["org.freedesktop.ColorManager.GetDevicesByKind", "display"];
    let device = call("/org/freedesktop/ColorManager", &manager)?;
    let profile = call(
        &device,
        &[get, "org.freedesktop.ColorManager.Device", "Profiles"],
    )?;
    call(
        &profile,
        &[get, "org.freedesktop.ColorManager.Profile", "Filename"],
    )
}

/// The first text in single quotes of what gdbus prints: "([objectpath '/a', '/b'],)" is "/a";
/// `None` for an empty list, "(@ao [],)".
pub fn first_quoted(text: &str) -> Option<&str> {
    text.split('\'').nth(1).filter(|s| !s.is_empty())
}

/// Held by the tests that change the colour settings or depend on them, as tests run at once.
#[cfg(test)]
pub static COLOUR_TEST: Mutex<()> = Mutex::new(());

/// An Adobe RGB profile, for images whose EXIF says Adobe RGB without carrying one.
pub static ADOBE_RGB: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    moxcms::ColorProfile::new_adobe_rgb()
        .encode()
        .unwrap_or_default()
});

/// `img` through the colour transform `t`.
pub fn transformed(
    t: &moxcms::Transform8BitExecutor,
    img: image::DynamicImage,
) -> image::DynamicImage {
    let mut rgba = img.into_rgba8();
    let mut out = vec![0; rgba.len()];
    if t.transform(&rgba, &mut out).is_ok() {
        rgba.copy_from_slice(&out);
    }
    rgba.into()
}
