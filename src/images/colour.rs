//! Colour management: images from their own ICC profile (or Adobe RGB from EXIF, or the
//! profile chosen for images without one) to sRGB or to the screen's profile, with moxcms. The
//! screen profile can come from colord.

use crate::files::config::Config;
use crate::metadata::exif;
use crate::support::lang::tr;
use eframe::egui;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// Colour management on (Preferences); read by the decode and thumbnail threads.
pub static COLOR_MANAGEMENT: AtomicBool = AtomicBool::new(true);

/// View → Filter, for low vision: an index into `FILTERS`; read by the decode and thumbnail
/// threads.
pub static VIEW_FILTER: AtomicUsize = AtomicUsize::new(0);

pub const FILTERS: [&str; 4] = ["None", "Greyscale", "Inverted", "More Contrast"];

pub static COLOUR: std::sync::LazyLock<std::sync::RwLock<Colour>> =
    std::sync::LazyLock::new(|| {
        std::sync::RwLock::new(Colour {
            screen: None,
            screen_path: String::new(),
            input: None,
            use_image_profile: true,
            intent: moxcms::RenderingIntent::Perceptual,
            transforms: Arc::default(),
        })
    });

/// The colour settings for what Glimt writes or hands to other programs: the image's own
/// profile to sRGB, untagged images left as sRGB, and the perceptual intent. They never change,
/// so the shared thumbnail cache, a print, the clipboard and an exported frame get the same
/// colours whatever the view is set to. Their transforms are kept apart from the view's, so a
/// change in Preferences does not make them again.
static FOR_FILES: std::sync::LazyLock<Colour> = std::sync::LazyLock::new(|| Colour {
    screen: None,
    screen_path: String::new(),
    input: None,
    use_image_profile: true,
    intent: moxcms::RenderingIntent::Perceptual,
    transforms: Arc::default(),
});

/// Until when images for the screen wait for the desktop's screen profile at start, so the first
/// ones are not shown in sRGB and then again in the screen's colours; `None`: no waiting.
pub static SCREEN_WAIT: (Mutex<Option<Instant>>, std::sync::Condvar) =
    (Mutex::new(None), std::sync::Condvar::new());

/// How long images wait at start for colord, which usually answers in about 50 ms.
pub const SCREEN_WAIT_MS: u64 = 150;

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

/// The built-in input profiles: (config value, label, profile). The profile is `None` for sRGB,
/// which images are shown in without a conversion. Preferences writes these values and
/// `set_input` reads them, so a renamed one fails `config::tests::frozen_names_are_kept`.
pub const INPUT_PROFILES: [(&str, &str, BuiltIn); 3] = [
    ("srgb", "sRGB", || None),
    ("adobe", "Adobe RGB", || {
        Some(moxcms::ColorProfile::new_adobe_rgb())
    }),
    ("p3", "Display P3", || {
        Some(moxcms::ColorProfile::new_display_p3())
    }),
];

/// Held by the tests that change the colour settings or depend on them, as tests run at once.
#[cfg(test)]
pub static COLOUR_TEST: Mutex<()> = Mutex::new(());

/// An Adobe RGB profile, for images whose EXIF says Adobe RGB without carrying one.
pub static ADOBE_RGB: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    moxcms::ColorProfile::new_adobe_rgb()
        .encode()
        .unwrap_or_default()
});

/// The colour settings of Preferences, for the decoding threads.
#[derive(Clone)]
pub struct Colour {
    /// `None`: the screen is taken to be sRGB.
    pub screen: Option<moxcms::ColorProfile>,
    /// The file `screen` was read from; empty for none.
    pub screen_path: String,
    /// For images without a profile of their own, or all with `use_image_profile` off; `None`:
    /// sRGB.
    pub input: Option<moxcms::ColorProfile>,
    /// An image's own profile counts (Geeqie's "Use image's color profile").
    pub use_image_profile: bool,
    /// Geeqie's rendering intent.
    pub intent: moxcms::RenderingIntent,
    /// Made once for each source and target: some profiles cost more to make a transform from
    /// than the transform itself. By (source, to the screen): 0 is sRGB, 1 the input profile,
    /// the rest a hash of the image's profile.
    pub transforms: Arc<Mutex<Transforms>>,
}

/// Makes a built-in profile of `INPUT_PROFILES`.
pub type BuiltIn = fn() -> Option<moxcms::ColorProfile>;

/// Colour transforms by (source, to the screen); `None` where none can be made.
pub type Transforms = HashMap<(u64, bool), Option<Arc<moxcms::Transform8BitExecutor>>>;

// What a test runs on its thread where an image's profile is about to be parsed, to see that the
// transform cache is not locked then.
#[cfg(test)]
thread_local! {
    static PARSING: std::cell::Cell<Option<fn(&Colour)>> =
        const { std::cell::Cell::new(None) };
    // What a test runs where colour_source is about to read the image file, to see that the
    // colour settings are not locked then.
    static READING: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

/// Converts the image from its own ICC profile (Adobe RGB, Display P3 …, or Adobe RGB that its
/// EXIF names, which arrives as `icc`) to sRGB, as the file says. An image without a profile is
/// sRGB and stays as it is. This is for everything that leaves Glimt: files, the shared
/// thumbnail cache, prints and the clipboard. It reads none of the view's colour settings, so
/// Preferences never change what other programs get. Without it such photos look dull. It is
/// done after scaling down, where it costs a few milliseconds.
pub fn to_srgb(img: image::DynamicImage, icc: Option<&[u8]>) -> image::DynamicImage {
    converted(&FOR_FILES, img, icc, false, false)
}

/// The image as the screen is to get it: from its profile straight to the screen profile, so
/// that colours outside sRGB which the screen has are kept; without a screen profile to sRGB.
/// It follows the colour settings of Preferences and the view filter, so it is for what is
/// shown only, never for files or the shared thumbnail cache (`to_srgb`).
pub fn to_display(img: image::DynamicImage, icc: Option<&[u8]>) -> image::DynamicImage {
    apply_view_filter(
        convert(img, icc, true, false),
        VIEW_FILTER.load(Ordering::Relaxed),
    )
}

/// An image that is sRGB already (a thumbnail, made by `to_srgb` or read from the shared cache)
/// to the screen profile, not through the profile for images without one a second time.
pub fn srgb_to_display(img: image::DynamicImage) -> image::DynamicImage {
    apply_view_filter(
        convert(img, None, true, true),
        VIEW_FILTER.load(Ordering::Relaxed),
    )
}

/// Applies the view filter `f` to the pixels; alpha stays as it was.
fn apply_view_filter(img: image::DynamicImage, f: usize) -> image::DynamicImage {
    if f == 0 {
        return img;
    }
    let mut rgba = img.into_rgba8();
    for p in rgba.pixels_mut() {
        let [r, g, b, _] = &mut p.0;
        match f {
            // Rec. 709 weights out of 256.
            1 => {
                let y = ((54 * *r as u32 + 183 * *g as u32 + 19 * *b as u32) >> 8) as u8;
                [*r, *g, *b] = [y, y, y];
            }
            2 => [*r, *g, *b] = [255 - *r, 255 - *g, 255 - *b],
            _ => {
                for c in [&mut *r, &mut *g, &mut *b] {
                    *c = ((*c as i32 - 128) * 8 / 5 + 128).clamp(0, 255) as u8;
                }
            }
        }
    }
    rgba.into()
}

/// The pixels of `img` from `icc` (when it counts, else the input profile or sRGB; sRGB with
/// `srgb`) to the screen profile with `screen`, else to sRGB, by the settings of Preferences.
/// Unchanged when both are sRGB, with colour management off, or for a profile that does not fit
/// (CMYK, grey).
fn convert(
    img: image::DynamicImage,
    icc: Option<&[u8]>,
    screen: bool,
    srgb: bool,
) -> image::DynamicImage {
    if !COLOR_MANAGEMENT.load(Ordering::Relaxed) {
        return img;
    }
    if screen {
        wait_for_screen();
    }
    // The settings are copied and their lock let go at once. A large or crafted profile can take
    // long to parse, and with the lock held a change in Preferences would wait for it, and every
    // other decoding thread would wait behind that change.
    let c = COLOUR.read().unwrap_or_else(|e| e.into_inner()).clone();
    converted(&c, img, icc, screen, srgb)
}

/// The pixels of `img` as `convert` says, by the colour settings `c`.
fn converted(
    c: &Colour,
    img: image::DynamicImage,
    icc: Option<&[u8]>,
    screen: bool,
    srgb: bool,
) -> image::DynamicImage {
    let icc = icc.filter(|_| c.use_image_profile);
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
    let key = (from, screen);
    let cached = (c.transforms.lock().unwrap_or_else(|e| e.into_inner()))
        .get(&key)
        .cloned();
    let t = cached.unwrap_or_else(|| {
        // The transform is made without the cache locked. A large or crafted profile can take
        // long to parse, and the other decoding threads should not wait for it.
        let t = make_transform(c, icc, from, screen);
        let mut transforms = c.transforms.lock().unwrap_or_else(|e| e.into_inner());
        // The cache keeps a transform for each profile the images have. Profiles that differ
        // only in the date written into them (exports of one editor) count as others, so a few
        // dozen are kept and then all are made again.
        if transforms.len() >= 64 && !transforms.contains_key(&key) {
            transforms.clear();
        }
        transforms.insert(key, t.clone());
        t
    });
    match t {
        Some(t) => transformed(&*t, img),
        None => img,
    }
}

/// The RGB profile in the ICC file `path`, or `None` when it shows sRGB as it is (so that it
/// costs nothing); an error for one that cannot be read or is not RGB.
pub fn rgb_profile(path: &str) -> Result<Option<moxcms::ColorProfile>, String> {
    use moxcms::{ColorProfile, DataColorSpace};
    let bytes = crate::images::decode::read_limited(path).map_err(|e| format!("{path}: {e}"))?;
    let profile = ColorProfile::new_from_slice(&bytes)
        .ok()
        .filter(|p| p.color_space == DataColorSpace::Rgb)
        .ok_or(format!("{path}: not an RGB colour profile"))?;
    Ok((!is_like_srgb(&profile)).then_some(profile))
}

/// Returns whether a few colours from sRGB through `profile` come out unchanged.
pub fn is_like_srgb(profile: &moxcms::ColorProfile) -> bool {
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
    c.transforms = Arc::default();
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

/// Sets the profile for images without one: a value of `INPUT_PROFILES`, an empty one for
/// sRGB, or the path of an ICC file.
pub fn set_input(name: &str) -> Result<(), String> {
    let profile = match INPUT_PROFILES.iter().find(|p| p.0 == name) {
        Some(p) => p.2(),
        None if name.is_empty() => None,
        None => rgb_profile(name)?,
    };
    set_colour(|c| c.input = profile);
    Ok(())
}

/// The rendering intent named `name` in `INTENTS`; another name is perceptual.
pub fn set_intent(name: &str) {
    let intent = INTENTS
        .iter()
        .find(|i| i.0 == name)
        .map_or_else(Default::default, |i| i.2);
    set_colour(|c| c.intent = intent);
}

/// The profile an image is shown from, for the info panel: its own (by name), Adobe RGB from
/// its EXIF, or the one taken for images without a profile.
pub fn colour_source(path: &Path) -> String {
    use moxcms::{ColorProfile, ProfileText};
    // The file is read before the colour settings are locked. On a stalled share the read can
    // take long, and a change in Preferences would wait for it, with every decoding thread
    // waiting behind that change.
    #[cfg(test)]
    if let Some(hook) = READING.get() {
        hook();
    }
    // The profile as the view reads it, also from a JPEG XL, which the image crate cannot open.
    let own = match crate::images::decode::icc_profile(path) {
        Some(icc) => ColorProfile::new_from_slice(&icc)
            .ok()
            .map(|p| crate::tf!("{name}, in the file", name = name_of(&p))),
        None => {
            let dec = crate::images::decode::image_reader(path)
                .ok()
                .and_then(|r| r.into_decoder().ok());
            // The block the view reads, so a PNG's text chunk counts too.
            let exif = dec.and_then(|mut d| exif::decoded(path, &mut d));
            (exif.is_some_and(|e| exif::adobe_rgb(&e)))
                .then(|| tr("Adobe RGB, from its EXIF").to_owned())
        }
    };
    fn name_of(p: &ColorProfile) -> String {
        match &p.description {
            Some(ProfileText::PlainString(s)) => s.clone(),
            Some(ProfileText::Localizable(l)) => {
                l.first().map_or(String::new(), |s| s.value.clone())
            }
            Some(ProfileText::Description(d)) => d.ascii_string.clone(),
            None => tr("unnamed").to_owned(),
        }
    }
    let c = COLOUR.read().unwrap_or_else(|e| e.into_inner());
    let input = c.input.as_ref().map_or("sRGB".to_owned(), name_of);
    match own {
        Some(own) if c.use_image_profile => own,
        Some(own) => crate::tf!("{own}; {input} used instead", own, input),
        None => crate::tf!("none: taken as {input}", input),
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
        let mut args = vec!["call", "--system", "--dest", "org.freedesktop.ColorManager"];
        args.extend(["--object-path", object, "--method"]);
        args.extend(method);
        let out = crate::files::external::short_output_of("gdbus", &args, 1 << 20)?;
        first_quoted(&out).map(str::to_owned)
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

/// The first text in single quotes of what gdbus prints: "([objectpath '/a', '/b'],)" is "/a".
/// Returns `None` for an empty list, "(@ao [],)".
pub fn first_quoted(text: &str) -> Option<&str> {
    text.split('\'').nth(1).filter(|s| !s.is_empty())
}

/// Makes the transform of an image's colours from `icc` (or the profile `from` stands for, see
/// `Colour::transforms`) to the screen with `screen`, else to sRGB. Returns `None` where none
/// is needed or none can be made.
fn make_transform(
    c: &Colour,
    icc: Option<&[u8]>,
    from: u64,
    screen: bool,
) -> Option<Arc<moxcms::Transform8BitExecutor>> {
    use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions};
    #[cfg(test)]
    if let Some(hook) = PARSING.get() {
        hook(c);
    }
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
    // An image's own profile that is sRGB (as most exports have) changes nothing on the way
    // to sRGB: no pass over every pixel, and no second copy of a large image for it. Looked
    // at once, as the transform is kept for that profile.
    if !screen && from >= 2 && is_like_srgb(&source) {
        return None;
    }
    let make = |rendering_intent| {
        let options = TransformOptions {
            rendering_intent,
            ..TransformOptions::default()
        };
        source.create_transform_8bit(Layout::Rgba, &target, Layout::Rgba, options)
    };
    // A profile without tables for the intent has the perceptual ones, as ICC says.
    (make(c.intent).or_else(|_| make(RenderingIntent::Perceptual))).ok()
}

/// `img` through the colour transform `t`, a megabyte at a time through one small buffer: a
/// full-size photo for printing or saving does not need a second copy of itself.
pub fn transformed(
    t: &moxcms::Transform8BitExecutor,
    img: image::DynamicImage,
) -> image::DynamicImage {
    const PART: usize = 1 << 20; // whole RGBA pixels
    let mut rgba = img.into_rgba8();
    let mut out = vec![0; PART.min(rgba.len())];
    for part in rgba.chunks_mut(PART) {
        let out = &mut out[..part.len()];
        if t.transform(part, out).is_ok() {
            part.copy_from_slice(out);
        }
    }
    rgba.into()
}

/// Sets up the colour settings in effect at start. Returns the screen profile's or input profile's
/// error, if either could not be read. With the desktop's profile asked for, images wait a moment
/// for it.
pub(crate) fn set_up_colour(config: &Config) -> Option<String> {
    COLOR_MANAGEMENT.store(config.color_management, Ordering::Relaxed);
    VIEW_FILTER.store(config.view_filter, Ordering::Relaxed);
    let error = (set_screen(&config.screen_profile))
        .and_then(|_| set_input(&config.input_profile))
        .err();
    set_colour(|c| c.use_image_profile = config.use_image_profile);
    set_intent(&config.rendering_intent);
    if config.desktop_profile && config.color_management {
        let deadline = Instant::now() + std::time::Duration::from_millis(SCREEN_WAIT_MS);
        *SCREEN_WAIT.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(deadline);
    }
    error
}

#[cfg(test)]
mod tests {
    /// A damaged colour profile in a photo gives the photo as it is, never a panic.
    #[test]
    fn survives_broken_device_lists() {
        for bytes in crate::support::test_support::broken(b"([objectpath '/a', '/b'],)") {
            super::first_quoted(&String::from_utf8_lossy(&bytes));
        }
    }

    #[test]
    fn survives_broken_profiles() {
        let img = image::DynamicImage::new_rgb8(2, 2);
        for bytes in crate::support::test_support::broken(&super::ADOBE_RGB) {
            let _ = super::convert(img.clone(), Some(&bytes), false, true);
        }
    }

    /// A photo larger than one part comes out as through one whole buffer.
    #[test]
    fn transforms_in_parts() {
        use moxcms::{ColorProfile, Layout, TransformOptions};
        let (srgb, p3) = (ColorProfile::new_srgb(), ColorProfile::new_display_p3());
        let options = TransformOptions::default();
        let t = srgb
            .create_transform_8bit(Layout::Rgba, &p3, Layout::Rgba, options)
            .unwrap();
        let img = image::RgbaImage::from_fn(700, 700, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 200, 255])
        });
        let mut whole = vec![0; img.len()];
        t.transform(&img, &mut whole).unwrap();
        let parts = super::transformed(&*t, img.into()).into_rgba8();
        assert_eq!(parts.as_raw(), &whole);
    }

    /// Greyscale keeps white white, inverted turns red cyan, more contrast spreads from the
    /// middle; alpha stays.
    #[test]
    fn view_filters() {
        let px = |f, rgba| {
            let img = image::RgbaImage::from_pixel(1, 1, image::Rgba(rgba));
            super::apply_view_filter(img.into(), f)
                .into_rgba8()
                .get_pixel(0, 0)
                .0
        };
        assert_eq!(px(0, [200, 10, 10, 9]), [200, 10, 10, 9]);
        assert_eq!(px(1, [255, 255, 255, 9]), [255, 255, 255, 9]);
        assert_eq!(px(1, [255, 0, 0, 9]), [53, 53, 53, 9]);
        assert_eq!(px(2, [255, 0, 0, 9]), [0, 255, 255, 9]);
        assert_eq!(px(3, [128, 200, 20, 9]), [128, 243, 0, 9]);
    }

    /// Through a Display P3 screen profile, sRGB red is less red in the screen's numbers. A file
    /// that is no profile leaves the one there; an sRGB screen profile counts as none.
    #[test]
    fn shows_through_a_screen_profile() {
        let _held = super::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glimt-screen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, bytes: Vec<u8>| {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            file.to_string_lossy().into_owned()
        };
        let p3 = write(
            "p3.icc",
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        );
        let srgb = write(
            "srgb.icc",
            moxcms::ColorProfile::new_srgb().encode().unwrap(),
        );
        let junk = write("junk.icc", b"junk".to_vec());
        let red = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([255, 0, 0, 255]),
            ))
        };
        let shown = || *super::to_display(red(), None).into_rgba8().get_pixel(0, 0);
        super::set_screen(&p3).unwrap();
        let on_p3 = shown();
        assert!(on_p3[0] < 250 && on_p3[1] > 20, "{on_p3:?}");
        assert!(super::set_screen(&junk).is_err());
        assert_eq!(shown(), on_p3);
        super::set_screen(&srgb).unwrap();
        assert!(super::COLOUR.read().unwrap().screen.is_none());
        assert_eq!(super::to_display(red(), None), red());
        // Images without a profile taken as Adobe RGB: in sRGB the same numbers mean another
        // colour.
        super::set_input("adobe").unwrap();
        let grey = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([60, 160, 60, 255]),
            ))
        };
        assert_ne!(super::to_display(grey(), None), grey());
        // A thumbnail is sRGB already: not taken as Adobe RGB a second time.
        assert_eq!(super::srgb_to_display(grey()), grey());
        // The image's own profile left out: the input profile for all, here sRGB.
        super::set_input("srgb").unwrap();
        super::set_colour(|c| c.use_image_profile = false);
        let adobe = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
        assert_eq!(super::to_display(grey(), Some(&adobe)), grey());
        super::set_colour(|c| c.use_image_profile = true);
        assert_ne!(super::to_display(grey(), Some(&adobe)), grey());
        assert!(super::set_input(&junk).is_err());
        // A profile that cannot be read: left as it is.
        assert_eq!(super::to_display(grey(), Some(b"junk")), grey());
        // The image's own profile is sRGB: no transform kept for it, the pixels as they are.
        let own_srgb = moxcms::ColorProfile::new_srgb().encode().unwrap();
        assert_eq!(super::to_display(grey(), Some(&own_srgb)), grey());
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            own_srgb.as_slice().hash(&mut h);
            h.finish().max(2)
        };
        let c = super::COLOUR.read().unwrap();
        assert!(matches!(
            c.transforms.lock().unwrap().get(&(key, false)),
            Some(None)
        ));
        drop(c);
        // Profiles that differ only in their date (bytes 24 to 36) are kept a few dozen at most.
        for day in 0..100u8 {
            let mut dated = adobe.clone();
            dated[30] = day;
            super::to_display(grey(), Some(&dated));
        }
        let c = super::COLOUR.read().unwrap();
        assert!(c.transforms.lock().unwrap().len() <= 64);
        drop(c);
        super::set_screen("").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A profile with a table for perceptual and another for colorimetric: the intent picks one,
    /// and saturation, which has none, takes the perceptual one.
    #[test]
    fn rendering_intent_picks_its_table() {
        use moxcms::{LutDataType, LutStore, LutType, LutWarehouse, Matrix3d};
        let _held = super::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
        // RGB to XYZ through a 2×2×2 grid, red and blue swapped in the colorimetric one.
        let table = |swap: bool| {
            let mut clut = Vec::new();
            for r in [0, 1] {
                for g in [0, 1] {
                    for b in [0, 1] {
                        let (x, z) = if swap { (b, r) } else { (r, b) };
                        clut.extend([x * 30000, g * 30000, z * 30000]);
                    }
                }
            }
            LutWarehouse::Lut(LutDataType {
                num_input_channels: 3,
                num_output_channels: 3,
                num_clut_grid_points: 2,
                matrix: Matrix3d::IDENTITY,
                num_input_table_entries: 2,
                num_output_table_entries: 2,
                input_table: LutStore::Store16([0, 65535].repeat(3)),
                clut_table: LutStore::Store16(clut),
                output_table: LutStore::Store16([0, 65535].repeat(3)),
                lut_type: LutType::Lut16,
            })
        };
        let mut profile = moxcms::ColorProfile::new_srgb();
        profile.lut_a_to_b_perceptual = Some(table(false));
        profile.lut_a_to_b_colorimetric = Some(table(true));
        super::set_colour(|c| c.input = Some(profile));
        let red = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([200, 40, 40, 255]),
            ))
        };
        let shown = |intent| {
            super::set_intent(intent);
            *super::to_display(red(), None).into_rgba8().get_pixel(0, 0)
        };
        let perceptual = shown("perceptual");
        assert_ne!(perceptual, red().into_rgba8()[(0, 0)]);
        assert_ne!(shown("relative"), perceptual);
        assert_eq!(shown("saturation"), perceptual);
        super::set_input("srgb").unwrap();
        super::set_intent("perceptual");
    }

    /// What Glimt hands out is in the colours the file says, whatever the view's settings are:
    /// colour management off, an input profile, the image's own profile left out and another
    /// intent change nothing. Before, they reached the shared thumbnail cache and prints.
    #[test]
    fn hands_out_the_files_colours_whatever_the_view() {
        use std::sync::atomic::Ordering;
        let _held = super::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let grey = || {
            image::DynamicImage::from(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([60, 160, 60, 255]),
            ))
        };
        let p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let tagged = super::to_srgb(grey(), Some(&p3));
        assert_ne!(tagged, grey());
        super::COLOR_MANAGEMENT.store(false, Ordering::Relaxed);
        let off = super::to_srgb(grey(), Some(&p3));
        super::COLOR_MANAGEMENT.store(true, Ordering::Relaxed);
        super::set_input("adobe").unwrap();
        super::set_colour(|c| c.use_image_profile = false);
        super::set_intent("relative");
        let set = (
            super::to_srgb(grey(), Some(&p3)),
            super::to_srgb(grey(), None),
        );
        super::set_input("srgb").unwrap();
        super::set_colour(|c| c.use_image_profile = true);
        super::set_intent("perceptual");
        assert_eq!(off, tagged);
        assert_eq!(set, (tagged, grey()));
    }

    /// Returns whether `free` comes true within two seconds. Other tests' threads take the
    /// colour locks for a moment too.
    fn soon(free: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !free() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }

    /// The transform cache and the colour settings are free while an image's profile is parsed,
    /// so that a slow one holds up no other decoding thread and no change in Preferences.
    #[test]
    fn parses_a_profile_with_the_cache_free() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static PARSED: AtomicBool = AtomicBool::new(false);
        fn check(c: &super::Colour) {
            assert!(
                soon(|| c.transforms.try_lock().is_ok()),
                "the cache is locked"
            );
            assert!(
                soon(|| super::COLOUR.try_write().is_ok()),
                "the settings are locked"
            );
            PARSED.store(true, Ordering::Relaxed);
        }
        let _held = super::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
        // A profile no other test has, so that it is not in the cache already.
        let mut p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        p3[31] = 177;
        super::PARSING.set(Some(check));
        super::to_srgb(image::DynamicImage::new_rgb8(1, 1), Some(&p3));
        super::PARSING.set(None);
        assert!(PARSED.load(Ordering::Relaxed));
    }

    /// The colour settings are free while the info panel's colour source reads the file, so that
    /// a stalled share holds up no change in Preferences and no decoding thread.
    #[test]
    fn reads_the_colour_source_with_the_settings_free() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static READ: AtomicBool = AtomicBool::new(false);
        fn check() {
            assert!(
                soon(|| super::COLOUR.try_write().is_ok()),
                "the settings are locked"
            );
            READ.store(true, Ordering::Relaxed);
        }
        let _held = super::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
        super::READING.set(Some(check));
        super::colour_source(std::path::Path::new("/nonexistent/glimt.jpg"));
        super::READING.set(None);
        assert!(READ.load(Ordering::Relaxed));
    }

    /// Images for the screen wait for colord at start until the deadline, and not at all after.
    #[test]
    fn waits_for_the_screen_profile_until_the_deadline() {
        let start = std::time::Instant::now();
        *super::SCREEN_WAIT.0.lock().unwrap() = Some(start + std::time::Duration::from_millis(40));
        super::wait_for_screen();
        let waited = start.elapsed().as_millis();
        assert!((35..1000).contains(&waited), "{waited} ms");
        assert!(super::SCREEN_WAIT.0.lock().unwrap().is_none());
        // A second wait of its own would take at least the 40 ms above. The bound leaves room
        // for a busy machine running the tests in parallel, where 5 ms was too tight.
        let start = std::time::Instant::now();
        super::wait_for_screen();
        let again = start.elapsed().as_millis();
        assert!(again < 30, "{again} ms");
    }

    /// What gdbus prints for colord: the first object or file, and nothing for an empty list.
    #[test]
    fn reads_colord() {
        let devices = "([objectpath '/org/freedesktop/ColorManager/devices/a', '/b'],)\n";
        let first = super::first_quoted(devices);
        assert_eq!(first, Some("/org/freedesktop/ColorManager/devices/a"));
        assert_eq!(
            super::first_quoted("(<'/x/edid.icc'>,)"),
            Some("/x/edid.icc")
        );
        assert_eq!(super::first_quoted("(@ao [],)\n"), None);
        assert_eq!(super::first_quoted("(<''>,)"), None);
    }
}
