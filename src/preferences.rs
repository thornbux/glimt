//! Edit → Preferences: one window of every setting, in sections, saved when one changes (when
//! a slider is let go). The settings themselves are `Config`'s (config.rs).

use crate::*;

impl crate::App {
    /// Edit → Preferences: every setting, saved when one changes; Esc closes it.
    pub(crate) fn preferences_window(&mut self, ctx: &egui::Context) {
        let mut open = self.preferences_open;
        // Esc closes Preferences (shortcuts are off while it is open), unless a dialog on top
        // of it is the one Esc is meant for.
        if open
            && self.prompt.is_none()
            && self.confirm_delete.is_none()
            && self.capturing.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            open = false;
        }
        egui::Window::new(tr("Preferences"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                // Scrolls when the window is smaller than the settings; a solid scroll bar
                // takes its own space instead of covering the text.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Plain settings set `changed`; the config is saved once at the end.
                    let mut changed = false;
                    // The look first, where it is found (also under View → Theme).
                    ui.horizontal(|ui| {
                        let mut chosen = false;
                        egui::ComboBox::from_id_salt("theme")
                            .selected_text(self.config.theme.clone())
                            .show_ui(ui, |ui| {
                                for name in theme::names() {
                                    let c = &mut self.config.theme;
                                    chosen |= ui.selectable_value(c, name.clone(), name).clicked();
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
                                "en" => "English".to_owned(),
                                code => (lang::LANGUAGES.iter().find(|l| l.0 == code))
                                    .map_or(code, |l| l.1)
                                    .to_owned(),
                            }
                        };
                        let mut chosen = false;
                        let codes = ["", "en"]
                            .into_iter()
                            .chain(lang::LANGUAGES.iter().map(|l| l.0));
                        egui::ComboBox::from_id_salt("language")
                            .selected_text(name(&self.config.language))
                            .show_ui(ui, |ui| {
                                for code in codes {
                                    let c = &mut self.config.language;
                                    chosen |= ui
                                        .selectable_value(c, code.to_owned(), name(code))
                                        .clicked();
                                }
                            });
                        ui.label(tr("Language"));
                        if chosen {
                            changed = true;
                            lang::set(&self.config.language);
                        }
                    });
                    let follow = tr("High Contrast when the desktop asks for high contrast");
                    if ui
                        .checkbox(&mut self.config.follow_contrast, follow)
                        .changed()
                    {
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
                    if ui
                        .checkbox(&mut self.config.follow_text_scale, follow)
                        .changed()
                    {
                        changed = true;
                        self.apply_theme(ui.ctx());
                    }
                    ui.separator();
                    changed |= self.memory_settings(ui);
                    self.cpu_settings(ui);
                    ui.separator();
                    changed |= self.slideshow_settings(ui, ctx);
                    ui.separator();
                    changed |= self.image_settings(ui);
                    ui.separator();
                    changed |= self.search_settings(ui, ctx);
                    ui.separator();
                    changed |= self.list_settings(ui, ctx);
                    ui.separator();
                    self.colour_settings(ui);
                    ui.separator();
                    changed |= self.cache_settings(ui, ctx);
                    ui.separator();
                    changed |= self.panel_settings(ui, ctx);
                    ui.separator();
                    changed |= self.window_settings(ui, ctx);
                    ui.separator();
                    egui::CollapsingHeader::new(tr("Keyboard shortcuts")).show(ui, |ui| {
                        self.key_settings(ui);
                    });
                    ui.separator();
                    changed |= self.delete_settings(ui);
                    #[cfg(feature = "video")]
                    {
                        ui.separator();
                        changed |= self.video_settings(ui, ctx);
                    }
                    // Saved once a slider is let go, not in every frame it is dragged.
                    self.preferences_unsaved |= changed;
                    if self.preferences_unsaved && !ctx.input(|i| i.pointer.any_down()) {
                        self.preferences_unsaved = false;
                        self.config.save();
                    }
                });
            });
        self.preferences_open = open;
    }

    /// Preferences: the slideshow's delay, order, subfolders, repeat and fade.
    pub(crate) fn slideshow_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        ui.label(tr(tr(
            "Slideshow (S, R = random, Ctrl+= faster, Ctrl+- slower):",
        )));
        let delay =
            egui::Slider::new(&mut self.config.delay, 1..=MAX_DELAY).text(tr("seconds per image"));
        if ui.add(delay).changed() {
            changed = true;
            self.title(ctx);
        }
        let mut random = self.config.random;
        if ui.checkbox(&mut random, tr("Random order")).changed() {
            self.set_random_order(ctx, random);
        }
        changed |= ui
            .checkbox(
                &mut self.config.slideshow_subfolders,
                tr("Include the images in subfolders"),
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.config.slideshow_repeat,
                tr("Repeat: start again after the last image"),
            )
            .changed();
        ui.horizontal(|ui| {
            ui.label(tr("Fade to the next image:"));
            for (k, (_, label, _)) in FADES.iter().enumerate() {
                changed |= ui
                    .radio_value(&mut self.config.fade, k, *label)
                    .on_hover_text(tr("Not with CPU use Power saving"))
                    .changed();
            }
        });
        changed
    }

    /// Preferences: rotation by EXIF and saving it, the background, enlarging, zoom and the wheel.
    pub(crate) fn image_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let mut rotate = self.config.exif_rotate;
        if ui
            .checkbox(&mut rotate, tr("Auto rotate images using EXIF (Alt+X)"))
            .changed()
        {
            self.set_exif_rotate(rotate);
        }
        changed |= ui
            .checkbox(
                &mut self.config.save_orientation,
                tr("Save rotation to the file when changing image (JPEG; PNG and WebP are saved again)"),
            )
            .changed();
        let quality = egui::Slider::new(&mut self.config.webp_quality, 50..=100)
            .text(tr("quality when a lossy WebP is saved again"));
        changed |= ui.add(quality).changed();
        ui.horizontal(|ui| {
            changed |= ui
                .color_edit_button_srgb(&mut self.config.background.0)
                .changed();
            ui.label(tr("Background behind the image"));
            if self.config.background.0 != [0, 0, 0] && ui.small_button(tr("Black")).clicked() {
                self.config.background.0 = [0, 0, 0];
                changed = true;
            }
        });
        let c = &mut self.config;
        changed |= ui
            .checkbox(&mut c.enlarge, tr("Enlarge small images to fit the window"))
            .changed();
        changed |= ui
            .checkbox(
                &mut c.keep_zoom,
                tr("Keep zoom and position when changing image"),
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut c.wheel_zoom,
                tr("Mouse wheel zooms (off: next/previous image, Ctrl+wheel zooms)"),
            )
            .changed();
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
            if ui.radio(fuzzy == on, label).clicked() && fuzzy != on {
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
        let mut changed = false;
        let mut show = self.config.thumbnails;
        if ui
            .checkbox(&mut show, tr("Show thumbnails in the file list (T)"))
            .changed()
        {
            self.set_thumbnails(show);
        }
        ui.horizontal(|ui| {
            ui.label(tr("Thumbnail size:"));
            let mut size = self.config.thumb_size;
            for (n, name) in [(0, tr("Small")), (1, tr("Medium")), (2, tr("Large"))] {
                if ui.radio_value(&mut size, n, name).changed() {
                    self.set_thumb_size(size);
                }
            }
        });
        let c = &mut self.config;
        changed |= ui
            .checkbox(&mut c.icons, tr("Show the file list as icons (Ctrl+I)"))
            .changed();
        changed |= ui
            .checkbox(&mut c.folder_tree, tr("Show folders as a tree (Ctrl+T)"))
            .changed();
        let mut sub = c.show_subfolders;
        if ui
            .checkbox(
                &mut sub,
                tr("Include the images in subfolders in the file list"),
            )
            .changed()
        {
            self.set_subfolders(ctx, sub);
        }
        let c = &mut self.config;
        #[cfg(feature = "marks")]
        {
            changed |= ui
                .checkbox(
                    &mut c.show_marks,
                    tr("Show marks and stars in the file list"),
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut c.xmp_stars,
                    tr("Also write stars to XMP sidecars (photo.jpg.xmp), for darktable and digiKam"),
                )
                .changed();
        }
        ui.horizontal(|ui| {
            ui.label(tr("Histogram (K):"));
            for (n, name) in [(0, tr("Off")), (1, "RGB"), (2, tr("Brightness"))] {
                changed |= ui.radio_value(&mut c.histogram, n, name).changed();
            }
            changed |= ui
                .checkbox(&mut c.histogram_log, tr("Logarithmic (J)"))
                .changed();
        });
        changed |= ui
            .checkbox(&mut c.animate, tr("Play animated GIF and WebP (A)"))
            .changed();
        changed
    }

    /// Preferences → Memory use: Low, Medium or High set the settings below it at once; other
    /// values are Custom.
    pub(crate) fn memory_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let c = &self.config;
        let now = (c.preload, c.decode_threads, c.animation_mb, c.low_memory);
        let level = (MEMORY_LEVELS.iter())
            .find(|l| (l.preload, l.decode_threads, l.animation_mb, l.one_arena) == now);
        let (mut chosen, mut custom) = (None, false);
        ui.horizontal(|ui| {
            ui.label(tr("Memory use:"));
            for l in &MEMORY_LEVELS {
                let on = level.is_some_and(|m| m.name == l.name);
                if ui.radio(on, l.name).on_hover_text(l.hover).clicked() {
                    chosen = Some(*l);
                }
            }
            // Chosen by changing the details, which it opens.
            custom = ui.radio(level.is_none(), tr("Custom")).clicked();
        });
        if let Some(l) = chosen {
            self.set_preload(l.preload);
            (self.config.decode_threads, self.config.animation_mb) =
                (l.decode_threads, l.animation_mb);
            self.config.low_memory = l.one_arena;
            MAX_ANIMATION.store(l.animation_mb << 20, Ordering::Relaxed);
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
                let [w, h] = self.limits.lock().unwrap()[0];
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
                    MAX_ANIMATION.store(c.animation_mb << 20, Ordering::Relaxed);
                    changed = true;
                }
                let low = tr("One malloc arena: much less memory, decoding up to ~8 % slower");
                changed |= ui.checkbox(&mut c.low_memory, low).changed();
                ui.weak(restart);
            });
        changed
    }

    /// Preferences → CPU use: power saving, balanced or full.
    pub(crate) fn cpu_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(tr("CPU use:"));
            for (k, (name, hover)) in CPU_LEVELS.iter().enumerate() {
                if ui
                    .radio(self.config.cpu_use == k, *name)
                    .on_hover_text(*hover)
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
        if ui.checkbox(&mut cm, tr("Colour management")).changed() {
            self.set_color_management(cm);
        }
        ui.add_enabled_ui(cm, |ui| {
            let mut own = self.config.own_profile;
            if ui
                .checkbox(&mut own, tr("Use the image's own profile"))
                .changed()
            {
                self.set_own_profile(own);
            }
            ui.label(tr("Images without a profile:"));
            let current = self.config.input_profile.clone();
            for (value, label) in INPUT_PROFILES {
                if ui.radio(current == value, label).clicked() && current != value {
                    self.set_input_profile(value.to_owned());
                }
            }
            let own_file = !INPUT_PROFILES.iter().any(|p| p.0 == current);
            let file = Path::new(&current).file_name().filter(|_| own_file);
            let label = file.map_or(tr("Profile file…").into(), |n| {
                format!("{}…", n.to_string_lossy())
            });
            if ui.radio(own_file, label).clicked() {
                self.pick(&ui.ctx().clone(), false, Pick::InputProfile);
            }
            let mut desktop = self.config.desktop_profile;
            if ui
                .checkbox(&mut desktop, tr("Use the desktop's screen profile"))
                .on_hover_text(tr("From colord, which GNOME and KDE set"))
                .changed()
            {
                self.set_desktop_profile(desktop, ui.ctx());
            }
            let from_desktop =
                (self.desktop_screen.clone()).filter(|_| self.config.desktop_profile);
            ui.horizontal(|ui| {
                ui.label(tr("Screen profile:"));
                if let Some(path) = from_desktop {
                    let name = Path::new(&path).file_name().unwrap_or_default();
                    let name = format!("{}, from the desktop", name.to_string_lossy());
                    ui.label(name).on_hover_text(path);
                    return;
                }
                let file = Path::new(&self.config.screen_profile).file_name();
                let name = file.map_or("none, sRGB".into(), |n| n.to_string_lossy());
                ui.label(name).on_hover_text(&self.config.screen_profile);
                if ui.button(tr("Choose…")).clicked() {
                    self.pick(&ui.ctx().clone(), false, Pick::ScreenProfile);
                }
                if !self.config.screen_profile.is_empty() && ui.button(tr("None")).clicked() {
                    self.set_screen_profile(String::new());
                }
            });
            ui.label(tr("Rendering intent:"));
            let current = self.config.rendering_intent.clone();
            for (value, label, _) in INTENTS {
                if ui.radio(current == value, label).clicked() && current != value {
                    self.set_rendering_intent(value.to_owned());
                }
            }
        });
    }

    /// Preferences: file details, and saving and cleaning the shared thumbnail cache.
    pub(crate) fn cache_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        changed |= ui
            .checkbox(
                &mut c.file_details,
                tr("Show file size and date in the file list"),
            )
            .changed();
        let save = tr("Save new thumbnails to ~/.cache/thumbnails (shared with other programs)");
        if ui.checkbox(&mut c.save_thumbnails, save).changed() {
            self.save_thumbs.store(c.save_thumbnails, Ordering::Relaxed);
            changed = true;
        }
        // Geeqie's cache maintenance: thumbnails of files that are gone.
        if ui
            .button(tr("Remove thumbnails of deleted files"))
            .on_hover_text(tr(tr(
                "In ~/.cache/thumbnails; kept for disks that are not plugged in",
            )))
            .clicked()
        {
            let (tx, ctx) = (self.notices_tx.clone(), ctx.clone());
            let pointer = Working::new(&self.busy);
            std::thread::spawn(move || {
                let (removed, seen) = thumbs::clean();
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

    /// Preferences: the toolbar, folder path, search field, EXIF panel and full-screen info.
    pub(crate) fn panel_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        changed |= ui
            .checkbox(&mut c.toolbar, tr("Show the toolbar"))
            .changed();
        changed |= ui
            .checkbox(&mut c.path_bar, tr("Show the folder path"))
            .changed();
        if ui
            .checkbox(&mut c.search_bar, tr("Show the file name search"))
            .changed()
        {
            changed = true;
            if !c.search_bar {
                self.search.clear();
                self.update_search(ctx);
            }
        }
        ui.horizontal(|ui| {
            ui.label(tr("Toolbar icons:"));
            for (n, label) in [tr("Small"), tr("Medium"), tr("Large")]
                .into_iter()
                .enumerate()
            {
                changed |= ui
                    .radio_value(&mut self.config.toolbar_icons, n, label)
                    .changed();
            }
        });
        let c = &mut self.config;
        let exif = tr("Show the EXIF panel (Ctrl+K; reads a little from each image shown)");
        changed |= ui.checkbox(&mut c.exif_panel, exif).changed();
        let osd = tr("Show image info in full screen (I)");
        changed |= ui.checkbox(&mut c.osd, osd).changed();
        ui.label(tr("Image info template:"));
        // Kept on one line in the config file, with \n for each new line.
        let mut text = c.osd_template.replace("\\n", "\n");
        let field = egui::TextEdit::multiline(&mut text)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text(tr("%name%  %position%"));
        if ui.add_enabled(c.osd, field).changed() {
            c.osd_template = text.replace('\n', "\\n");
            changed = true;
        }
        ui.weak(
            "Fields: %name%, %path%, %position%, %dimensions%, %marks%, %keywords%, %comment%, \
             and each label of the EXIF panel, such as %Camera% or %ISO%. A | starts a part, \
             left out when its fields are empty.",
        );
        if ui.button(tr("Default")).clicked() {
            c.osd_template = OSD_TEMPLATE.to_owned();
            changed = true;
        }
        changed
    }

    /// Preferences: refreshing, the folder and window at start, remote control, the screen saver
    /// and the window title.
    pub(crate) fn window_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        changed |= ui
            .checkbox(
                &mut c.auto_refresh,
                tr("Refresh the folder when files change"),
            )
            .changed();
        changed |= ui
            .checkbox(&mut c.remember_dir, tr("Start in the last used folder"))
            .changed();
        let window = tr("Remember the window size and position");
        changed |= ui.checkbox(&mut c.remember_window, window).changed();
        let remote = tr("Listen for glimt --remote commands (after restart)");
        changed |= ui.checkbox(&mut c.remote_control, remote).changed();
        let inhibit = tr("Keep the screen saver off in full screen");
        changed |= ui.checkbox(&mut c.inhibit_screensaver, inhibit).changed();
        let order = "Window title \"Glimt - image name\" (off: \"image name - Glimt\")";
        if ui.checkbox(&mut c.app_name_first, order).changed() {
            changed = true;
            self.title(ctx);
        }
        changed
    }

    /// Preferences → Keyboard shortcuts: every action with a key, which can be changed (the next
    /// key press) or reset to Geeqie's.
    pub(crate) fn key_settings(&mut self, ui: &mut egui::Ui) {
        if let Some(action) = self.capturing {
            let pressed = ui.input(|i| {
                use egui::Key as K;
                i.events.iter().find_map(|e| match e {
                    // A modifier on its own is not the key; wait for the one pressed with it.
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
                    _ => None,
                })
            });
            if let Some((key, mods)) = pressed {
                self.capturing = None;
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
                        self.notice = Some(tf!(
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
                    if let Ok(mut own) = OWN_KEYS.write() {
                        own.clone_from(&self.config.keys.0);
                    }
                }
            }
        }
        let all = shortcuts();
        let mut actions: Vec<Action> = Vec::new();
        for s in SHORTCUTS {
            if !actions.contains(&s.0) && (s.0 != Action::Play || cfg!(feature = "video")) {
                actions.push(s.0);
            }
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
                if self.capturing == Some(action) {
                    ui.strong(tr("Press a key (Esc cancels)"));
                } else {
                    ui.label(keys.join(", "));
                }
                ui.horizontal(|ui| {
                    if ui.small_button(tr("Change")).clicked() {
                        self.capturing = Some(action);
                    }
                    let own = self.config.keys.0.iter().any(|k| k.0 == action);
                    if own && ui.small_button(tr("Reset")).clicked() {
                        self.config.keys.0.retain(|k| k.0 != action);
                        self.config.save();
                        if let Ok(mut o) = OWN_KEYS.write() {
                            o.clone_from(&self.config.keys.0);
                        }
                    }
                });
                ui.end_row();
            }
        });
    }

    /// Preferences: confirmations, the Delete key and the trash.
    pub(crate) fn delete_settings(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.label(tr("Delete behaviour:"));
        let c = &mut self.config;
        changed |= ui
            .checkbox(&mut c.confirm_delete, tr("Confirm permanent file delete"))
            .changed();
        changed |= ui
            .checkbox(&mut c.confirm_trash, tr("Confirm move file to Trash"))
            .changed();
        changed |= ui
            .checkbox(&mut c.delete_key, tr("Enable Delete key"))
            .changed();
        let mut no_trash = !c.trash;
        let none = tr("Use no trash at all (Delete deletes permanently)");
        if ui.checkbox(&mut no_trash, none).changed() {
            c.trash = !no_trash;
            changed = true;
        }
        changed
    }

    /// Preferences: videos in the list and the player.
    #[cfg(feature = "video")]
    pub(crate) fn video_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let c = &mut self.config;
        ui.separator();
        ui.label(tr("Video (needs ffmpeg for previews):"));
        if ui
            .checkbox(&mut c.videos, tr("Show videos in the file list"))
            .changed()
        {
            changed = true;
            self.list(ctx);
        }
        ui.horizontal(|ui| {
            ui.label(tr("Player (Enter):"));
            let player = egui::TextEdit::singleline(&mut self.config.video_player)
                .hint_text(tr("default application"));
            let hint = "A command line, such as mpv --fs; a path with spaces goes in \"quotes\"";
            changed |= ui.add(player).on_hover_text(hint).changed();
        });
        changed
    }
}
