//! The Help menu's pages: manual, keyboard shortcuts, keyboard map and About.

use crate::{Action, DIFFERENCES, MOUSE, SHORTCUTS};
use eframe::egui;
use egui::Key;

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Manual,
    Shortcuts,
    Keymap,
    About,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Manual => "Help Manual",
            Page::Shortcuts => "Keyboard Shortcuts",
            Page::Keymap => "Keyboard Map",
            Page::About => "About Glimt",
        }
    }
}

/// The open help page in a window; closing the window sets `page` to `None`.
pub fn window(ctx: &egui::Context, page: &mut Option<Page>) {
    let Some(shown) = *page else { return };
    let mut open = true;
    egui::Window::new(shown.title())
        .id(egui::Id::new("help"))
        .open(&mut open)
        .collapsible(false)
        .default_height(520.0)
        .show(ctx, |ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical().show(ui, |ui| match shown {
                Page::Manual => manual(ui),
                Page::Shortcuts => shortcuts(ui),
                Page::Keymap => keymap(ui),
                Page::About => about(ui),
            });
        });
    if !open {
        *page = None;
    }
}

const MANUAL: &[(&str, &str)] = &[
    (
        "Opening images",
        "Start Glimt with a folder or an image (glimt ~/Pictures), drop one on the window, use \
         File → Open, type a \
         path in the folder field above the folder list and press Enter, or click a folder in \
         the list. ⬆ .. goes to the parent folder. Network shares work too: type \
         smb://server/share (or sftp://…) and Glimt mounts it with gio, asking for user, \
         domain and password if the share needs them (a password saved in the keyring from \
         Files is used without asking).",
    ),
    (
        "Browsing",
        "Space, Page Down and → show the next image; Backspace, Page Up and ← the previous; \
         Home and End the first and last. The images on each side are decoded in advance, so \
         browsing is instant. Click a name in the file list to show it.",
    ),
    (
        "Zoom",
        "X fits the image to the window, Z shows it 1:1, + and - zoom. The mouse wheel zooms \
         (or browses, see Preferences) towards the pointer. A zoomed image larger than the \
         window can be dragged or moved with the arrow keys. \
         ] and [ rotate the view, Shift+R turns it upside down, Shift+M mirrors and Shift+F \
         flips it (Edit → Orientation); the next image is shown as it is.",
    ),
    (
        "Slideshow",
        "S starts and stops, P pauses, R switches random order on and off, Ctrl+= and Ctrl+- \
         change the delay. Browsing during a slideshow gives the new image the full delay.",
    ),
    (
        "Selecting and file operations",
        "Ctrl+click toggles a file, Shift+click or Shift with the arrow keys, Page Up/Down, \
         Home and End selects a range, Ctrl+A selects all (the matches while searching). \
         Copy, Move, Rename and Delete act on the selected files, otherwise on the shown \
         image; right-click a file for a menu with them. File → Open With (also in the \
         right-click menu) opens them in another application, such as GIMP. Ctrl+Shift+C \
         copies the image itself to the clipboard, to paste into other programs.",
    ),
    (
        "Finding and sorting",
        "Type three or more letters in the search field above the file list: the list shows \
         the matching names, best first, ignoring case and gaps (like fzf). Start with exif: \
         to search the EXIF instead, for example exif:canon 2024:05 for photos from a Canon \
         camera taken in May 2024; every word must match. The first EXIF search in a folder \
         reads all files once, which takes a while in large folders. With the marks feature, \
         1–6 toggle a mark, Ctrl+1–6 select the files with it and Alt+0–5 give stars; \
         mark:2 or stars:3 in the search field shows those files. The sort menu at \
         the bottom left sorts by name, number, date, EXIF date, size or extension.",
    ),
    (
        "Panels",
        "T shows thumbnails, Ctrl+I shows the file list as a grid of icons (↑ and ↓ move a row), Ctrl+T shows the folders as a tree, K shows a histogram (RGB, brightness, off) and J makes it logarithmic, E compares side by side: the next image, or the other of two selected files, at the same zoom and position, Ctrl+K the EXIF panel, Ctrl+H hides the file list, F or F11 \
         shows the image full screen, Esc leaves full screen. In full screen, I shows or \
         hides the name, place and EXIF over the image.",
    ),
    (
        "Preferences",
        "Edit → Preferences (Ctrl+O) has the settings: preloading, slideshow, thumbnails, \
         what is shown, window and memory use, and how deleting works.",
    ),
];

fn manual(ui: &mut egui::Ui) {
    for (heading, text) in MANUAL {
        ui.strong(*heading);
        ui.label(*text);
        ui.add_space(6.0);
    }
    egui::CollapsingHeader::new("Differences from Geeqie").show(ui, |ui| {
        egui::Grid::new("differences").striped(true).show(ui, |ui| {
            ui.strong("");
            ui.strong("Glimt");
            ui.strong("Geeqie");
            ui.end_row();
            for (what, glimt, geeqie) in DIFFERENCES {
                ui.label(*what);
                ui.label(*glimt);
                ui.label(*geeqie);
                ui.end_row();
            }
        });
    });
}

/// Every shortcut of an action on one row, in menu order, then the mouse.
fn shortcuts(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let mut rows: Vec<(Action, Vec<String>)> = Vec::new();
    for &(action, mods, key) in SHORTCUTS {
        if action == Action::Play && !cfg!(feature = "video") {
            continue;
        }
        let text = ctx.format_shortcut(&egui::KeyboardShortcut::new(mods, key));
        match rows.iter_mut().find(|r| r.0 == action) {
            Some(row) => row.1.push(text),
            None => rows.push((action, vec![text])),
        }
    }
    rows.sort_by_key(|r| r.0 as usize);
    egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
        for (action, keys) in rows {
            ui.label(keys.join(", "));
            ui.label(action.label());
            ui.end_row();
        }
        for (what, does) in MOUSE {
            ui.label(*what);
            ui.label(*does);
            ui.end_row();
        }
    });
}

/// A keyboard with the keys Glimt uses highlighted; hover a key to see what it does.
fn keymap(ui: &mut egui::Ui) {
    use Key::*;
    // (label, key, width in key units); `None` keys are drawn but never used.
    type Row = &'static [(&'static str, Option<Key>, f32)];
    const ROWS: [Row; 6] = [
        &[
            ("Esc", Some(Escape), 1.0),
            ("", None, 0.5),
            ("F1", Some(F1), 1.0),
            ("F2", Some(F2), 1.0),
            ("F3", Some(F3), 1.0),
            ("F4", Some(F4), 1.0),
            ("", None, 0.5),
            ("F5", Some(F5), 1.0),
            ("F6", Some(F6), 1.0),
            ("F7", Some(F7), 1.0),
            ("F8", Some(F8), 1.0),
            ("", None, 0.5),
            ("F9", Some(F9), 1.0),
            ("F10", Some(F10), 1.0),
            ("F11", Some(F11), 1.0),
            ("F12", Some(F12), 1.0),
        ],
        &[
            ("`", Some(Backtick), 1.0),
            ("1", Some(Num1), 1.0),
            ("2", Some(Num2), 1.0),
            ("3", Some(Num3), 1.0),
            ("4", Some(Num4), 1.0),
            ("5", Some(Num5), 1.0),
            ("6", Some(Num6), 1.0),
            ("7", Some(Num7), 1.0),
            ("8", Some(Num8), 1.0),
            ("9", Some(Num9), 1.0),
            ("0", Some(Num0), 1.0),
            ("-", Some(Minus), 1.0),
            ("=", Some(Equals), 1.0),
            ("Bksp", Some(Backspace), 2.0),
        ],
        &[
            ("Tab", Some(Tab), 1.5),
            ("Q", Some(Q), 1.0),
            ("W", Some(W), 1.0),
            ("E", Some(E), 1.0),
            ("R", Some(R), 1.0),
            ("T", Some(T), 1.0),
            ("Y", Some(Y), 1.0),
            ("U", Some(U), 1.0),
            ("I", Some(I), 1.0),
            ("O", Some(O), 1.0),
            ("P", Some(P), 1.0),
            ("[", Some(OpenBracket), 1.0),
            ("]", Some(CloseBracket), 1.0),
            ("\\", Some(Backslash), 1.5),
        ],
        &[
            ("Caps", None, 1.75),
            ("A", Some(A), 1.0),
            ("S", Some(S), 1.0),
            ("D", Some(D), 1.0),
            ("F", Some(F), 1.0),
            ("G", Some(G), 1.0),
            ("H", Some(H), 1.0),
            ("J", Some(J), 1.0),
            ("K", Some(K), 1.0),
            ("L", Some(L), 1.0),
            (";", Some(Semicolon), 1.0),
            ("'", Some(Quote), 1.0),
            ("Enter", Some(Enter), 2.25),
        ],
        &[
            ("Shift", None, 2.25),
            ("Z", Some(Z), 1.0),
            ("X", Some(X), 1.0),
            ("C", Some(C), 1.0),
            ("V", Some(V), 1.0),
            ("B", Some(B), 1.0),
            ("N", Some(N), 1.0),
            ("M", Some(M), 1.0),
            (",", Some(Comma), 1.0),
            (".", Some(Period), 1.0),
            ("/", Some(Slash), 1.0),
            ("Shift", None, 2.75),
        ],
        &[
            ("Ctrl", None, 1.5),
            ("Alt", None, 1.5),
            ("Space", Some(Space), 7.0),
            ("Alt", None, 1.5),
            ("Ctrl", None, 1.5),
        ],
    ];
    const NAV: [[(&str, Key); 3]; 2] = [
        [("Ins", Insert), ("Home", Home), ("PgUp", PageUp)],
        [("Del", Delete), ("End", End), ("PgDn", PageDown)],
    ];
    const ARROWS: [(&str, Key, f32, f32); 4] = [
        ("⬆", ArrowUp, 1.0, 0.0),
        ("⬅", ArrowLeft, 0.0, 1.0),
        ("⬇", ArrowDown, 1.0, 1.0),
        ("➡", ArrowRight, 2.0, 1.0),
    ];

    ui.label("Highlighted keys do something; bright ones also without Ctrl, Shift or Alt.");
    ui.label("Hover a key to see its actions.");
    ui.add_space(8.0);
    let unit = 34.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(unit * 19.5, unit * 6.6), egui::Sense::hover());
    let draw = |ui: &mut egui::Ui, label: &str, key: Option<Key>, at: egui::Rect| {
        let actions: Vec<String> = key
            .map(|k| {
                SHORTCUTS
                    .iter()
                    .filter(|s| s.2 == k && (s.0 != Action::Play || cfg!(feature = "video")))
                    .map(|&(a, m, k)| {
                        let keys = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(m, k));
                        format!("{keys}: {}", a.label())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let plain = key.is_some_and(|k| {
            SHORTCUTS
                .iter()
                .any(|s| s.2 == k && s.1 == egui::Modifiers::NONE)
        });
        let v = ui.visuals();
        let fill = if plain {
            v.selection.bg_fill
        } else if !actions.is_empty() {
            v.selection.bg_fill.gamma_multiply(0.45)
        } else {
            v.widgets.inactive.weak_bg_fill
        };
        let r = at.shrink(2.0);
        ui.painter().rect_filled(r, 4.0, fill);
        let color = if actions.is_empty() {
            v.weak_text_color()
        } else {
            v.strong_text_color()
        };
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
        if !actions.is_empty() {
            let id = ui
                .id()
                .with(("key", label, at.min.x as i32, at.min.y as i32));
            ui.interact(r, id, egui::Sense::hover())
                .on_hover_text(actions.join("\n"));
        }
    };
    for (y, row) in ROWS.iter().enumerate() {
        let mut x = 0.0;
        // A gap under the function keys, as on a keyboard.
        let top = rect.min.y + y as f32 * unit + if y > 0 { unit * 0.3 } else { 0.0 };
        for &(label, key, width) in *row {
            let at = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + x * unit, top),
                egui::vec2(width * unit, unit),
            );
            if !label.is_empty() {
                draw(ui, label, key, at);
            }
            x += width;
        }
    }
    let left = rect.min.x + 15.5 * unit;
    for (y, row) in NAV.iter().enumerate() {
        for (x, &(label, key)) in row.iter().enumerate() {
            let min = egui::pos2(left + x as f32 * unit, rect.min.y + (1.3 + y as f32) * unit);
            draw(
                ui,
                label,
                Some(key),
                egui::Rect::from_min_size(min, egui::vec2(unit, unit)),
            );
        }
    }
    for (label, key, x, y) in ARROWS {
        let min = egui::pos2(left + x * unit, rect.min.y + (4.3 + y) * unit);
        draw(
            ui,
            label,
            Some(key),
            egui::Rect::from_min_size(min, egui::vec2(unit, unit)),
        );
    }
}

/// Libraries and assets Glimt is built with, and their licenses.
const CREDITS: &[(&str, &str, &str)] = &[
    (
        "egui / eframe",
        "Emil Ernerfeldt and contributors",
        "MIT or Apache-2.0",
    ),
    (
        "winit, glutin",
        "The winit and glutin contributors",
        "Apache-2.0",
    ),
    ("sctk-adwaita", "Window frame on Wayland", "MIT"),
    (
        "image, png, zune-jpeg",
        "The image-rs and zune contributors",
        "MIT or Apache-2.0",
    ),
    ("fast_image_resize", "Kirill Kuzminykh", "MIT or Apache-2.0"),
    ("Ubuntu font", "Dalton Maag", "Ubuntu Font Licence 1.0"),
    ("Noto Emoji", "Google", "SIL Open Font License 1.1"),
    ("emoji-icon-font, Hack", "Font authors", "MIT"),
    (
        "ffmpeg (optional, run as a program)",
        "The FFmpeg developers",
        "LGPL-2.1 or GPL",
    ),
];

fn about(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.heading(format!("Glimt {}", env!("CARGO_PKG_VERSION")));
        ui.label("A fast, memory-frugal image viewer for Linux.");
        ui.label("Glimt is Norwegian for \"glimpse\": a quick look.");
        ui.weak("Copyright © 2026 Hans-Henry Jakobsen");
    });
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Credits")
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Inspired by Geeqie (geeqie.org); Glimt shares no code with it.");
            ui.label("Written with Claude Code (Anthropic).");
            ui.add_space(4.0);
            egui::Grid::new("credits").striped(true).show(ui, |ui| {
                for (what, who, license) in CREDITS {
                    ui.label(*what);
                    ui.weak(*who);
                    ui.label(*license);
                    ui.end_row();
                }
            });
        });
    egui::CollapsingHeader::new("License (MIT)").show(ui, |ui| {
        ui.label(include_str!("../LICENSE"));
    });
}
