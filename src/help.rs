//! The Help menu's pages: manual, keyboard shortcuts, keyboard map and About.

use crate::{Action, DIFFERENCES, MOUSE, shortcuts as all_shortcuts};
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
         Files is used without asking). glimt --remote next (or prev, slideshow, open PATH …) controls a running Glimt \
         from a script; glimt --help lists the commands.",
    ),
    (
        "Browsing",
        "Space, Page Down and → show the next image; Backspace, Page Up and ← the previous; \
         Home and End the first and last. The images on each side are decoded in advance, so \
         browsing is instant. Click a name in the file list to show it. In a TIFF with several \
         pages, Ctrl+Page Down and Ctrl+Page Up go to the next and previous page.",
    ),
    (
        "Zoom",
        "X fits the image to the window, Z shows it 1:1, + and - zoom. The mouse wheel zooms \
         (or browses, see Preferences) towards the pointer. A zoomed image larger than the \
         window can be dragged or moved with the arrow keys. \
         ] and [ rotate the view, Shift+R turns it upside down, Shift+M mirrors and Shift+F \
         flips it (Edit → Orientation); the next image is shown as it is. Edit → Orientation \
         → Save Orientation to File stores the rotation in a JPEG as its EXIF orientation, \
         without touching the pixels; a setting does it on every change of image. PNG and \
         WebP are turned and saved again with their EXIF data: PNG and lossless WebP without \
         loss, a lossy WebP as lossy again (through ffmpeg, at the quality set in Preferences).",
    ),
    (
        "Slideshow",
        "S starts and stops, P pauses, R switches random order on and off, Ctrl+= and Ctrl+- \
         change the delay. Browsing during a slideshow gives the new image the full delay. \
         With \"Include the images in subfolders\" in Preferences it shows those too; the list \
         goes back to the folder when the slideshow stops.",
    ),
    (
        "Selecting and file operations",
        "Ctrl+click toggles a file, Shift+click or Shift with the arrow keys, Page Up/Down, \
         Home and End selects a range, Ctrl+A selects all (the matches while searching). \
         Copy, Move, Rename and Delete act on the selected files, otherwise on the shown \
         image; right-click a file for a menu with them. Rename with several files selected \
         asks for a pattern: * is the old name and # a number (### gives 001); existing files \
         are never overwritten. File → Open With (also in the \
         right-click menu) opens them in another application, such as GIMP. Ctrl+Shift+C \
         copies the image itself to the clipboard, to paste into other programs; Select → \
         Copy Files to Clipboard copies the files, to paste in a file manager.",
    ),
    (
        "Finding and sorting",
        "Type three or more letters in the search field above the file list: the list shows the \
         matching names, best first, ignoring case and gaps (like fzf). Start with exif: to \
         search the EXIF, keywords and comments instead, for example exif:canon 2024:05 for \
         photos from a Canon camera taken in May 2024; every word must match. Start with re: for \
         a regular expression instead, ignoring case: re:^img_\\d+\\.jpe?g$, or \
         exif:re:canon.*2024:0[5-7] for the EXIF. A setting in Preferences makes the field a \
         regular expression always, from the first letter and in list order, as Geeqie's file \
         filter. The first EXIF search in a folder reads all files once, which takes a while in \
         large folders. With the marks feature, 1–6 toggle a mark, Ctrl+1–6 select the files with \
         it and Alt+0–5 give stars; mark:2 or stars:3 in the search field shows those files; a \
         setting also writes the stars to XMP sidecars for darktable and digiKam. The sort menu \
         at the bottom left sorts by name, number, date, EXIF date, size or extension.",
    ),
    (
        "Keywords and comments",
        "The EXIF panel (Ctrl+K) shows the keywords and comment of the image below its EXIF; \
         Alt+K puts the cursor in the keywords, separated by commas. They are saved when you \
         leave the field, to the sidecar photo.jpg.xmp that darktable and digiKam also read \
         (as dc:subject and dc:description); keywords written into the image by other \
         programs are shown too. With several files selected, a button adds the keywords to \
         all of them. exif: searches find keywords and comments. Renaming, moving, copying \
         and deleting in Glimt take the sidecar along.",
    ),
    (
        "Printing",
        "File → Print (Ctrl+P) prints the selected images, or the shown one, one per page, as \
         large as fits and turned to landscape for wide images, with the file name below if \
         you like. It goes to a printer through CUPS (lp), or into a PDF file. The printer, \
         paper and file names are remembered.",
    ),
    (
        "Plugins",
        "Edit → Plugins runs a program on the selected files, or the shown one, as Geeqie's \
         plugins do, and shows them again afterwards. A plugin is a desktop file in \
         ~/.local/share/glimt/plugins (the menu opens that folder), for example Name=Rotate \
         and Exec=mogrify -rotate 90 %f: %f runs it once for each file, %F once for all, and \
         without either it runs once in the folder. A program next to the desktop file is \
         found there. New plugins show after a restart.",
    ),
    (
        "Sort manager",
        "Shift+S shows the sort manager on the right, as in Geeqie: a list of folders, added \
         with Add This Folder or Add…. A click on a folder copies the selected files, or the \
         shown one, there, or moves them with Move chosen; × takes the folder off the list. \
         Handy for sorting a card's photos into albums.",
    ),
    (
        "Pan view",
        "Ctrl+J (View → Pan View) shows the images of the list as a timeline, as Geeqie's pan \
         view does: thumbnails under a heading for each day, oldest first, by the file date or, \
         with EXIF date ticked, the date the photo was taken. A click shows the image, a double \
         click also closes the view. With Include Subfolders it covers the whole tree.",
    ),
    (
        "Collections",
        "A collection is a list of images from any folders, as in Geeqie. C makes a new one of \
         the selected files (or the shown image), File → Add to Collection (also in the \
         right-click menu) adds to one, and File → Collections or O opens one: the file list \
         then shows its images, which browse, sort and play as a slideshow like a folder. \
         Delete takes the shown or selected images out of the collection, leaving the files; \
         renaming and moving in Glimt keep them in it. Each change is saved at once, as a \
         Geeqie .gqv file in ~/.local/share/glimt/collections; Geeqie's own collections open \
         too (O, or a .gqv file on the command line).",
    ),
    (
        "Finding duplicates",
        "D (File → Find Duplicates) opens a window that compares the images in the list: by \
         checksum (identical files; only files of the same size are read), name, size, \
         dimensions, or how alike they look (95, 90 or 85%, from the thumbnails). Turn on \
         Include Subfolders first to search a whole tree. The groups found are listed; click \
         a name to show it, check the ones to remove (or check all but the first of each \
         group) and move them to the trash.",
    ),
    (
        "Themes",
        "View → Theme, or Theme at the top of Edit → Preferences, changes the look: Dark (the \
         default), Light, Desktop (light or dark and the accent colour as the desktop), Nord, and \
         for low vision High Contrast (white on black) and Contrast (black on white), both with \
         larger text. Themes are small text files: Save as Theme… writes the look shown as one to \
         edit, Open Themes Folder shows where they go (~/.config/glimt/themes), and Get More \
         Themes… opens the collection on GitHub. Without an accent colour of its own, a theme \
         takes the desktop's.",
    ),
    (
        "Layouts",
        "View → Layout shows several images at once: two side by side, two above each other, or \
         four (2 × 2). The first pane is the shown image, with zoom; the others show the images \
         that follow it in the list, so with random order (R) they are random too. For each \
         layout, the slideshow either changes all images together or one at a time, round the \
         panes; choose in the same menu, as well as Show File Names, in every pane or none. \
         With full screen (F) the panes fill the screen, as two or four borderless pictures.",
    ),
    (
        "Panels",
        "T shows thumbnails, Ctrl+I shows the file list as a grid of icons (↑ and ↓ move a row), Ctrl+T shows the folders as a tree, View → Include Subfolders lists the images of every folder below, K shows a histogram (RGB, brightness, off) and J makes it logarithmic, A stops and starts animations, E compares side by side: the next image, or the other of two selected files, at the same zoom and position, Ctrl+K the EXIF panel (its GPS position opens a map), Ctrl+E a window with every EXIF field, Ctrl+H hides the file list, F or F11 \
         shows the image full screen, Esc leaves full screen. In full screen, I shows or \
         hides the name, place and EXIF over the image.",
    ),
    (
        "Colour",
        "Photos with an ICC profile (Adobe RGB, Display P3 …) are shown in their own colours: \
         converted to sRGB, or, for a calibrated or wide-gamut screen, straight to the screen \
         profile chosen under View → Colour Management (also in Preferences). The desktop's \
         screen profile, which GNOME and KDE keep in colord, goes before the chosen one when \
         there is one; Use the desktop's screen profile off leaves it out. A photo without \
         one whose EXIF says Adobe RGB, as from a camera set to it, is taken as Adobe RGB; other \
         images without one as sRGB, or the profile chosen there for them (Adobe RGB, Display \
         P3 or a file). Use the image's own profile off takes that one for every image. The \
         EXIF panel (Ctrl+K) says which profile an image is shown from. The rendering intent \
         there decides how colours the target cannot show are brought in; only profiles with \
         tables for each intent (LUT profiles) tell them apart. With a screen profile \
         every image is converted, a few milliseconds each in the background; files Glimt \
         writes and the thumbnails it shares with other programs stay sRGB.",
    ),
    (
        "Preferences",
        "Edit → Preferences (Ctrl+O) has the settings: preloading, slideshow, thumbnails, \
         the theme, what is shown, window and memory use, and how deleting works. Under Keyboard shortcuts, \
         Change gives an action the next key you press; Reset gives it Geeqie's keys back.",
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
    for (action, mods, key) in all_shortcuts() {
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
                all_shortcuts()
                    .into_iter()
                    .filter(|s| s.2 == k && (s.0 != Action::Play || cfg!(feature = "video")))
                    .map(|(a, m, k)| {
                        let keys = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(m, k));
                        format!("{keys}: {}", a.label())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let plain = key.is_some_and(|k| {
            all_shortcuts()
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
    (
        "Inter",
        "The Inter Project Authors",
        "SIL Open Font License 1.1",
    ),
    ("Phosphor Icons", "Phosphor Icons", "MIT"),
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
