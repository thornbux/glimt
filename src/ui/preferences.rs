//! Edit → Preferences: one window of every setting, in sections, saved when one changes (when
//! a slider is let go). The settings themselves are `Config`'s (config.rs).

use crate::files::config::{
    CPU_LEVELS, FADES, FULL_SCREEN_INFO_TEMPLATE, MAX_SLIDESHOW_DELAY, MEMORY_LEVELS,
};
use crate::images::colour::{INPUT_PROFILES, INTENTS};
use crate::support::format::thousands;
use crate::support::lang::tr;
use crate::support::threads::{Working, set_cpu_use};
use crate::ui::actions::{ALL, Action, SHORTCUTS, shortcuts};
use crate::ui::dialogs::Pick;
use crate::ui::menus::{checkbox, radio, radio_choice};
use crate::{files::config, images::thumbs, support::lang, tf, ui::actions, ui::theme};
use eframe::egui;
use std::path::Path;

// The texts of each section that the search field looks in, its title first. The test
// `the_search_knows_every_text` keeps them the same as the texts the section shows.
const LOOK: &[&str] = &[
    "Look",
    "Automatic (system language)",
    "Background behind the image",
    "Black",
    "Checkerboard behind transparent images",
    "Get More Themes…",
    "High Contrast when the desktop asks for high contrast",
    "Language",
    "Large",
    "Larger still when the desktop's text is (GNOME's Large Text)",
    "Medium",
    "Open Themes Folder",
    "Save as Theme…",
    "Show the file name search",
    "Show the folder path",
    "Show the toolbar",
    "Small",
    "Theme",
    "Toolbar icons:",
    "size of the menus, lists and text",
];

const KEYBOARD: &[&str] = &[
    "Keyboard",
    "Change",
    "Keyboard shortcuts",
    "Press a key (Esc cancels)",
    "Reset",
];

const IMAGE: &[&str] = &[
    "Image",
    "Auto rotate images using EXIF (Alt+X)",
    "Brightness",
    "Default",
    "Fields: %name%, %path%, %position%, %dimensions%, %marks%, %keywords%, %comment%, and each label of the info panel, such as %Camera% or %ISO%. A | starts a part, left out when its fields are empty.",
    "Enlarge small images to fit the window",
    "Hide the pointer in full screen when the mouse is still",
    "Histogram (K):",
    "Image info template:",
    "Keep zoom and position when changing image",
    "Logarithmic (J)",
    "Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)",
    "Off",
    "Play animated GIF and WebP (A)",
    "Save rotation to the file when changing image (JPEG; PNG and WebP are saved again)",
    "Show image info in full screen (I)",
    "quality of a cropped JPEG (Edit → Crop)",
    "quality when a lossy WebP is saved again",
];

const SLIDESHOW: &[&str] = &[
    "Slideshow",
    "Fade to the next image:",
    "Include the images in subfolders",
    "Not with CPU use Power saving",
    "Random order",
    "Repeat: start again after the last image",
    "S starts and stops, R random order, Ctrl+= faster, Ctrl+- slower",
    "seconds per image",
];

const FILE_LIST: &[&str] = &[
    "File list",
    "Also write stars to XMP sidecars (photo.jpg.xmp), for darktable and digiKam",
    "Copies of the pictures in your home folder: those of an encrypted volume or a stick stay readable there",
    "Fuzzy, best matches first (like fzf)",
    "In ~/.cache/thumbnails; kept for disks that are not plugged in",
    "Include the images in subfolders in the file list",
    "Large",
    "Medium",
    "Refresh the folder when files change",
    "Regular expression, in list order (like Geeqie's file filter)",
    "Remove thumbnails of deleted files",
    "Save new thumbnails to ~/.cache/thumbnails (shared with other programs)",
    "Search field above the file list:",
    "Show file size and date in the file list",
    "Show folders as a tree (Ctrl+T)",
    "Show marks and stars in the file list",
    "Show the info panel (Ctrl+K; reads a little from each image shown)",
    "Show the file list as icons (Ctrl+I)",
    "Show File Names Under Thumbnails",
    "Show thumbnails in the file list (T)",
    "Small",
    "Thumbnail size:",
];

const DELETING: &[&str] = &[
    "Deleting",
    "Confirm move file to Trash",
    "Confirm permanent file delete",
    "Enable Delete key",
    "Use no trash at all (Delete deletes permanently)",
];

const COLOUR: &[&str] = &[
    "Colour management",
    "Choose…",
    "Colour management",
    "From colord, which GNOME and KDE set",
    "Images without a profile:",
    "None",
    "Profile file…",
    "Rendering intent:",
    "Screen profile:",
    "Use the desktop's screen profile",
    "Use the image's own profile",
    "none, sRGB",
    "{name}, from the desktop",
];

const MEMORY: &[&str] = &[
    "Memory and CPU",
    "CPU use:",
    "Custom",
    "Decoding threads and one malloc arena take effect after a restart.",
    "Details",
    "MB at most for an animation's frames",
    "Memory use:",
    "One malloc arena: much less memory, decoding up to ~8 % slower",
    "Preload next and previous images:",
    "Up to ≈ {mb} MB, each image at most {w}×{h}",
    "images decoded at once",
    "images on each side",
];

const VIDEO: &[&str] = &[
    "Video",
    "A command line, such as mpv --fs; a path with spaces goes in \"quotes\"",
    "Player (Enter):",
    "Previews need ffmpeg",
    "Show videos in the file list",
    "default application",
];

const START: &[&str] = &[
    "Start and system",
    "Custom window title (empty: \"image name - Glimt\"):",
    "Fields: %name%, %path%, %folder%, %app% and %id%. A | starts a part, left out when its fields are empty.",
    "Keep a list of recent folders (File → Open Recent)",
    "Keep the screen saver off in full screen",
    "Listen for glimt --remote commands (after restart)",
    "Remember the window size and position",
    "Start in the last used folder",
];

/// The Preferences window: open or not, its search, and the action whose key is being chosen.
#[derive(Default)]
pub(crate) struct Preferences {
    /// Edit → Preferences is open.
    open: bool,
    /// Preferences changed while the pointer is down: saved when it is let go.
    has_unsaved: bool,
    /// The search field of Preferences: only the sections with these words show.
    query: String,
    /// Preferences was just opened: its search field takes the keys.
    search_takes_focus: bool,
    /// The action whose key is being chosen in Preferences: the next key press goes to it.
    key_being_chosen_for: Option<Action>,
}

impl Preferences {
    /// Opens Preferences with an empty search, because a search from last time would hide
    /// settings without saying why. The search field takes the keys.
    pub(crate) fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.search_takes_focus = true;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

impl crate::App {
    /// Edit → Preferences: every setting, saved when one changes; Esc closes it.
    pub(crate) fn preferences_window(&mut self, ctx: &egui::Context) {
        let mut open = self.preferences.open;
        // Esc closes Preferences (shortcuts are off while it is open), and a search in its
        // field is cleared first. It does not close Preferences when a dialog on top of it, a
        // popup (a combo box, the colour picker) or another text field takes the Esc, which
        // there only closes the popup or leaves the field.
        let search_id = egui::Id::unique("preferences search");
        let search_had_focus =
            ctx.memory(|m| m.has_focus(search_id) || m.had_focus_last_frame(search_id));
        if open
            && !self.modal_open()
            && self.preferences.key_being_chosen_for.is_none()
            && !egui::Popup::is_any_open(ctx)
            && (search_had_focus || !self.focus_held(ctx))
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            open = !self.preferences.query.trim().is_empty();
            self.preferences.query.clear();
        }
        let mut keys_drawn = false;
        egui::Window::new(tr("Preferences"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                // Scrolls when the window is smaller than the settings; a solid scroll bar
                // takes its own space instead of covering the text.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                // Above the settings, so it stays when they scroll.
                let search = egui::TextEdit::singleline(&mut self.preferences.query)
                    .id(search_id)
                    .hint_text(tr("Search the settings"))
                    .desired_width(f32::INFINITY);
                let search = ui.add(search);
                // Until it has it: the window's first frame only measures it.
                if self.preferences.search_takes_focus {
                    search.request_focus();
                    self.preferences.search_takes_focus = !search.has_focus();
                }
                let query = self.preferences.query.to_lowercase();
                let shown = sections_shown(&query);
                if !shown.contains(&true) {
                    ui.weak(tr("No setting has these words"));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Plain settings set `changed`; the config is saved once at the end.
                    let mut changed = false;
                    // The look first, where it is found (also under View → Theme).
                    if shown[0] {
                        changed |= self.look_settings(ui, ctx);
                    }
                    if shown[1] {
                        section(ui, theme::icon::KEYBOARD, tr(KEYBOARD[0]));
                        // Open on a search, with the actions it finds.
                        let keys = egui::CollapsingHeader::new(tr("Keyboard shortcuts"))
                            .open((!query.is_empty()).then_some(true))
                            .show(ui, |ui| {
                                self.key_settings(ui, &query);
                            });
                        crate::support::test_hooks::noted(
                            &keys.header_response,
                            tr("Keyboard shortcuts"),
                        );
                        keys_drawn = keys.body_returned.is_some();
                    }
                    if shown[2] {
                        section(ui, theme::icon::IMAGE, tr(IMAGE[0]));
                        changed |= self.image_settings(ui);
                        changed |= self.histogram_settings(ui);
                        changed |= self.info_settings(ui);
                    }
                    if shown[3] {
                        section(ui, theme::icon::PLAY, tr(SLIDESHOW[0]));
                        changed |= self.slideshow_settings(ui, ctx);
                    }
                    if shown[4] {
                        section(ui, theme::icon::LIST, tr(FILE_LIST[0]));
                        changed |= self.list_settings(ui, ctx);
                        self.info_panel_setting(ui);
                        changed |= self.search_settings(ui, ctx);
                        changed |= self.refresh_setting(ui);
                        changed |= self.cache_settings(ui, ctx);
                    }
                    if shown[5] {
                        section(ui, theme::icon::TRASH, tr(DELETING[0]));
                        changed |= self.delete_settings(ui);
                    }
                    if shown[6] {
                        section(ui, theme::icon::COLOUR, tr(COLOUR[0]));
                        self.colour_settings(ui);
                    }
                    if shown[7] {
                        section(ui, theme::icon::CPU, tr(MEMORY[0]));
                        changed |= self.memory_settings(ui);
                        self.cpu_settings(ui);
                    }
                    #[cfg(feature = "video")]
                    if shown[8] {
                        section(ui, theme::icon::FILM, tr(VIDEO[0]));
                        changed |= self.video_settings(ui, ctx);
                    }
                    if shown[9] {
                        section(ui, theme::icon::DESKTOP, tr(START[0]));
                        changed |= self.window_settings(ui, ctx);
                    }
                    // Saved once a slider is let go, not in every frame it is dragged.
                    self.preferences.has_unsaved |= changed;
                    if self.preferences.has_unsaved && !ctx.input(|i| i.pointer.any_down()) {
                        self.preferences.has_unsaved = false;
                        self.config.save();
                    }
                });
            });
        self.preferences.open = open;
        // A key is chosen only while its row is drawn. Closing the window or the header ends
        // the choosing, so a key pressed later in another field is not taken for the action.
        if !keys_drawn {
            self.preferences.key_being_chosen_for = None;
        }
    }

    /// Preferences: the look, first where it is found (also under View → Theme): the theme,
    /// the language, contrast, the size of the interface, the background and the bars.
    fn look_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        section(ui, theme::icon::PALETTE, tr(LOOK[0]));
        let names = self.desktop_settings.theme_names(ctx);
        ui.horizontal(|ui| {
            let mut chosen = false;
            egui::ComboBox::from_id_salt("theme")
                .selected_text(theme::shown_name(&self.config.theme))
                .show_ui(ui, |ui| {
                    for name in names {
                        let c = &mut self.config.theme;
                        let shown = theme::shown_name(&name);
                        chosen |= crate::ui::menus::choice(ui, c, name.clone(), shown).clicked();
                    }
                });
            ui.label(tr("Theme"));
            // Chosen again too, so a theme file just edited shows.
            if chosen {
                changed = true;
                self.apply_theme(ui.ctx());
            }
        });
        ui.horizontal(|ui| self.theme_buttons(ui));
        // Each language by its own name, so one can find the way back from a
        // language one does not read.
        ui.horizontal(|ui| {
            let name = |code: &str| -> String {
                match code {
                    "" => tr("Automatic (system language)").to_owned(),
                    lang::ENGLISH => "English".to_owned(),
                    code => (lang::LANGUAGES.iter().find(|l| l.0 == code))
                        .map_or(code, |l| l.1)
                        .to_owned(),
                }
            };
            let mut chosen = false;
            let codes = ["", lang::ENGLISH]
                .into_iter()
                .chain(lang::LANGUAGES.iter().map(|l| l.0));
            egui::ComboBox::from_id_salt("language")
                .selected_text(name(&self.config.language))
                .show_ui(ui, |ui| {
                    for code in codes {
                        let c = &mut self.config.language;
                        chosen |=
                            crate::ui::menus::choice(ui, c, code.to_owned(), &name(code)).clicked();
                    }
                });
            ui.label(tr("Language"));
            if chosen {
                changed = true;
                lang::set_language(&self.config.language);
            }
        });
        let follow = tr("High Contrast when the desktop asks for high contrast");
        if checkbox(ui, &mut self.config.follow_contrast, follow).changed() {
            changed = true;
            self.apply_theme(ui.ctx());
        }
        ui.horizontal(|ui| {
            let size = egui::Slider::new(&mut self.config.ui_scale, 50..=300)
                .suffix(" %")
                .text(tr("size of the menus, lists and text"));
            // Applied when let go: a size changing under the pointer would move it.
            let size = ui.add(size);
            changed |= size.changed();
            if size.drag_stopped() || (size.changed() && !size.dragged()) {
                self.apply_theme(ui.ctx());
            }
        });
        let follow = tr("Larger still when the desktop's text is (GNOME's Large Text)");
        if checkbox(ui, &mut self.config.follow_text_scale, follow).changed() {
            changed = true;
            self.apply_theme(ui.ctx());
        }
        changed |= self.background_setting(ui);
        changed |= self.bar_settings(ui, ctx);
        changed
    }

    /// Preferences: the slideshow's delay, order, subfolders, repeat and fade.
    pub(crate) fn slideshow_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        ui.weak(tr(
            "S starts and stops, R random order, Ctrl+= faster, Ctrl+- slower",
        ));
        let delay = egui::Slider::new(&mut self.config.slideshow_delay, 1..=MAX_SLIDESHOW_DELAY)
            .text(tr("seconds per image"));
        if ui.add(delay).changed() {
            changed = true;
            self.update_window_title(ctx);
        }
        let mut random = self.config.slideshow_random;
        if checkbox(ui, &mut random, tr("Random order")).changed() {
            self.set_random_order(ctx, random);
        }
        changed |= checkbox(
            ui,
            &mut self.config.slideshow_subfolders,
            tr("Include the images in subfolders"),
        )
        .changed();
        changed |= checkbox(
            ui,
            &mut self.config.slideshow_repeat,
            tr("Repeat: start again after the last image"),
        )
        .changed();
        ui.horizontal(|ui| {
            ui.label(tr("Fade to the next image:"));
            for (k, (_, label, _)) in FADES.iter().enumerate() {
                changed |= radio_choice(ui, &mut self.config.slideshow_fade, k, tr(label))
                    .on_hover_text(tr("Not with CPU use Power saving"))
                    .changed();
            }
        });
        changed
    }

    /// Preferences: rotation by EXIF and saving it, the background, enlarging, zoom, the wheel and
    /// the pointer in full screen.
    pub(crate) fn image_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let mut rotate = self.config.exif_rotate;
        if checkbox(ui, &mut rotate, tr("Auto rotate images using EXIF (Alt+X)")).changed() {
            self.set_exif_rotate(rotate);
        }
        changed |= checkbox(ui,
                &mut self.config.save_orientation,
                tr("Save rotation to the file when changing image (JPEG; PNG and WebP are saved again)"),
            )
            .changed();
        let quality = egui::Slider::new(&mut self.config.webp_quality, 50..=100)
            .text(tr("quality when a lossy WebP is saved again"));
        changed |= ui.add(quality).changed();
        let quality = egui::Slider::new(&mut self.config.jpeg_quality, 50..=100)
            .text(tr("quality of a cropped JPEG (Edit → Crop)"));
        changed |= ui.add(quality).changed();
        let c = &mut self.config;
        changed |= checkbox(
            ui,
            &mut c.enlarge,
            tr("Enlarge small images to fit the window"),
        )
        .changed();
        changed |= checkbox(
            ui,
            &mut c.keep_zoom,
            tr("Keep zoom and position when changing image"),
        )
        .changed();
        changed |= checkbox(
            ui,
            &mut c.wheel_zoom,
            tr("Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)"),
        )
        .changed();
        changed |= checkbox(
            ui,
            &mut c.hide_pointer,
            tr("Hide the pointer in full screen when the mouse is still"),
        )
        .changed();
        changed
    }

    /// Preferences → Look: the colour behind the image.
    fn background_setting(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui
                .color_edit_button_srgb(&mut self.config.background.0)
                .changed();
            ui.label(tr("Background behind the image"));
            if self.config.background.0 != [0, 0, 0]
                && crate::ui::menus::small_button(ui, tr("Black")).clicked()
            {
                self.config.background.0 = [0, 0, 0];
                changed = true;
            }
        });
        changed |= checkbox(
            ui,
            &mut self.config.checkerboard,
            tr("Checkerboard behind transparent images"),
        )
        .changed();
        changed
    }

    /// Preferences → Image: the histogram and animations.
    fn histogram_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(tr("Histogram (K):"));
            for (n, name) in [(0, tr("Off")), (1, "RGB"), (2, tr("Brightness"))] {
                changed |= radio_choice(ui, &mut self.config.histogram, n, name).changed();
            }
            let log = self.config.histogram_log;
            self.toggle_box(ui, log, Action::HistogramLog, tr("Logarithmic (J)"));
        });
        let c = &mut self.config;
        changed |= checkbox(ui, &mut c.animate, tr("Play animated GIF and WebP (A)")).changed();
        changed
    }

    /// Preferences: fuzzy or regular expression search.
    pub(crate) fn search_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        ui.label(tr("Search field above the file list:"));
        let fuzzy = self.config.fuzzy_search;
        for (on, label) in [
            (true, tr("Fuzzy, best matches first (like fzf)")),
            (
                false,
                tr("Regular expression, in list order (like Geeqie's file filter)"),
            ),
        ] {
            if radio(ui, fuzzy == on, label).clicked() && fuzzy != on {
                self.config.fuzzy_search = on;
                changed = true;
                self.update_search(ctx);
            }
        }
        changed
    }

    /// Preferences: thumbnails, icons, the folder tree, subfolders, marks, the histogram and
    /// animations.
    pub(crate) fn list_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut show = self.config.thumbnails;
        if checkbox(ui, &mut show, tr("Show thumbnails in the file list (T)")).changed() {
            self.set_thumbnails(show);
        }
        ui.horizontal(|ui| {
            ui.label(tr("Thumbnail size:"));
            self.thumb_size_setting(ui);
        });
        let icons = tr("Show the file list as icons (Ctrl+I)");
        self.toggle_box(ui, self.config.icon_grid, Action::Icons, icons);
        let mut names = self.config.grid_names;
        if checkbox(ui, &mut names, tr("Show File Names Under Thumbnails")).changed() {
            self.set_grid_names(names);
        }
        let tree = tr("Show folders as a tree (Ctrl+T)");
        self.toggle_box(ui, self.config.folder_tree, Action::FolderTree, tree);
        let mut sub = self.config.show_subfolders;
        if checkbox(
            ui,
            &mut sub,
            tr("Include the images in subfolders in the file list"),
        )
        .changed()
        {
            self.set_subfolders(ctx, sub);
        }
        // The settings above save themselves through their setters, so only the marks' are
        // left to the caller. A `cfg!` keeps `changed` changed in a build without marks.
        let mut changed = false;
        if cfg!(feature = "marks") {
            let c = &mut self.config;
            changed |= checkbox(
                ui,
                &mut c.show_marks,
                tr("Show marks and stars in the file list"),
            )
            .changed();
            changed |= checkbox(
                ui,
                &mut c.xmp_stars,
                tr("Also write stars to XMP sidecars (photo.jpg.xmp), for darktable and digiKam"),
            )
            .changed();
        }
        changed
    }

    /// Draws a checkbox of a setting that a View action turns on and off, and changes it
    /// through that action's `toggle_setting`, so it does what the menu does besides.
    fn toggle_box(&mut self, ui: &mut egui::Ui, on: bool, action: Action, text: &str) {
        let mut on = on;
        if checkbox(ui, &mut on, text).changed() {
            self.toggle_setting(action);
        }
    }

    /// Preferences → Memory use: Low, Medium or High set the settings below it at once; other
    /// values are Custom.
    pub(crate) fn memory_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let c = &self.config;
        let now = (
            c.preload,
            c.decode_threads,
            c.animation_mb,
            c.one_malloc_arena,
        );
        let level = (MEMORY_LEVELS.iter())
            .find(|l| (l.preload, l.decode_threads, l.animation_mb, l.one_arena) == now);
        let (mut chosen, mut custom) = (None, false);
        ui.horizontal(|ui| {
            ui.label(tr("Memory use:"));
            for l in &MEMORY_LEVELS {
                let on = level.is_some_and(|m| m.name == l.name);
                if radio(ui, on, tr(l.name))
                    .on_hover_text(tr(l.tooltip))
                    .clicked()
                {
                    chosen = Some(*l);
                }
            }
            // Chosen by changing the details, which it opens.
            custom = radio(ui, level.is_none(), tr("Custom")).clicked();
        });
        if let Some(l) = chosen {
            self.set_preload(l.preload);
            (self.config.decode_threads, self.config.animation_mb) =
                (l.decode_threads, l.animation_mb);
            self.config.one_malloc_arena = l.one_arena;
            crate::images::decode::set_animation_limit(l.animation_mb);
        }
        let mut changed = chosen.is_some();
        let restart = tr("Decoding threads and one malloc arena take effect after a restart.");
        egui::CollapsingHeader::new(tr("Details"))
            .open((level.is_none() || custom).then_some(true))
            .show(ui, |ui| {
                let mut n = self.config.preload;
                ui.label(tr("Preload next and previous images:"));
                let slider = egui::Slider::new(&mut n, 0..=20).text(tr("images on each side"));
                if ui.add(slider).changed() {
                    self.set_preload(n);
                    changed = true;
                }
                let [w, h] = self.decoding.view_box();
                let mb = (2 * n as u64 + 1) * w as u64 * h as u64 * 4 / 1_000_000;
                ui.weak(tf!("Up to ≈ {mb} MB, each image at most {w}×{h}", mb, w, h));
                let c = &mut self.config;
                changed |= ui
                    .add(
                        egui::Slider::new(&mut c.decode_threads, 1..=8)
                            .text(tr("images decoded at once")),
                    )
                    .changed();
                let animation = egui::Slider::new(&mut c.animation_mb, 8..=1024)
                    .logarithmic(true)
                    .text(tr("MB at most for an animation's frames"));
                if ui.add(animation).changed() {
                    crate::images::decode::set_animation_limit(c.animation_mb);
                    changed = true;
                }
                let low = tr("One malloc arena: much less memory, decoding up to ~8 % slower");
                changed |= checkbox(ui, &mut c.one_malloc_arena, low).changed();
                ui.weak(restart);
            });
        changed
    }

    /// Preferences → CPU use: power saving, balanced or full.
    pub(crate) fn cpu_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(tr("CPU use:"));
            for (k, (name, hover)) in CPU_LEVELS.iter().enumerate() {
                if radio(ui, self.config.cpu_use == k, tr(name))
                    .on_hover_text(tr(hover))
                    .clicked()
                {
                    self.config.cpu_use = k;
                    set_cpu_use(k);
                    self.config.save();
                }
            }
        });
    }

    /// Colour management, as in Geeqie's View → Color Management and in Preferences: on or off,
    /// the profile of images without one, whether an image's own counts, and the screen profile.
    pub(crate) fn colour_settings(&mut self, ui: &mut egui::Ui) {
        let mut cm = self.config.color_management;
        if checkbox(ui, &mut cm, tr("Colour management")).changed() {
            self.set_color_management(cm);
        }
        ui.add_enabled_ui(cm, |ui| {
            let mut own = self.config.use_image_profile;
            if checkbox(ui, &mut own, tr("Use the image's own profile")).changed() {
                self.set_own_profile(own);
            }
            ui.label(tr("Images without a profile:"));
            let current = self.config.input_profile.clone();
            for (value, label, _) in INPUT_PROFILES {
                if radio(ui, current == value, label).clicked() && current != value {
                    self.set_input_profile(ui.ctx(), value.to_owned());
                }
            }
            let own_file = !INPUT_PROFILES.iter().any(|p| p.0 == current);
            let file = Path::new(&current).file_name().filter(|_| own_file);
            let label = file.map_or(tr("Profile file…").into(), |n| {
                format!("{}…", n.to_string_lossy())
            });
            if radio(ui, own_file, &label).clicked() {
                self.pick(&ui.ctx().clone(), false, Pick::InputProfile);
            }
            let mut desktop = self.config.desktop_profile;
            if checkbox(ui, &mut desktop, tr("Use the desktop's screen profile"))
                .on_hover_text(tr("From colord, which GNOME and KDE set"))
                .changed()
            {
                self.set_desktop_profile(desktop, ui.ctx());
            }
            let from_desktop = (self.desktop_settings.screen_profile().map(str::to_owned))
                .filter(|_| self.config.desktop_profile);
            ui.horizontal(|ui| {
                ui.label(tr("Screen profile:"));
                if let Some(path) = from_desktop {
                    let name = Path::new(&path).file_name().unwrap_or_default();
                    let name = tf!("{name}, from the desktop", name = name.to_string_lossy());
                    ui.label(name).on_hover_text(path);
                    return;
                }
                let file = Path::new(&self.config.screen_profile).file_name();
                let name = file.map_or(tr("none, sRGB").into(), |n| n.to_string_lossy());
                ui.label(name).on_hover_text(&self.config.screen_profile);
                if crate::ui::menus::button(ui, tr("Choose…")).clicked() {
                    self.pick(&ui.ctx().clone(), false, Pick::ScreenProfile);
                }
                if !self.config.screen_profile.is_empty()
                    && crate::ui::menus::button(ui, tr("None")).clicked()
                {
                    self.set_screen_profile(ui.ctx(), String::new());
                }
            });
            ui.label(tr("Rendering intent:"));
            let current = self.config.rendering_intent.clone();
            for (value, label, _) in INTENTS {
                if radio(ui, current == value, tr(label)).clicked() && current != value {
                    self.set_rendering_intent(value.to_owned());
                }
            }
        });
    }

    /// Preferences: file details, and saving and cleaning the shared thumbnail cache.
    pub(crate) fn cache_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        changed |= checkbox(
            ui,
            &mut c.file_details,
            tr("Show file size and date in the file list"),
        )
        .changed();
        let save = tr("Save new thumbnails to ~/.cache/thumbnails (shared with other programs)");
        let kept = tr(
            "Copies of the pictures in your home folder: those of an encrypted volume or a stick stay readable there",
        );
        if checkbox(ui, &mut c.save_thumbnails, save)
            .on_hover_text(kept)
            .changed()
        {
            self.thumbs.save_to_cache(c.save_thumbnails);
            changed = true;
        }
        // Geeqie's cache maintenance: thumbnails of files that are gone.
        if crate::ui::menus::button(ui, tr("Remove thumbnails of deleted files"))
            .on_hover_text(tr(
                "In ~/.cache/thumbnails; kept for disks that are not plugged in",
            ))
            .clicked()
        {
            let (tx, ctx) = (self.notice_sender.clone(), ctx.clone());
            let pointer = Working::new(&self.busy);
            std::thread::spawn(move || {
                let (removed, seen) = thumbs::remove_orphaned_thumbnails();
                let _ = tx.send(tf!(
                    "Removed {removed} of {seen} thumbnails",
                    removed = thousands(removed),
                    seen = thousands(seen)
                ));
                drop(pointer);
                ctx.request_repaint();
            });
        }
        changed
    }

    /// Preferences: the toolbar, folder path, search field, info panel and full-screen info.
    pub(crate) fn bar_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        self.toggle_box(
            ui,
            self.config.toolbar,
            Action::Toolbar,
            tr("Show the toolbar"),
        );
        let path = tr("Show the folder path");
        self.toggle_box(ui, self.config.path_bar, Action::PathBar, path);
        let c = &mut self.config;
        if checkbox(ui, &mut c.search_bar, tr("Show the file name search")).changed() {
            changed = true;
            if !c.search_bar {
                self.search.clear_query();
                self.update_search(ctx);
            }
        }
        ui.horizontal(|ui| {
            ui.label(tr("Toolbar icons:"));
            for (n, label) in [tr("Small"), tr("Medium"), tr("Large")]
                .into_iter()
                .enumerate()
            {
                changed |= radio_choice(ui, &mut self.config.toolbar_icons, n, label).changed();
            }
        });
        changed
    }

    /// Draws the thumbnail sizes as radio buttons, and sets the size clicked. Preferences and
    /// View → Thumbnail Size both show these, so the two always offer the same sizes. Its name
    /// ends in _setting, so the search of Preferences finds the sizes.
    pub(crate) fn thumb_size_setting(&mut self, ui: &mut egui::Ui) {
        let mut size = self.config.thumb_size;
        for (n, name) in [(0, tr("Small")), (1, tr("Medium")), (2, tr("Large"))] {
            if radio_choice(ui, &mut size, n, name).changed() {
                self.set_thumb_size(size);
            }
        }
    }

    /// Preferences → File list: the info panel.
    fn info_panel_setting(&mut self, ui: &mut egui::Ui) {
        let exif = tr("Show the info panel (Ctrl+K; reads a little from each image shown)");
        self.toggle_box(ui, self.config.info_panel, Action::InfoPanel, exif);
    }

    /// Preferences → Image: the full screen info and its template.
    fn info_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let label = tr("Show image info in full screen (I)");
        self.toggle_box(
            ui,
            self.config.full_screen_info,
            Action::FullScreenInfo,
            label,
        );
        let c = &mut self.config;
        ui.label(tr("Image info template:"));
        // Kept on one line in the config file, with \n for each new line.
        let mut text = c.full_screen_info_template.replace("\\n", "\n");
        let field = egui::TextEdit::multiline(&mut text)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("%name%  %position%");
        if ui.add_enabled(c.full_screen_info, field).changed() {
            c.full_screen_info_template = text.replace('\n', "\\n");
            changed = true;
        }
        ui.weak(tr(
            "Fields: %name%, %path%, %position%, %dimensions%, %marks%, %keywords%, %comment%, \
             and each label of the info panel, such as %Camera% or %ISO%. A | starts a part, \
             left out when its fields are empty.",
        ));
        if crate::ui::menus::button(ui, tr("Default")).clicked() {
            c.full_screen_info_template = FULL_SCREEN_INFO_TEMPLATE.to_owned();
            changed = true;
        }
        changed
    }

    /// Preferences: refreshing, the folder and window at start, remote control, the screen saver
    /// and the window title.
    pub(crate) fn window_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let (mut changed, mut clear) = (false, false);
        let c = &mut self.config;
        changed |= checkbox(ui, &mut c.remember_dir, tr("Start in the last used folder")).changed();
        let recent = tr("Keep a list of recent folders (File → Open Recent)");
        if checkbox(ui, &mut c.keep_recent, recent).changed() {
            changed = true;
            // Off: the list kept so far goes too, below, once `c` is done with.
            clear = !c.keep_recent;
        }
        let window = tr("Remember the window size and position");
        changed |= checkbox(ui, &mut c.remember_window, window).changed();
        let remote = tr("Listen for glimt --remote commands (after restart)");
        changed |= checkbox(ui, &mut c.remote_control, remote).changed();
        let inhibit = tr("Keep the screen saver off in full screen");
        changed |= checkbox(ui, &mut c.inhibit_screensaver, inhibit).changed();
        ui.label(tr("Custom window title (empty: \"image name - Glimt\"):"));
        let field = egui::TextEdit::singleline(&mut self.config.window_title)
            .desired_width(f32::INFINITY)
            .hint_text("%name% - %app%");
        if ui.add(field).changed() {
            changed = true;
            self.update_window_title(ctx);
        }
        ui.weak(tr(
            "Fields: %name%, %path%, %folder%, %app% and %id%. A | starts a part, left out when \
             its fields are empty.",
        ));
        if clear {
            self.clear_recent();
        }
        changed
    }

    /// Preferences → File list: the folder read again when its files change.
    fn refresh_setting(&mut self, ui: &mut egui::Ui) -> bool {
        let refresh = tr("Refresh the folder when files change");
        checkbox(ui, &mut self.config.auto_refresh, refresh).changed()
    }

    /// Preferences → Keyboard shortcuts: every action with a key, which can be changed (the next
    /// key press) or reset to Geeqie's.
    pub(crate) fn key_settings(&mut self, ui: &mut egui::Ui, query: &str) {
        if let Some(action) = self.preferences.key_being_chosen_for {
            let pressed = ui.input(pressed_key);
            if let Some((key, mods)) = pressed {
                self.preferences.key_being_chosen_for = None;
                if key != egui::Key::Escape {
                    let mods = egui::Modifiers {
                        mac_cmd: false,
                        ..mods
                    };
                    // Said when another action has the key: the new one takes it, and the other
                    // no longer answers to it.
                    let taken: Vec<&str> = (shortcuts().iter())
                        .filter(|s| s.0 != action && actions::same_keys(s, &(action, mods, key)))
                        .map(|s| s.0.label())
                        .collect();
                    if !taken.is_empty() {
                        let keys = ui
                            .ctx()
                            .format_shortcut(&egui::KeyboardShortcut::new(mods, key));
                        self.notice.give(tf!(
                            "{keys} was the key of {others} too: now of {label} only",
                            keys,
                            others = taken.join(", "),
                            label = action.label()
                        ));
                    }
                    // The action's own key before, and another action's own key of this one.
                    self.config
                        .keys
                        .0
                        .retain(|k| k.0 != action && !actions::same_keys(k, &(action, mods, key)));
                    self.config.keys.0.push((action, mods, key));
                    self.config.save();
                    crate::ui::actions::use_user_keys(&self.config.keys);
                }
            }
        }
        let all = shortcuts();
        let mut actions = key_actions();
        // Those a search finds, or all when it finds none (it found the section's own words).
        let has = |a: &Action| section_matches(query, &[], &[a.english(), a.label()]);
        if actions.iter().any(has) {
            actions.retain(has);
        }
        egui::Grid::new("keys").striped(true).show(ui, |ui| {
            for action in actions {
                ui.label(action.label());
                let keys: Vec<String> = (all.iter().filter(|s| s.0 == action))
                    .map(|s| {
                        ui.ctx()
                            .format_shortcut(&egui::KeyboardShortcut::new(s.1, s.2))
                    })
                    .collect();
                if self.preferences.key_being_chosen_for == Some(action) {
                    ui.strong(tr("Press a key (Esc cancels)"));
                } else {
                    ui.label(keys.join(", "));
                }
                ui.horizontal(|ui| {
                    let change = crate::ui::menus::small_button(ui, tr("Change"));
                    if crate::support::test_hooks::noted(
                        &change,
                        &format!("Change {}", action.label()),
                    )
                    .clicked()
                    {
                        self.preferences.key_being_chosen_for = Some(action);
                    }
                    let own = self.config.keys.0.iter().any(|k| k.0 == action);
                    if own && crate::ui::menus::small_button(ui, tr("Reset")).clicked() {
                        self.config.keys.0.retain(|k| k.0 != action);
                        self.config.save();
                        crate::ui::actions::use_user_keys(&self.config.keys);
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Preferences: confirmations, the Delete key and the trash.
    pub(crate) fn delete_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        changed |= checkbox(
            ui,
            &mut c.confirm_delete,
            tr("Confirm permanent file delete"),
        )
        .changed();
        changed |= checkbox(ui, &mut c.confirm_trash, tr("Confirm move file to Trash")).changed();
        changed |= checkbox(ui, &mut c.delete_key, tr("Enable Delete key")).changed();
        let mut no_trash = !c.delete_to_trash;
        let none = tr("Use no trash at all (Delete deletes permanently)");
        if checkbox(ui, &mut no_trash, none).changed() {
            c.delete_to_trash = !no_trash;
            changed = true;
        }
        changed
    }

    /// Preferences: videos in the list and the player.
    #[cfg(feature = "video")]
    pub(crate) fn video_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        ui.weak(tr("Previews need ffmpeg"));
        if checkbox(ui, &mut c.list_videos, tr("Show videos in the file list")).changed() {
            changed = true;
            self.start_listing(ctx);
        }
        ui.horizontal(|ui| {
            ui.label(tr("Player (Enter):"));
            let player = egui::TextEdit::singleline(&mut self.config.video_player)
                .hint_text(tr("default application"));
            let hint =
                tr("A command line, such as mpv --fs; a path with spaces goes in \"quotes\"");
            changed |= ui.add(player).on_hover_text(hint).changed();
        });
        changed
    }
}

/// Returns the key pressed this frame with its modifiers, for a key being chosen, or `None`
/// while none is. A modifier on its own is not the key, so the one pressed with it is waited
/// for. Ctrl+C and Ctrl+X count as keys too, though egui-winit sends them as Copy and Cut
/// events. Those events mean Ctrl+C and Ctrl+X even from the Copy and Cut keys, with no
/// modifier held, as `App::shortcut` reads them.
fn pressed_key(i: &egui::InputState) -> Option<(egui::Key, egui::Modifiers)> {
    use egui::Key as K;
    i.events.iter().find_map(|e| match e {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } if !matches!(
            key,
            K::ShiftLeft
                | K::ShiftRight
                | K::ControlLeft
                | K::ControlRight
                | K::AltLeft
                | K::AltRight
                | K::SuperLeft
                | K::SuperRight
        ) =>
        {
            Some((*key, *modifiers))
        }
        e => actions::clipboard_key(e).map(|key| (key, actions::CTRL)),
    })
}

/// A section of Preferences: its icon in the accent colour and its title, over a line.
fn section(ui: &mut egui::Ui, icon: &str, title: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let accent = ui.visuals().selection.bg_fill;
        ui.label(egui::RichText::new(icon).size(18.0).color(accent));
        ui.strong(title);
    });
    ui.separator();
}

/// Whether a section with these texts (English, the title first) and `more` (as shown) has
/// every word of the lowercase `query`, in English or in the language shown; always without
/// a search. Only a few hundred short texts, and only while there is a search.
fn section_matches(query: &str, texts: &[&'static str], more: &[&str]) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let mut all = String::new();
    for t in texts {
        all.push_str(t);
        all.push('\n');
        all.push_str(tr(t));
        all.push('\n');
    }
    for t in more {
        all.push_str(t);
        all.push('\n');
    }
    let all = all.to_lowercase();
    query.split_whitespace().all(|w| all.contains(w))
}

/// Returns the actions the keyboard section lists: every action built in, once. Those with a
/// default key come first, in the order of `SHORTCUTS`, and the others follow, as each can be
/// given a key of its own. The section's rows and its search both read this list.
fn key_actions() -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();
    for &action in (SHORTCUTS.iter().map(|s| &s.0)).chain(ALL) {
        if !actions.contains(&action) && action.is_in_build() {
            actions.push(action);
        }
    }
    actions
}

/// Which sections of Preferences have a text the search `query` (lower case) finds, in the
/// order they show: the look, keys, image, slideshow, file list, deleting, colour, memory,
/// video and start.
fn sections_shown(query: &str) -> [bool; 10] {
    let shows = |texts: &[&'static str], more: &[&str]| section_matches(query, texts, more);
    // The search finds a level by its English name and by the name shown.
    let levels = |l: &config::MemoryLevel| [l.name, l.tooltip];
    let levels: Vec<&str> = (MEMORY_LEVELS.iter().flat_map(levels))
        .chain(CPU_LEVELS.iter().flat_map(|l| [l.0, l.1]))
        .flat_map(|t| [t, tr(t)])
        .collect();
    // These are only the actions the section lists. A search for one that is not built in
    // would find none of its rows, and the section would then show every action.
    let actions: Vec<&str> = (key_actions().iter())
        .flat_map(|a| [a.english(), a.label()])
        .collect();
    [
        shows(LOOK, &[]),
        shows(KEYBOARD, &actions),
        shows(IMAGE, &[]),
        shows(SLIDESHOW, &[]),
        shows(FILE_LIST, &[]),
        shows(DELETING, &[]),
        shows(COLOUR, &[]),
        shows(MEMORY, &levels),
        cfg!(feature = "video") && shows(VIDEO, &[]),
        shows(START, &[]),
    ]
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;

    use super::*;

    /// The `tr("…")` texts of `code`, and of the settings methods it calls (theirs, not
    /// what those do: `self.list` is not a setting).
    fn texts(code: &str, out: &mut Vec<String>) {
        let sources = [include_str!("preferences.rs"), include_str!("menus.rs")];
        let literals = crate::support::source_rules::literals(code);
        // tf!'s templates too: the search matches their words, not the values put in. Not the
        // notices a setting's button sends, which are not on the window.
        for call in ["tr(", "tf!("] {
            for (at, _) in code.match_indices(call) {
                // Part of a longer name (`str(`), not a call after `return`.
                let named = code[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
                let before = code[..at].trim_end();
                if named || before.ends_with("send(") || before.ends_with("notice.give(") {
                    continue;
                }
                let after = at + call.len();
                let quote = code.len() - code[after..].trim_start().len();
                if let Some((_, text)) = literals.iter().find(|l| l.0 == quote) {
                    out.push(text.clone());
                }
            }
        }
        for (at, _) in code.match_indices("self.") {
            let name: String = (code[at + 5..].chars())
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let setting = name.ends_with("_settings") || name.ends_with("_setting");
            if !(setting || name == "theme_buttons")
                || !code[at + 5 + name.len()..].starts_with('(')
            {
                continue;
            }
            let head = format!("fn {name}(");
            for src in sources {
                if let Some(start) = src.find(&head) {
                    let body = &src[start..];
                    let end = body.find("\n    }\n").unwrap_or(body.len());
                    texts(&body[..end], out);
                }
            }
        }
    }

    /// The texts a piece of code shows: after `return` too, not a longer name's or a notice's.
    #[test]
    fn texts_finds_the_calls_shown() {
        let code = r#"if a { return tr("A"); } ui.label(tr("B")); ui.weak(tf!("C {x}", x));
            let _ = tx.send(tf!("D")); self.notice.give(tf!("E")); attr("F");"#;
        let mut out = Vec::new();
        texts(code, &mut out);
        assert_eq!(out, ["A", "B", "C {x}"]);
    }

    /// Each section's list has every text the section shows, and no other.
    #[test]
    fn the_search_knows_every_text() {
        let src = include_str!("preferences.rs");
        let function = |name: &str| {
            let start = src.find(&format!("fn {name}(")).unwrap();
            &src[start..start + src[start..].find("\n    }\n").unwrap()]
        };
        // The look's section is a function of its own, called where it shows.
        let body = (function("preferences_window"))
            .replace("self.look_settings(ui, ctx)", function("look_settings"));
        let sections = [
            LOOK, KEYBOARD, IMAGE, SLIDESHOW, FILE_LIST, DELETING, COLOUR, MEMORY, VIDEO, START,
        ];
        let mut parts = body.split("section(ui,").skip(1);
        let mut wrong = Vec::new();
        for list in sections {
            let mut shown = Vec::new();
            texts(parts.next().unwrap(), &mut shown);
            shown.sort();
            shown.dedup();
            let mut listed: Vec<String> = list[1..].iter().map(|t| t.to_string()).collect();
            listed.sort();
            if shown != listed {
                wrong.push(format!("{}: {shown:#?}", list[0]));
            }
            // The titles go through tr from the lists, which the translation test cannot see.
            if !include_str!("../../po/no_nb.po").contains(&format!("msgid \"{}\"\n", list[0])) {
                wrong.push(format!("{} is not translated", list[0]));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_search_finds_every_word_in_either_language() {
        assert!(section_matches("", LOOK, &[]));
        assert!(section_matches("  ", LOOK, &[]));
        assert!(section_matches("theme", LOOK, &[]));
        assert!(section_matches("save theme", LOOK, &[]));
        assert!(!section_matches("theme slideshow", LOOK, &[]));
        assert!(section_matches("rotate", KEYBOARD, &["Rotate Clockwise"]));
        // The preload estimate, made with tf!: its template's words.
        assert!(section_matches("each image at most", MEMORY, &[]));
    }

    /// Esc in Preferences clears a search first and closes the window the next time; with no
    /// search, or one of only spaces, it closes at once.
    #[test]
    fn escape_clears_the_preferences_search_then_closes() {
        use eframe::egui::{Event, Key, RawInput};
        let dir = std::env::temp_dir().join(format!("glimt-test-esc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let escape = |app: &mut crate::App| {
            let input = RawInput {
                events: vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.preferences_window(ui.ctx()));
        };
        app.preferences.show();
        app.preferences.query = "zoom".into();
        escape(&mut app);
        assert!(app.preferences.open && app.preferences.query.is_empty());
        escape(&mut app);
        assert!(!app.preferences.open);
        app.preferences.show();
        app.preferences.query = "  ".into();
        escape(&mut app);
        assert!(!app.preferences.open && app.preferences.query.is_empty());
        // A dialog on top of Preferences takes the Esc, and Preferences stays open.
        app.preferences.show();
        app.dialogs.ask_trash_folder(&dir);
        escape(&mut app);
        assert!(app.preferences.open);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The keyboard section lists every action built in, also one without a default key, so
    /// that each can be given a key of its own. Before, it listed only those of `SHORTCUTS`.
    #[test]
    fn every_action_built_in_can_be_given_a_key() {
        let listed = super::key_actions();
        for action in crate::ui::actions::ALL.iter().filter(|a| a.is_in_build()) {
            assert!(listed.contains(action), "{action:?}");
        }
    }

    /// Esc in another field of Preferences, such as the custom window title, only leaves the
    /// field, and Esc in an open combo box only closes it. Both once closed Preferences too.
    #[test]
    fn escape_in_a_field_or_popup_keeps_preferences_open() {
        use eframe::egui::{self, Event, Key, RawInput};
        let dir = std::env::temp_dir().join(format!("glimt-test-esc-pref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.preferences.show();
        let field = egui::Id::unique("a field of Preferences");
        let frame = |app: &mut crate::App, events: Vec<Event>, focus: bool, popup: bool| {
            let input = RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                app.preferences_window(ui.ctx());
                if focus {
                    ui.memory_mut(|m| m.request_focus(field));
                }
                if popup {
                    egui::Popup::open_id(ui.ctx(), egui::Id::unique("a combo box"));
                }
                app.note_focus(ui.ctx());
            });
        };
        let esc = || {
            vec![Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]
        };
        frame(&mut app, Vec::new(), true, false);
        frame(&mut app, esc(), false, false);
        assert!(
            app.preferences.open,
            "Esc that left a field closed Preferences"
        );
        frame(&mut app, Vec::new(), false, true);
        frame(&mut app, esc(), false, false);
        assert!(
            app.preferences.open,
            "Esc that closed a popup closed Preferences"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A search for an action that is not built in, such as Play Video without video, does
    /// not open the keyboard section, which would then list every action.
    #[test]
    fn the_search_finds_only_listed_actions() {
        let listed = super::key_actions();
        for s in SHORTCUTS.iter().filter(|s| !listed.contains(&s.0)) {
            let query = s.0.english().to_lowercase();
            assert!(!super::sections_shown(&query)[1], "{query}");
        }
    }

    /// Ctrl+C and Ctrl+X can be chosen as keys, though egui-winit sends them as Copy and Cut
    /// events instead of key presses. The Copy and Cut keys send the same events with no
    /// modifier held, and they are chosen as Ctrl+C and Ctrl+X, as `App::shortcut` reads them.
    #[test]
    fn ctrl_c_and_ctrl_x_can_be_chosen() {
        for (held, event, key) in [
            (actions::CTRL, egui::Event::Copy, egui::Key::C),
            (actions::CTRL, egui::Event::Cut, egui::Key::X),
            (egui::Modifiers::NONE, egui::Event::Copy, egui::Key::C),
        ] {
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::ModifiersChanged(held), event],
                ..Default::default()
            });
            assert_eq!(ctx.input(super::pressed_key), Some((key, actions::CTRL)));
        }
    }

    /// Choosing a key ends when its row is no longer drawn: with the Keyboard shortcuts header
    /// closed, and with Preferences closed.
    #[test]
    fn choosing_a_key_ends_with_its_row() {
        let dir = std::env::temp_dir().join(format!("glimt-test-capture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let mut time = 0.0;
        let mut frame = |app: &mut crate::App| {
            time += 0.1;
            let input = eframe::egui::RawInput {
                time: Some(time),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.preferences_window(ui.ctx()));
        };
        // The header starts closed.
        app.preferences.open = true;
        app.preferences.key_being_chosen_for = Some(Action::Next);
        frame(&mut app);
        assert_eq!(app.preferences.key_being_chosen_for, None);
        // A search for the action opens the header, so its row is drawn and the choosing stays.
        app.preferences.query = "next image".into();
        app.preferences.key_being_chosen_for = Some(Action::Next);
        frame(&mut app);
        assert_eq!(app.preferences.key_being_chosen_for, Some(Action::Next));
        // A closed window fades out first, drawing its rows a few frames more.
        app.preferences.open = false;
        for _ in 0..30 {
            frame(&mut app);
        }
        assert_eq!(app.preferences.key_being_chosen_for, None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
