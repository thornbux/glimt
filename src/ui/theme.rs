//! Glimt's look: Inter for text, Phosphor icons, rounder widgets with more room, and the
//! desktop's light or dark style and accent colour, from the XDG desktop portal (GNOME, KDE).

use crate::support::threads::{Receiving, Took};
use eframe::egui;
use std::sync::Arc;
use std::sync::mpsc;

/// Phosphor icons (regular), by code point.
pub mod icon {
    pub const UP: &str = "\u{e08e}";
    pub const PLAY: &str = "\u{e3d0}";
    pub const STOP: &str = "\u{e46c}";
    pub const FOLDER: &str = "\u{e24a}";
    pub const CLOSE: &str = "\u{e4f6}";
    // The menus: submenu titles and rows that are no Action (those have Action::icon).
    pub const STACK: &str = "\u{e466}"; // stack
    pub const TOOL: &str = "\u{e596}"; // puzzle-piece
    pub const APP: &str = "\u{e5da}"; // app-window
    #[cfg(feature = "marks")]
    pub const STAR: &str = "\u{e46a}"; // star
    pub const ORIENTATION: &str = "\u{e094}"; // arrows-clockwise
    pub const LAYOUT: &str = "\u{e464}"; // squares-four
    pub const PALETTE: &str = "\u{e6c8}"; // palette
    pub const COLOUR: &str = "\u{e566}"; // drop-half
    pub const FILTER: &str = "\u{e434}"; // sliders-horizontal
    pub const SIZE: &str = "\u{e0a2}"; // arrows-out
    pub const ZOOM: &str = "\u{e310}"; // magnifying-glass-plus
    pub const MOON: &str = "\u{e330}"; // moon
    pub const SUN: &str = "\u{e472}"; // sun
    pub const MONITOR: &str = "\u{e32e}"; // monitor
    pub const SNOWFLAKE: &str = "\u{e5aa}"; // snowflake
    pub const HALF: &str = "\u{e18c}"; // circle-half
    pub const HALF_TILT: &str = "\u{e18e}"; // circle-half-tilt
    pub const SUN_DIM: &str = "\u{e474}"; // sun-dim
    pub const BRUSH: &str = "\u{e6f0}"; // paint-brush
    pub const CIRCLE: &str = "\u{e18a}"; // circle
    pub const SQUARE: &str = "\u{e45e}"; // square
    pub const COLUMNS: &str = "\u{e546}"; // columns
    pub const ROWS: &str = "\u{e5a2}"; // rows
    pub const GRID_FOUR: &str = "\u{e296}"; // grid-four
    pub const TOGETHER: &str = "\u{e0a6}"; // arrows-out-simple
    pub const CURSOR: &str = "\u{e1dc}"; // cursor
    pub const FOLDER_OPEN: &str = "\u{e256}"; // folder-open
    pub const RECENT: &str = "\u{e1a0}"; // clock-counter-clockwise
    pub const MOVE_IN: &str = "\u{e098}"; // arrows-down-up
    // Preferences' sections.
    pub const KEYBOARD: &str = "\u{e2d8}"; // keyboard
    pub const IMAGE: &str = "\u{e2ca}"; // image
    pub const LIST: &str = "\u{e2f2}"; // list-bullets
    pub const TRASH: &str = "\u{e4a6}"; // trash
    pub const CPU: &str = "\u{e610}"; // cpu
    #[cfg(feature = "video")]
    pub const FILM: &str = "\u{e792}"; // film-strip
    pub const DESKTOP: &str = "\u{e560}"; // desktop
    pub const STACK_PLUS: &str = "\u{edf6}"; // stack-plus
    pub const CARET_DOWN: &str = "\u{e136}"; // caret-down
    pub const CARET_RIGHT: &str = "\u{e13a}"; // caret-right
}

/// The themes that come with Glimt; "Desktop" sets nothing.
pub(crate) const BUILT_IN: [(&str, &str); 6] = [
    ("Desktop", ""),
    ("Light", include_str!("../../themes/Light.theme")),
    ("Dark", include_str!("../../themes/Dark.theme")),
    ("Nord", include_str!("../../themes/Nord.theme")),
    // For low vision: white on black, and black on white.
    (
        "High Contrast",
        include_str!("../../themes/High Contrast.theme"),
    ),
    ("Contrast", include_str!("../../themes/Contrast.theme")),
];

/// The theme for those who ask the desktop for high contrast.
pub const HIGH_CONTRAST: &str = "High Contrast";

/// A theme: what a `.theme` file sets, as `key=value` lines; what it leaves out follows the
/// desktop (light or dark, accent) or Glimt's defaults.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Theme {
    pub dark: Option<bool>,
    pub accent: Option<egui::Color32>,
    pub panel: Option<egui::Color32>,
    pub window: Option<egui::Color32>,
    pub field: Option<egui::Color32>,
    pub faint: Option<egui::Color32>,
    pub hover: Option<egui::Color32>,
    pub active: Option<egui::Color32>,
    pub border: Option<egui::Color32>,
    pub text: Option<egui::Color32>,
    pub radius: Option<u8>,
    pub text_size: Option<f32>,
}

impl Theme {
    /// Lines `key=value`; `#` starts a comment; colours as `#rrggbb`. Unknown keys and bad
    /// values are left out, so an older Glimt reads a newer theme.
    pub fn parse(text: &str) -> Theme {
        let mut t = Theme::default();
        for line in text.lines().map(str::trim) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let color = || {
                let hex = value.strip_prefix('#').filter(|h| h.len() == 6)?;
                let [_, r, g, b] = u32::from_str_radix(hex, 16).ok()?.to_be_bytes();
                Some(egui::Color32::from_rgb(r, g, b))
            };
            match key {
                "dark" => t.dark = value.parse().ok(),
                "accent" => t.accent = color(),
                "panel" => t.panel = color(),
                "window" => t.window = color(),
                "field" => t.field = color(),
                "faint" => t.faint = color(),
                "hover" => t.hover = color(),
                "active" => t.active = color(),
                "border" => t.border = color(),
                "text" => t.text = color(),
                "radius" => t.radius = value.parse::<u8>().ok().map(|r| r.min(20)),
                // "NaN" and "inf" parse as numbers; clamp keeps NaN as it is.
                "text_size" => {
                    t.text_size = (value.parse::<f32>().ok())
                        .filter(|s| s.is_finite())
                        .map(|s| s.clamp(10.0, 24.0))
                }
                _ => {}
            }
        }
        t
    }

    /// The theme as a file, every value filled in from what `ctx` shows now, to start a new one.
    pub fn file_from(ctx: &egui::Context) -> String {
        let (s, dark) = (ctx.style_of(ctx.theme()), ctx.theme() == egui::Theme::Dark);
        let v = &s.visuals;
        let hex = |c: egui::Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        let size = s
            .text_styles
            .get(&egui::TextStyle::Body)
            .map_or(14.0, |f| f.size);
        format!(
            "# Glimt theme: every value may be left out, to follow the desktop or the defaults.\n\
             dark={dark}\naccent={}\npanel={}\nwindow={}\nfield={}\nfaint={}\nhover={}\n\
             active={}\nborder={}\ntext={}\nradius={}\ntext_size={size:.1}\n",
            hex(v.selection.bg_fill),
            hex(v.panel_fill),
            hex(v.window_fill),
            hex(v.extreme_bg_color),
            hex(v.faint_bg_color),
            hex(v.widgets.hovered.bg_fill),
            hex(v.widgets.active.bg_fill),
            hex(v.window_stroke.color),
            hex(v.text_color()),
            v.widgets.inactive.corner_radius.nw,
        )
    }
}

/// What the desktop asks of the look, from the XDG desktop portal.
#[derive(Clone, Copy, Default)]
pub struct DesktopLook {
    /// Dark or light; `None` for no preference.
    pub dark: Option<bool>,
    pub accent: Option<egui::Color32>,
    /// High contrast, as GNOME's and KDE's accessibility settings set it.
    pub high_contrast: bool,
    /// The size of text, as GNOME's Large Text and text scaling set it.
    pub text_scale: Option<f32>,
}

/// What the desktop says: its dark style and accent, and its screen profile.
pub(crate) struct DesktopSettings {
    /// The desktop's dark style and accent colour, once read from the portal.
    look: DesktopLook,
    look_answer: Receiving<DesktopLook>,
    /// The desktop's screen profile file, once read from colord.
    screen_profile: Option<String>,
    screen_profile_answer: Receiving<Option<String>>,
}

impl DesktopSettings {
    /// The desktop asked in the background: gdbus takes a moment, and the window shows
    /// meanwhile and takes the desktop's look after. Its screen profile only if `profile`.
    pub(crate) fn new(ctx: &egui::Context, profile: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_desktop_look());
            repaint.request_repaint();
        });
        DesktopSettings {
            look: Default::default(),
            look_answer: Receiving::of(rx),
            screen_profile: None,
            screen_profile_answer: profile
                .then(|| crate::images::colour::read_desktop_screen(ctx))
                .map_or_else(Receiving::none, Receiving::of),
        }
    }

    /// The desktop's dark style, accent colour, contrast and text size.
    pub(crate) fn look(&self) -> DesktopLook {
        self.look
    }

    /// The desktop's screen profile file. Returns `None` until colord has told it, or when it
    /// has none.
    pub(crate) fn screen_profile(&self) -> Option<&str> {
        self.screen_profile.as_deref()
    }

    /// Asks colord for the desktop's screen profile in the background, unless it is known.
    pub(crate) fn ask_screen_profile(&mut self, ctx: &egui::Context) {
        if self.screen_profile.is_none() {
            self.screen_profile_answer =
                Receiving::of(crate::images::colour::read_desktop_screen(ctx));
        }
    }

    /// Takes what the desktop answered. Returns whether its look came, and whether a screen
    /// profile came.
    pub(crate) fn receive(&mut self) -> (bool, bool) {
        let look = match self.look_answer.take() {
            Took::Answer(look) => Some(look),
            _ => None,
        };
        if let Some(look) = look {
            self.look = look;
        }
        let mut screen = false;
        if let Took::Answer(path) = self.screen_profile_answer.take() {
            self.screen_profile = path;
            screen = self.screen_profile.is_some();
        }
        (look.is_some(), screen)
    }
}

/// The Phosphor icons first (Inter has glyphs of its own at some of their code points), then
/// Inter, and egui's own fonts after them for the scripts and emoji Inter lacks. The Phosphor
/// file is cut to its icons, as the full one also has letters:
/// `pyftsubset Phosphor.ttf --unicodes=U+E000-F8FF --layout-features='' --output-file=...`
pub fn fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        let data = egui::FontData::from_static(bytes);
        fonts.font_data.insert(name.to_owned(), Arc::new(data));
    };
    add(
        "Inter",
        include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    );
    add(
        "Phosphor",
        include_bytes!("../../assets/fonts/Phosphor.ttf"),
    );
    let families = &mut fonts.families;
    let proportional = families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "Phosphor".to_owned());
    proportional.insert(1, "Inter".to_owned());
    let monospace = families.entry(egui::FontFamily::Monospace).or_default();
    monospace.push("Phosphor".to_owned());
    ctx.set_fonts(fonts);
}

/// Where more themes go: `$XDG_CONFIG_HOME/glimt/themes`, one `name.theme` file each.
pub fn own_themes_dir() -> Option<std::path::PathBuf> {
    crate::files::config::xdg_dir("XDG_CONFIG_HOME", ".config", "glimt").map(|d| d.join("themes"))
}

/// The built-in themes, then the user's, by name.
/// Whether `name` is one of the themes built in.
pub fn is_built_in(name: &str) -> bool {
    BUILT_IN.iter().any(|b| b.0 == name)
}

/// Returns a theme's name as menus show it. A built-in theme's name is in the language in use.
pub fn shown_name(name: &str) -> &str {
    (BUILT_IN.iter().find(|b| b.0 == name)).map_or(name, |b| crate::support::lang::tr(b.0))
}

pub fn names() -> Vec<String> {
    let mut own: Vec<String> = (own_themes_dir().and_then(|d| std::fs::read_dir(d).ok()))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "theme"))
        .filter_map(|p| Some(p.file_stem()?.to_string_lossy().into_owned()))
        .filter(|n| !BUILT_IN.iter().any(|b| b.0 == n))
        .collect();
    own.sort_by_key(|n| n.to_lowercase());
    BUILT_IN.iter().map(|b| b.0.to_owned()).chain(own).collect()
}

/// The theme of that name: the user's file if there is one, else the built-in one, else
/// "Desktop". Read each time, so a changed file shows when it is chosen again.
pub fn load(name: &str) -> Theme {
    let file = own_themes_dir().map(|d| d.join(format!("{name}.theme")));
    match file.and_then(|f| crate::images::decode::read_text(f).ok()) {
        Some(text) => Theme::parse(&text),
        None => BUILT_IN
            .iter()
            .find(|b| b.0 == name)
            .map_or_else(Theme::default, |b| Theme::parse(b.1)),
    }
}

/// Show `theme`, with the desktop's light or dark style and accent where it sets none.
pub fn apply(
    ctx: &egui::Context,
    theme: &Theme,
    desktop_dark: Option<bool>,
    desktop_accent: Option<egui::Color32>,
) {
    let dark = theme.dark.or(desktop_dark).unwrap_or(true);
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    // GNOME's blue when the desktop names none.
    let accent =
        (theme.accent.or(desktop_accent)).unwrap_or(egui::Color32::from_rgb(0x35, 0x84, 0xe4));
    for t in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(t, |s| apply_style(s, t == egui::Theme::Dark, accent, theme));
    }
}

fn apply_style(style: &mut egui::Style, dark: bool, accent: egui::Color32, theme: &Theme) {
    let gray = |v: u8| egui::Color32::from_gray(v);
    // The theme's own colour, or a gray for the dark style and one for the light style.
    let pick = |own: Option<egui::Color32>, on_dark: u8, on_light: u8| {
        own.unwrap_or(gray(if dark { on_dark } else { on_light }))
    };
    // The release tests' and the screenshots' build shows the text cursor without blinking,
    // so a shot at rest is the same in every run.
    #[cfg(feature = "test-hooks")]
    {
        style.visuals.text_cursor.blink = false;
    }
    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(8.0, 6.0);
    spacing.button_padding = egui::vec2(8.0, 4.0);
    spacing.menu_margin = egui::Margin::same(6);
    spacing.window_margin = egui::Margin::same(12);
    if let Some(size) = theme.text_size {
        // The body text at that size, and every other text style in proportion to it.
        let defaults = egui::Style::default().text_styles;
        let body = defaults
            .get(&egui::TextStyle::Body)
            .map_or(size, |f| f.size);
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = defaults.get(text_style).map_or(font.size, |f| f.size) * size / body;
        }
    }
    let radius = theme.radius.unwrap_or(7);
    let (hover, active) = (pick(theme.hover, 58, 225), pick(theme.active, 70, 210));
    let visuals = &mut style.visuals;
    visuals.panel_fill = pick(theme.panel, 30, 246);
    visuals.window_fill = pick(theme.window, 36, 252);
    visuals.extreme_bg_color = pick(theme.field, 22, 255);
    visuals.faint_bg_color = pick(theme.faint, 34, 240);
    visuals.override_text_color = theme.text;
    let border = pick(theme.border, 50, 215);
    visuals.window_stroke = egui::Stroke::new(1.0, border);
    visuals.window_corner_radius = egui::CornerRadius::same(radius.saturating_add(5));
    visuals.menu_corner_radius = egui::CornerRadius::same(radius.saturating_add(3));
    let shadow = egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: egui::Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    visuals.window_shadow = shadow;
    visuals.popup_shadow = shadow;
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent;
    // Black on a light accent (yellow), white on a dark one.
    let light = 0.299 * accent.r() as f32 + 0.587 * accent.g() as f32 + 0.114 * accent.b() as f32;
    visuals.selection.stroke.color = gray(if light > 160.0 { 0 } else { 255 });
    let widgets = &mut visuals.widgets;
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = egui::CornerRadius::same(radius);
    }
    // Flat buttons that light up under the pointer, as in GNOME and KDE. A theme that names a
    // border colour gets edges on fields and buttons too, which the contrast themes need.
    widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    widgets.inactive.bg_stroke = theme
        .border
        .map_or(egui::Stroke::NONE, |c| egui::Stroke::new(1.0, c));
    widgets.hovered.weak_bg_fill = hover;
    widgets.hovered.bg_fill = hover;
    widgets.hovered.bg_stroke = egui::Stroke::NONE;
    widgets.active.weak_bg_fill = active;
    widgets.active.bg_fill = active;
    widgets.active.bg_stroke = egui::Stroke::NONE;
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
}

/// The desktop's dark style, accent colour and contrast, from the XDG desktop portal; `None`
/// and false where it says nothing. Runs `gdbus`, so it belongs in a background thread.
pub fn read_desktop_look() -> DesktopLook {
    let read = |namespace: &str, key: &str| {
        let args = [
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.ReadOne",
            namespace,
            key,
        ];
        crate::files::external::short_output_of("gdbus", &args, 1 << 16).unwrap_or_default()
    };
    DesktopLook {
        dark: parse_scheme(&read("org.freedesktop.appearance", "color-scheme")),
        accent: parse_accent(&read("org.freedesktop.appearance", "accent-color")),
        high_contrast: parse_contrast(&read("org.freedesktop.appearance", "contrast")),
        // GNOME's text scaling factor, from the portal too: gsettings in a Flatpak reads the
        // sandbox's settings, not the desktop's.
        text_scale: parse_scale(&read("org.gnome.desktop.interface", "text-scaling-factor")),
    }
}

/// "(<1.25>,)": GNOME's text scaling factor (1.25 for Large Text). Returns `None` elsewhere, or
/// outside 0.5–3.
fn parse_scale(text: &str) -> Option<f32> {
    let n = text.trim().trim_start_matches("(<").split('>').next()?;
    (n.parse::<f32>().ok()).filter(|s| (0.5..=3.0).contains(s))
}

/// "(<uint32 1>,)": 1 is high contrast, 0 none.
fn parse_contrast(text: &str) -> bool {
    text.split("uint32 ")
        .nth(1)
        .is_some_and(|n| n.starts_with('1'))
}

/// "(<uint32 1>,)": 1 is dark, 2 light, 0 no preference.
fn parse_scheme(text: &str) -> Option<bool> {
    let n = text.split("uint32 ").nth(1)?.split('>').next()?.trim();
    match n {
        "1" => Some(true),
        "2" => Some(false),
        _ => None,
    }
}

/// "(<(0.93, 0.36, 0.0)>,)": red, green and blue from 0 to 1; outside that range means none.
fn parse_accent(text: &str) -> Option<egui::Color32> {
    let inner = text.split("<(").nth(1)?.split(")>").next()?;
    let c: Vec<f32> = inner
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let [r, g, b] = c[..] else { return None };
    let ok = |v: f32| (0.0..=1.0).contains(&v);
    (ok(r) && ok(g) && ok(b))
        .then(|| egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8))
}

#[cfg(test)]
mod tests {
    /// Every character of the UI text (and the comments, which cost nothing) is in Inter or
    /// Phosphor, or a Latin letter: egui 0.37 stopped bundling the emoji fonts, and a glyph no
    /// font has is drawn as nothing, as the U+23F7 triangle after "Sort by name" was.
    #[test]
    fn every_glyph_of_the_ui_text_is_in_a_font_glimt_ships() {
        // What Inter has, of the signs the texts use; the Phosphor icons are U+E000–F8FF.
        const IN_INTER: &str = "©«°±·»¿×–“”„…⁴←↑→↓≈★✓⬆";
        // Every source file and translation, read from src/ and po/: one added is checked unlisted.
        for (name, text) in crate::support::source_rules::sources() {
            for c in text.chars() {
                let fine = c.is_ascii()
                    || c.is_alphabetic()
                    || ('\u{e000}'..='\u{f8ff}').contains(&c)
                    || IN_INTER.contains(c);
                assert!(
                    fine,
                    "{name}: U+{:04X} {c:?} is in no font Glimt ships",
                    c as u32
                );
            }
        }
    }

    /// The portal's answers broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_desktop_answers() {
        for seed in ["(<uint32 1>,)", "(<(0.93, 0.36, 0.0)>,)", "(<1.25>,)"] {
            for bytes in crate::support::test_support::broken(seed.as_bytes()) {
                let text = String::from_utf8_lossy(&bytes);
                super::parse_scheme(&text);
                super::parse_contrast(&text);
                super::parse_accent(&text);
                super::parse_scale(&text);
            }
        }
    }

    #[test]
    fn reads_the_portal() {
        assert_eq!(super::parse_scheme("(<uint32 1>,)\n"), Some(true));
        assert_eq!(super::parse_scheme("(<uint32 0>,)\n"), None);
        assert_eq!(super::parse_scheme(""), None);
        assert!(super::parse_contrast("(<uint32 1>,)\n"));
        assert!(!super::parse_contrast("(<uint32 0>,)\n") && !super::parse_contrast(""));
        assert_eq!(super::parse_scale("(<1.25>,)\n"), Some(1.25));
        assert_eq!(super::parse_scale("(<9.0>,)"), None);
        assert_eq!(super::parse_scale(""), None);
        let orange = super::parse_accent("(<(0.92941176891326904, 0.35686275362968445, 0.0)>,)\n");
        assert_eq!(orange, Some(eframe::egui::Color32::from_rgb(237, 91, 0)));
        // KDE and GNOME without an accent give a colour out of range.
        assert_eq!(super::parse_accent("(<(-1.0, -1.0, -1.0)>,)"), None);
    }

    #[test]
    fn reads_themes() {
        let t = super::Theme::parse(
            "# x\ndark = true\naccent=#88c0d0\nradius=99\ntext=#fff\nnew_key=1\n",
        );
        assert_eq!(t.dark, Some(true));
        assert_eq!(
            t.accent,
            Some(eframe::egui::Color32::from_rgb(0x88, 0xc0, 0xd0))
        );
        assert_eq!(t.radius, Some(20));
        assert_eq!(t.text, None);
        assert_eq!(super::Theme::parse("text_size=NaN").text_size, None);
        // Every built-in theme reads, and sets what it says.
        for (name, text) in super::BUILT_IN {
            let t = super::Theme::parse(text);
            assert_eq!(t == super::Theme::default(), name == "Desktop", "{name}");
        }
        // A saved theme reads back as it was shown.
        let ctx = eframe::egui::Context::default();
        let nord = super::Theme::parse(super::BUILT_IN[3].1);
        super::apply(&ctx, &nord, None, None);
        let saved = super::Theme::parse(&super::Theme::file_from(&ctx));
        assert_eq!(
            (saved.accent, saved.panel, saved.text),
            (nord.accent, nord.panel, nord.text)
        );
    }

    /// Theme files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_themes() {
        let seed = "dark = true\naccent=#88c0d0\nradius=9\ntext=#fff\ntext_size=15\n";
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            super::Theme::parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
