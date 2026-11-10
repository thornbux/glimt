//! Colour management: images from their own ICC profile (or Adobe RGB from EXIF, or the
//! profile chosen for images without one) to sRGB or to the screen's profile, with moxcms. The
//! screen profile can come from colord.

use crate::files::config::Config;
use crate::metadata::exif;
use crate::support::lang::tr;
use crate::support::threads::lock_even_if_poisoned;
use crate::support::threads::{AnswerSender, Receiving, Took, Working, answer_channel};
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

/// Held while a colour profile chosen is read and set (`ProfileChanges`), so that two chosen
/// at once are set one after the other.
static PROFILE_CHANGES: Mutex<()> = Mutex::new(());

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
    /// than the transform itself. By (source, to the screen, the pixels' form): 0 is sRGB, 1
    /// the input profile, the rest a hash of the image's profile.
    pub transforms: Arc<Mutex<Transforms>>,
}

/// A colour profile to read and set in the background (`ProfileChanges::ask`).
pub(crate) enum ProfileAsked {
    /// The screen profile in the file at `path`, chosen by the user (`chosen`), or the one that
    /// counts now, the desktop's or the chosen one.
    Screen { path: String, chosen: bool },
    /// The profile for images without one, by its name in `INPUT_PROFILES` or its file.
    Input { name: String },
}

/// A colour profile read and set in the background, with what `set_screen` or `set_input`
/// answered.
pub(crate) enum ProfileSet {
    Screen {
        path: String,
        chosen: bool,
        answer: Result<bool, String>,
    },
    Input {
        name: String,
        answer: Result<(), String>,
    },
}

/// The colour profiles chosen, read and set in threads of their own, because a profile's file
/// may be on a share that stopped answering. Their answers keep the busy pointer until the
/// window has taken them.
#[derive(Default)]
pub(crate) struct ProfileChanges {
    sender: Option<AnswerSender<ProfileSet>>,
    answers: Receiving<(ProfileSet, Working)>,
}

impl ProfileChanges {
    /// Reads and sets the profile `asked` in a thread of its own, after the ones asked before.
    pub(crate) fn ask(
        &mut self,
        busy: &Arc<AtomicUsize>,
        ctx: &egui::Context,
        asked: ProfileAsked,
    ) {
        let sender = match &self.sender {
            Some(sender) => sender.clone(),
            None => {
                let (sender, answers) = answer_channel(busy);
                self.answers = Receiving::of(answers);
                self.sender.insert(sender).clone()
            }
        };
        let (ctx, pointer) = (ctx.clone(), Working::new(busy));
        std::thread::spawn(move || {
            let set = {
                let _one_at_a_time = lock_even_if_poisoned(&PROFILE_CHANGES);
                match asked {
                    ProfileAsked::Screen { path, chosen } => {
                        let answer = set_screen(&path);
                        ProfileSet::Screen {
                            path,
                            chosen,
                            answer,
                        }
                    }
                    ProfileAsked::Input { name } => {
                        let answer = set_input(&name);
                        ProfileSet::Input { name, answer }
                    }
                }
            };
            let _ = sender.send(set);
            drop(pointer);
            ctx.request_repaint();
        });
    }

    /// Returns the next profile set, with the busy pointer it holds until it is handled, or
    /// `None` while none has come.
    pub(crate) fn next(&mut self) -> Option<(ProfileSet, Working)> {
        match self.answers.next() {
            Took::Answer(answer) => Some(answer),
            Took::Nothing | Took::Gone => None,
        }
    }
}

/// Makes a built-in profile of `INPUT_PROFILES`.
pub type BuiltIn = fn() -> Option<moxcms::ColorProfile>;

/// Colour transforms by (source, to the screen, the pixels' form); `None` where none can be
/// made.
pub type Transforms = HashMap<(u64, bool, Form), Option<Transform>>;

/// The form of the pixels a colour transform reads and writes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Form {
    /// 16 bits for each channel, else 8.
    pub sixteen_bits: bool,
    /// An alpha channel, which the transform carries over unchanged.
    pub alpha: bool,
}

impl Form {
    /// The form `img` goes through a transform in: its own depth (a float as 16 bits) and its
    /// own alpha, grey as RGB. Before, every image came out in 8-bit RGBA, so Export Frames
    /// wrote a 16-bit page with a profile in 8 bits.
    pub fn of(img: &image::DynamicImage) -> Form {
        let colour = img.color();
        Form {
            sixteen_bits: colour.bytes_per_pixel() > colour.channel_count(),
            alpha: colour.has_alpha(),
        }
    }
}

/// A colour transform of 8-bit or of 16-bit pixels.
#[derive(Clone)]
pub enum Transform {
    Eight(Arc<moxcms::Transform8BitExecutor>),
    Sixteen(Arc<moxcms::Transform16BitExecutor>),
}

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
/// Unchanged when both are sRGB, or with colour management off. A profile that does not fit the
/// pixels (`profile_fits`) counts as none.
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
    let icc = icc.filter(|p| c.use_image_profile && profile_fits(p, img.color()));
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
    let form = Form::of(&img);
    let key = (from, screen, form);
    let cached = (lock_even_if_poisoned(&c.transforms)).get(&key).cloned();
    let t = cached.unwrap_or_else(|| {
        // The transform is made without the cache locked. A large or crafted profile can take
        // long to parse, and the other decoding threads should not wait for it.
        let t = make_transform(c, icc, from, screen, form);
        let mut transforms = lock_even_if_poisoned(&c.transforms);
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
        Some(t) => transformed(&t, img),
        None => img,
    }
}

/// Returns whether the ICC profile `icc` describes pixels of the colour type `pixels`: its colour
/// space, in bytes 16 to 20 of its header, is RGB for colour pixels and grey for grey ones. A
/// CMYK JPEG or TIFF is decoded to RGB but keeps its CMYK profile. Before, the view read the RGB
/// pixels as CMYK through that profile and showed the image nearly black. A derived file keeps
/// a profile by the same rule.
pub fn profile_fits(icc: &[u8], pixels: image::ColorType) -> bool {
    let space: &[u8] = if pixels.has_color() { b"RGB " } else { b"GRAY" };
    icc.get(16..20) == Some(space)
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
    let mut until = lock_even_if_poisoned(lock);
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
            let exif = dec.and_then(|mut d| exif::block_from_decoder(path, &mut d));
            (exif.is_some_and(|e| exif::is_adobe_rgb(&e)))
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
        let mut until = lock_even_if_poisoned(lock);
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
/// `Colour::transforms`) to the screen with `screen`, else to sRGB, for pixels of the form
/// `form`. Returns `None` where none is needed or none can be made.
fn make_transform(
    c: &Colour,
    icc: Option<&[u8]>,
    from: u64,
    screen: bool,
    form: Form,
) -> Option<Transform> {
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
        let layout = if form.alpha {
            Layout::Rgba
        } else {
            Layout::Rgb
        };
        if form.sixteen_bits {
            (source.create_transform_16bit(layout, &target, layout, options))
                .map(Transform::Sixteen)
        } else {
            (source.create_transform_8bit(layout, &target, layout, options)).map(Transform::Eight)
        }
    };
    // A profile without tables for the intent has the perceptual ones, as ICC says.
    (make(c.intent).or_else(|_| make(RenderingIntent::Perceptual))).ok()
}

/// `img` through the colour transform `t`, made for the pixels' form (`Form::of`), in that
/// form.
pub fn transformed(t: &Transform, img: image::DynamicImage) -> image::DynamicImage {
    let alpha = img.color().has_alpha();
    match t {
        Transform::Eight(t) if alpha => in_place(&**t, img.into_rgba8(), 4),
        Transform::Eight(t) => in_place(&**t, img.into_rgb8(), 3),
        Transform::Sixteen(t) if alpha => in_place(&**t, img.into_rgba16(), 4),
        Transform::Sixteen(t) => in_place(&**t, img.into_rgb16(), 3),
    }
}

/// `pixels` of `channels` channels through `t`, a quarter of a million pixels at a time through
/// one small buffer: a full-size photo for printing or saving does not need a second copy of
/// itself.
fn in_place<P, T>(
    t: &(dyn moxcms::TransformExecutor<T> + Send + Sync),
    mut pixels: image::ImageBuffer<P, Vec<T>>,
    channels: usize,
) -> image::DynamicImage
where
    P: image::Pixel<Subpixel = T>,
    T: Copy + Default,
    image::DynamicImage: From<image::ImageBuffer<P, Vec<T>>>,
{
    let part = (1 << 18) * channels; // whole pixels
    let mut out = vec![T::default(); part.min(pixels.len())];
    for part in pixels.chunks_mut(part) {
        let out = &mut out[..part.len()];
        if t.transform(part, out).is_ok() {
            part.copy_from_slice(out);
        }
    }
    pixels.into()
}

/// Sets up the colour settings in effect at start. Returns the screen profile's or input profile's
/// error, if either could not be read. The wait for the desktop's profile starts later, where
/// colord is asked (`start_screen_wait`).
pub(crate) fn set_up_colour(config: &Config) -> Option<String> {
    COLOR_MANAGEMENT.store(config.color_management, Ordering::Relaxed);
    VIEW_FILTER.store(config.view_filter, Ordering::Relaxed);
    let error = (set_screen(&config.screen_profile))
        .and_then(|_| set_input(&config.input_profile))
        .err();
    set_colour(|c| c.use_image_profile = config.use_image_profile);
    set_intent(&config.rendering_intent);
    error
}

/// Makes the images for the screen wait a moment for the desktop's screen profile, when colour
/// management is on. It is called just before colord is asked, in the window, and not when the
/// settings are set up in `main`: the window takes 100 to 300 ms to open in between, and the
/// wait ran out before the question was asked.
pub(crate) fn start_screen_wait() {
    if COLOR_MANAGEMENT.load(Ordering::Relaxed) {
        let deadline = Instant::now() + std::time::Duration::from_millis(SCREEN_WAIT_MS);
        *lock_even_if_poisoned(&SCREEN_WAIT.0) = Some(deadline);
    }
}

#[cfg(test)]
mod tests {
    use crate::support::threads::lock_even_if_poisoned;

    /// A profile chosen is read and set in a thread of its own, and its answer comes back with
    /// the busy pointer: a file that is no profile is refused with the reason, and a built-in
    /// profile is set.
    #[test]
    fn a_profile_is_set_in_the_background() {
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
        let junk =
            std::env::temp_dir().join(format!("glimt-test-icc-chosen-{}", std::process::id()));
        std::fs::write(&junk, b"no profile").unwrap();
        let (busy, ctx) = (std::sync::Arc::default(), eframe::egui::Context::default());
        let mut changes = super::ProfileChanges::default();
        let junk_name = junk.to_string_lossy().into_owned();
        let asked = super::ProfileAsked::Input {
            name: junk_name.clone(),
        };
        changes.ask(&busy, &ctx, asked);
        let srgb = super::ProfileAsked::Input {
            name: "srgb".into(),
        };
        changes.ask(&busy, &ctx, srgb);
        let mut answers = Vec::new();
        let took = std::time::Instant::now();
        while answers.len() < 2 && took.elapsed().as_secs() < 5 {
            match changes.next() {
                Some((super::ProfileSet::Input { name, answer }, _pointer)) => {
                    answers.push((name, answer.is_ok()));
                }
                Some(_) => panic!("a screen profile was not asked for"),
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        std::fs::remove_file(&junk).unwrap();
        answers.sort();
        let mut expected = vec![(junk_name, false), ("srgb".to_owned(), true)];
        expected.sort();
        assert_eq!(answers, expected);
        assert_eq!(busy.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

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
        let parts = super::transformed(&super::Transform::Eight(t), img.into()).into_rgba8();
        assert_eq!(parts.as_raw(), &whole);
    }

    /// A 16-bit page with a profile comes out in 16 bits and without an alpha channel it did
    /// not have, so Export Frames writes it as it writes one without a profile. Before, every
    /// page with a profile came out as 8-bit RGBA.
    #[test]
    fn keeps_the_pixels_form() {
        let deep = image::DynamicImage::from(image::ImageBuffer::from_pixel(
            2,
            2,
            image::Rgb([15000u16, 41000, 15000]),
        ));
        let srgb = super::to_srgb(deep.clone(), Some(&super::ADOBE_RGB));
        assert_eq!(srgb.color(), image::ColorType::Rgb16);
        assert_ne!(srgb, deep);
        let flat =
            image::DynamicImage::from(image::RgbImage::from_pixel(2, 2, image::Rgb([60, 160, 60])));
        let srgb = super::to_srgb(flat.clone(), Some(&super::ADOBE_RGB));
        assert_eq!(srgb.color(), image::ColorType::Rgb8);
        assert_ne!(srgb, flat);
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
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
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
            lock_even_if_poisoned(&c.transforms).get(&(key, false, super::Form::of(&grey()))),
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
        assert!(lock_even_if_poisoned(&c.transforms).len() <= 64);
        drop(c);
        super::set_screen("").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A profile with a table for perceptual and another for colorimetric: the intent picks one,
    /// and saturation, which has none, takes the perceptual one.
    #[test]
    fn rendering_intent_picks_its_table() {
        use moxcms::{LutDataType, LutStore, LutType, LutWarehouse, Matrix3d};
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
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

    /// A CMYK profile with an RGB image, as a CMYK JPEG or TIFF is decoded: the pixels come out as
    /// they are. Before, moxcms read them as CMYK with K = 255 and the image showed nearly black.
    #[test]
    fn a_profile_that_does_not_fit_the_pixels_is_none() {
        use moxcms::{DataColorSpace, Layout, LutDataType, LutStore, LutType, LutWarehouse};
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
        // CMYK to XYZ through a 2×2×2×2 grid: a real CMYK profile, which moxcms makes a
        // transform from.
        let mut profile = moxcms::ColorProfile::default();
        profile.color_space = DataColorSpace::Cmyk;
        profile.pcs = DataColorSpace::Xyz;
        profile.profile_class = moxcms::ProfileClass::OutputDevice;
        profile.white_point = moxcms::Xyzd {
            x: 0.9642,
            y: 1.0,
            z: 0.8249,
        };
        profile.lut_a_to_b_perceptual = Some(LutWarehouse::Lut(LutDataType {
            num_input_channels: 4,
            num_output_channels: 3,
            num_clut_grid_points: 2,
            matrix: moxcms::Matrix3d::IDENTITY,
            num_input_table_entries: 2,
            num_output_table_entries: 2,
            input_table: LutStore::Store16([0, 65535].repeat(4)),
            clut_table: LutStore::Store16((0..16 * 3).map(|i| 20000 + i * 300).collect()),
            output_table: LutStore::Store16([0, 65535].repeat(3)),
            lut_type: LutType::Lut16,
        }));
        let cmyk = profile.encode().unwrap();
        assert_eq!(&cmyk[16..20], b"CMYK");
        let srgb = moxcms::ColorProfile::new_srgb();
        let parsed = moxcms::ColorProfile::new_from_slice(&cmyk).unwrap();
        let options = moxcms::TransformOptions::default();
        assert!(
            parsed
                .create_transform_8bit(Layout::Rgba, &srgb, Layout::Rgba, options)
                .is_ok()
        );
        let photo = || {
            image::DynamicImage::from(image::RgbImage::from_pixel(2, 2, image::Rgb([60, 160, 60])))
        };
        let unchanged = photo().into_rgba8();
        assert_eq!(super::to_srgb(photo(), Some(&cmyk)).into_rgba8(), unchanged);
        assert_eq!(
            super::to_display(photo(), Some(&cmyk)).into_rgba8(),
            unchanged
        );
    }

    /// What Glimt hands out is in the colours the file says, whatever the view's settings are:
    /// colour management off, an input profile, the image's own profile left out and another
    /// intent change nothing. Before, they reached the shared thumbnail cache and prints.
    #[test]
    fn hands_out_the_files_colours_whatever_the_view() {
        use std::sync::atomic::Ordering;
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
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
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
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
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
        super::READING.set(Some(check));
        super::colour_source(std::path::Path::new("/nonexistent/glimt.jpg"));
        super::READING.set(None);
        assert!(READ.load(Ordering::Relaxed));
    }

    /// The wait for the desktop's screen profile starts where colord is asked, not when the
    /// settings are set up before the window opens, which it outlasted.
    #[test]
    fn the_screen_wait_starts_where_colord_is_asked() {
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
        *lock_even_if_poisoned(&super::SCREEN_WAIT.0) = None;
        let config = crate::files::config::Config {
            color_management: true,
            desktop_profile: true,
            ..Default::default()
        };
        super::set_up_colour(&config);
        assert!(lock_even_if_poisoned(&super::SCREEN_WAIT.0).is_none());
        super::start_screen_wait();
        let deadline = lock_even_if_poisoned(&super::SCREEN_WAIT.0).take();
        assert!(deadline.is_some_and(|d| d > std::time::Instant::now()));
    }

    /// Images for the screen wait for colord at start until the deadline, and not at all after.
    #[test]
    fn waits_for_the_screen_profile_until_the_deadline() {
        // The test above sets the deadline too.
        let _held = lock_even_if_poisoned(&super::COLOUR_TEST);
        let start = std::time::Instant::now();
        *lock_even_if_poisoned(&super::SCREEN_WAIT.0) =
            Some(start + std::time::Duration::from_millis(40));
        super::wait_for_screen();
        let waited = start.elapsed().as_millis();
        assert!((35..1000).contains(&waited), "{waited} ms");
        assert!(lock_even_if_poisoned(&super::SCREEN_WAIT.0).is_none());
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
