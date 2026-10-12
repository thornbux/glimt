//! Glimt's look: Inter for text, Phosphor icons, rounder widgets with more room, and the
//! desktop's light or dark style and accent colour, from the XDG desktop portal (GNOME, KDE).

use eframe::egui;
use std::sync::Arc;

/// Phosphor icons (regular), by code point.
pub mod icon {
    pub const BACK: &str = "\u{e058}";
    pub const FORWARD: &str = "\u{e06c}";
    pub const UP: &str = "\u{e08e}";
    pub const HOME: &str = "\u{e2c2}";
    pub const REFRESH: &str = "\u{e036}";
    pub const ZOOM_IN: &str = "\u{e310}";
    pub const ZOOM_OUT: &str = "\u{e30e}";
    pub const FIT: &str = "\u{e1d0}";
    pub const THUMBNAILS: &str = "\u{e2ca}";
    pub const PLAY: &str = "\u{e3d0}";
    pub const STOP: &str = "\u{e46c}";
    pub const SETTINGS: &str = "\u{e270}";
    pub const FOLDER: &str = "\u{e24a}";
    pub const CLOSE: &str = "\u{e4f6}";
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
    add("Inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add("Phosphor", include_bytes!("../assets/fonts/Phosphor.ttf"));
    let families = &mut fonts.families;
    let proportional = families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "Phosphor".to_owned());
    proportional.insert(1, "Inter".to_owned());
    let monospace = families.entry(egui::FontFamily::Monospace).or_default();
    monospace.push("Phosphor".to_owned());
    ctx.set_fonts(fonts);
}

/// `pref`: "system", "dark" or "light". The style of both themes, with `accent` if the desktop
/// has one.
pub fn apply(
    ctx: &egui::Context,
    pref: &str,
    desktop_dark: Option<bool>,
    accent: Option<egui::Color32>,
) {
    let dark = match pref {
        "dark" => true,
        "light" => false,
        _ => desktop_dark.unwrap_or(true),
    };
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    // GNOME's blue when the desktop names none.
    let accent = accent.unwrap_or(egui::Color32::from_rgb(0x35, 0x84, 0xe4));
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |s| style(s, theme == egui::Theme::Dark, accent));
    }
}

fn style(s: &mut egui::Style, dark: bool, accent: egui::Color32) {
    let gray = |v: u8| egui::Color32::from_gray(v);
    let sp = &mut s.spacing;
    sp.item_spacing = egui::vec2(8.0, 6.0);
    sp.button_padding = egui::vec2(8.0, 4.0);
    sp.menu_margin = egui::Margin::same(6);
    sp.window_margin = egui::Margin::same(12);
    let v = &mut s.visuals;
    let (panel, window, field, hover, active) = if dark {
        (gray(30), gray(36), gray(22), gray(58), gray(70))
    } else {
        (gray(246), gray(252), gray(255), gray(225), gray(210))
    };
    v.panel_fill = panel;
    v.window_fill = window;
    v.extreme_bg_color = field;
    v.faint_bg_color = if dark { gray(34) } else { gray(240) };
    v.window_stroke = egui::Stroke::new(1.0, if dark { gray(50) } else { gray(215) });
    v.window_corner_radius = egui::CornerRadius::same(12);
    v.menu_corner_radius = egui::CornerRadius::same(10);
    let shadow = egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: egui::Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent;
    v.selection.stroke.color = gray(255);
    let w = &mut v.widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = egui::CornerRadius::same(7);
    }
    // Flat buttons that light up under the pointer, as in GNOME and KDE.
    w.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_fill = hover;
    w.hovered.bg_stroke = egui::Stroke::NONE;
    w.active.weak_bg_fill = active;
    w.active.bg_fill = active;
    w.active.bg_stroke = egui::Stroke::NONE;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, if dark { gray(48) } else { gray(220) });
}

/// The desktop's dark style and accent colour, from the XDG desktop portal; `None` where it says
/// nothing. Runs `gdbus`, so it belongs in a background thread.
pub fn desktop() -> (Option<bool>, Option<egui::Color32>) {
    let read = |key: &str| {
        crate::command("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
                "--method",
                "org.freedesktop.portal.Settings.ReadOne",
                "org.freedesktop.appearance",
                key,
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    (
        parse_scheme(&read("color-scheme")),
        parse_accent(&read("accent-color")),
    )
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
    #[test]
    fn reads_the_portal() {
        assert_eq!(super::parse_scheme("(<uint32 1>,)\n"), Some(true));
        assert_eq!(super::parse_scheme("(<uint32 0>,)\n"), None);
        assert_eq!(super::parse_scheme(""), None);
        let orange = super::parse_accent("(<(0.92941176891326904, 0.35686275362968445, 0.0)>,)\n");
        assert_eq!(orange, Some(eframe::egui::Color32::from_rgb(237, 91, 0)));
        // KDE and GNOME without an accent give a colour out of range.
        assert_eq!(super::parse_accent("(<(-1.0, -1.0, -1.0)>,)"), None);
    }
}
