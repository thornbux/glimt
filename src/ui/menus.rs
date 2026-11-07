//! The menu bar and toolbar, the menus that change settings, Plugins and Open With, and the helpers
//! every menu is drawn with.

use crate::files::config::xdg_dir;
use crate::files::external::{apps_for, command, open_url};
use crate::files::file_ops::OperationReport;
use crate::images::colour::{
    COLOR_MANAGEMENT, FILTERS, VIEW_FILTER, set_colour, set_input, set_intent, set_screen,
};
use crate::support::format::count;
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::{Action, shortcut_text};
use crate::{features::plugins, files::config, files::external, model::collections, tf, ui::theme};
use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Instant;

/// Where the popup IDs of the menu bar's menus are kept, for `scroll_menu` and `close_menus`.
pub(crate) const BAR_MENUS: &str = "menu bar menus";

/// Open With's applications: each one's name and desktop file.
type Apps = Vec<(String, PathBuf)>;

/// Open With's programs for each file type, read in the background.
#[derive(Default)]
pub(crate) struct OpenWith {
    /// The applications by file extension, or by the whole path for a file without one. An
    /// entry is asked for once in the background, and then kept with the time it came.
    apps: HashMap<String, Answer>,
}

/// One entry of `OpenWith`: the question still running, or its answer and when it came.
enum Answer {
    Asking(Receiving<Apps>),
    Found(Apps, Instant),
}

impl OpenWith {
    /// How long an empty answer is kept. gio may have timed out on a sleeping share, or the
    /// thread may have panicked, so the question is asked again after this.
    const RETRY: std::time::Duration = std::time::Duration::from_secs(60);

    /// Finds the applications for the file type `key` with `ask`, in a thread of its own the
    /// first time. Returns `None` while that thread runs.
    fn apps(
        &mut self,
        key: &str,
        ask: impl FnOnce() -> Apps + Send + 'static,
        ctx: &egui::Context,
    ) -> Option<&Apps> {
        let stale = match self.apps.get(key) {
            None => true,
            Some(Answer::Asking(_)) => false,
            Some(Answer::Found(apps, at)) => apps.is_empty() && at.elapsed() >= Self::RETRY,
        };
        if stale {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(ask());
                ctx.request_repaint();
            });
            self.apps
                .insert(key.to_owned(), Answer::Asking(Receiving::of(rx)));
        }
        let entry = self.apps.get_mut(key)?;
        if let Answer::Asking(rx) = entry {
            // A thread that panicked counts as an empty answer, which is asked again later.
            match rx.take() {
                Took::Nothing => return None,
                Took::Answer(apps) => *entry = Answer::Found(apps, Instant::now()),
                Took::Gone => *entry = Answer::Found(Vec::new(), Instant::now()),
            }
        }
        match entry {
            Answer::Found(apps, _) => Some(apps),
            Answer::Asking(_) => None,
        }
    }
}

impl crate::App {
    // ---- Settings and the menus that change them ----

    /// The chosen theme, read from its file again, with the desktop's look where it sets none;
    /// High Contrast while the desktop asks for it, unless that is turned off.
    pub(crate) fn apply_theme(&self, ctx: &egui::Context) {
        let look = self.desktop.look();
        let name = match look.high_contrast && self.config.follow_contrast {
            true => theme::HIGH_CONTRAST,
            false => &self.config.theme,
        };
        theme::apply(ctx, &theme::load(name), look.dark, look.accent);
        let desktop = look.text_scale.filter(|_| self.config.follow_text_scale);
        ctx.set_zoom_factor(self.config.ui_scale as f32 / 100.0 * desktop.unwrap_or(1.0));
    }

    /// View → Theme: the themes to choose from, then where to make and find more.
    pub(crate) fn theme_menu(&mut self, ui: &mut egui::Ui) {
        for name in theme::names() {
            let icon = match name.as_str() {
                "Dark" => theme::icon::MOON,
                "Light" => theme::icon::SUN,
                "Desktop" => theme::icon::MONITOR,
                "Nord" => theme::icon::SNOWFLAKE,
                theme::HIGH_CONTRAST => theme::icon::HALF,
                "Contrast" => theme::icon::HALF_TILT,
                _ => theme::icon::BRUSH,
            };
            // Chosen again too, so a theme file just edited shows.
            let shown = theme::shown_name(&name);
            #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted below.
            let radio = ui.radio(self.config.theme == name, iconed(ui, icon, shown));
            if noted(&radio, &name).clicked() {
                self.config.theme = name;
                self.config.save();
                self.apply_theme(ui.ctx());
            }
        }
        // Locked: no theme written, and no file manager or browser opened from here.
        if !config::locked() {
            ui.separator();
            self.theme_buttons(ui);
        }
    }

    /// Save as Theme…, Open Themes Folder and Get More Themes…, in View → Theme and Preferences.
    pub(crate) fn theme_buttons(&mut self, ui: &mut egui::Ui) {
        if button(ui, tr("Save as Theme…")).clicked() {
            self.dialogs.ask_theme_name();
        }
        if button(ui, tr("Open Themes Folder")).clicked()
            && let Some(dir) = theme::dir()
        {
            let _ = std::fs::create_dir_all(&dir);
            open_url(&dir.to_string_lossy());
        }
        if button(ui, tr("Get More Themes…")).clicked() {
            open_url("https://github.com/thornbux/glimt/tree/main/themes");
        }
    }

    /// Preload `n` images on each side.
    pub(crate) fn set_preload(&mut self, n: usize) {
        self.decoding.set_preload(n);
        self.refill_preload();
        self.config.preload = n;
    }

    /// Decode everything again with or without EXIF rotation.
    pub(crate) fn set_exif_rotate(&mut self, on: bool) {
        self.config.exif_rotate = on;
        self.config.save();
        self.decoding.set_auto_rotate(on);
        // The thumbnails too, which are turned by it as well.
        self.redecode();
    }

    /// Decode everything again through another view filter.
    pub(crate) fn set_view_filter(&mut self, f: usize) {
        self.config.view_filter = f;
        self.config.save();
        VIEW_FILTER.store(f, Ordering::Relaxed);
        self.redecode();
    }

    /// Decode everything again with or without colour management.
    pub(crate) fn set_color_management(&mut self, on: bool) {
        self.config.color_management = on;
        self.config.save();
        COLOR_MANAGEMENT.store(on, Ordering::Relaxed);
        self.redecode();
    }

    /// Show everything through the screen profile in `path` (empty: none, the screen is sRGB).
    pub(crate) fn set_screen_profile(&mut self, path: String) {
        if let Err(e) = set_screen(&path) {
            self.notice = Some(e);
            return;
        }
        self.config.screen_profile = path;
        self.config.save();
        self.use_screen();
    }

    /// Whether the desktop's screen profile goes before the chosen one; read from colord when on.
    pub(crate) fn set_desktop_profile(&mut self, on: bool, ctx: &egui::Context) {
        self.config.desktop_profile = on;
        self.config.save();
        if on {
            self.desktop.ask_screen(ctx);
        }
        self.use_screen();
    }

    /// Show through the desktop's screen profile when it counts and colord has one, else through
    /// the chosen one.
    pub(crate) fn use_screen(&mut self) {
        let path = match self.desktop.screen() {
            Some(p) if self.config.desktop_profile => p.to_owned(),
            _ => self.config.screen_profile.clone(),
        };
        match set_screen(&path) {
            Ok(false) => {}
            Ok(true) => self.redecode(),
            Err(e) => self.notice = Some(e),
        }
    }

    /// The profile for images without one: srgb, adobe, p3 or an ICC file.
    pub(crate) fn set_input_profile(&mut self, name: String) {
        if let Err(e) = set_input(&name) {
            self.notice = Some(e);
            return;
        }
        self.config.input_profile = name;
        self.config.save();
        self.redecode();
    }

    /// Whether an image's own profile counts, or the input profile is used for all.
    pub(crate) fn set_own_profile(&mut self, on: bool) {
        set_colour(|c| c.use_image_profile = on);
        self.config.use_image_profile = on;
        self.config.save();
        self.redecode();
    }

    /// The rendering intent, from `INTENTS`.
    pub(crate) fn set_rendering_intent(&mut self, name: String) {
        set_intent(&name);
        self.config.rendering_intent = name;
        self.config.save();
        self.redecode();
    }

    /// The menu bar, as Geeqie's, and the toolbar under it.
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let bar = egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                scroll_menu(ui, tr("File"), |ui| self.menu_file(ui, act));
                scroll_menu(ui, tr("Go"), |ui| self.menu_go(ui, act));
                scroll_menu(ui, tr("Select"), |ui| self.menu_select(ui, act));
                scroll_menu(ui, tr("Edit"), |ui| self.menu_edit(ui, act));
                scroll_menu(ui, tr("View"), |ui| self.menu_view(ui, act));
                scroll_menu(ui, tr("Window"), |ui| self.menu_window(ui, act));
                scroll_menu(ui, tr("Help"), |ui| self.menu_help(ui, act));
            });
        });
        self.window.close_menus(ui.ctx(), bar.response.rect);
        if self.config.toolbar {
            self.toolbar(ui, act);
        }
    }

    /// The File menu.
    fn menu_file(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::OpenFile);
        menu_item(ui, act, Action::OpenFolder);
        menu_item(ui, act, Action::OpenRecursive);
        menu_item(ui, act, Action::OpenFolders);
        self.recent_menu(ui);
        menu_item(ui, act, Action::NewFolder);
        menu_item(ui, act, Action::CopyTo);
        menu_item(ui, act, Action::MoveTo);
        menu_item(ui, act, Action::Rename);
        menu_item(ui, act, Action::OpenWith);
        self.open_with_menu(ui);
        menu_item(ui, act, Action::SetWallpaper);
        #[cfg(feature = "video")]
        menu_item(ui, act, Action::PlayVideo);
        menu_item(ui, act, Action::SaveAs);
        menu_item(ui, act, Action::Print);
        // Only for a file of several frames or pages: the menu stays as it was
        // for every other one.
        if self.frame_count() > 1 {
            menu_item(ui, act, Action::SaveFrame);
            menu_item(ui, act, Action::ExportFrames);
        }
        ui.separator();
        menu_item(ui, act, Action::Delete);
        menu_item(ui, act, Action::DeletePermanently);
        ui.separator();
        self.collections_menu(ui, act);
        menu_item(ui, act, Action::FindDuplicates);
        menu_item(ui, act, Action::CheckImages);
        menu_item(ui, act, Action::BackUp);
        ui.separator();
        menu_item(ui, act, Action::CloseWindow);
        menu_item(ui, act, Action::Quit);
    }

    /// The Go menu.
    fn menu_go(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::First);
        menu_item(ui, act, Action::Previous);
        menu_item(ui, act, Action::Next);
        menu_item(ui, act, Action::Last);
        ui.separator();
        menu_item(ui, act, Action::Back);
        menu_item(ui, act, Action::Forward);
        menu_item(ui, act, Action::Home);
        menu_item(ui, act, Action::Up);
        ui.separator();
        menu_item(ui, act, Action::FocusSearch);
    }

    /// The Select menu.
    fn menu_select(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::SelectAll);
        menu_item(ui, act, Action::SelectNone);
        menu_item(ui, act, Action::InvertSelection);
        ui.separator();
        menu_item(ui, act, Action::CopyPath);
        menu_item(ui, act, Action::CopyPathUnquoted);
        menu_item(ui, act, Action::CopyImage);
        menu_item(ui, act, Action::CopyFiles);
    }

    /// The Edit menu.
    fn menu_edit(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::Undo);
        ui.separator();
        #[cfg(feature = "marks")]
        scroll_menu(
            ui,
            iconed(ui, theme::icon::STAR, tr("Marks and Stars")),
            |ui| {
                for action in [
                    Action::Mark1,
                    Action::Mark2,
                    Action::Mark3,
                    Action::Mark4,
                    Action::Mark5,
                    Action::Mark6,
                ] {
                    menu_item(ui, act, action);
                }
                ui.separator();
                for action in [
                    Action::SelectMark1,
                    Action::SelectMark2,
                    Action::SelectMark3,
                    Action::SelectMark4,
                    Action::SelectMark5,
                    Action::SelectMark6,
                ] {
                    menu_item(ui, act, action);
                }
                ui.separator();
                for action in [
                    Action::Rate0,
                    Action::Rate1,
                    Action::Rate2,
                    Action::Rate3,
                    Action::Rate4,
                    Action::Rate5,
                ] {
                    menu_item(ui, act, action);
                }
            },
        );
        scroll_menu(
            ui,
            iconed(ui, theme::icon::ORIENTATION, tr("Orientation")),
            |ui| {
                for action in [
                    Action::RotateCw,
                    Action::RotateCcw,
                    Action::Rotate180,
                    Action::Mirror,
                    Action::Flip,
                ] {
                    menu_item(ui, act, action);
                }
                ui.separator();
                menu_item(ui, act, Action::SaveOrientation);
            },
        );
        menu_item(ui, act, Action::Crop);
        menu_item(ui, act, Action::FocusKeywords);
        ui.separator();
        self.plugins_menu(ui);
        ui.separator();
        menu_item(ui, act, Action::Preferences);
    }

    /// The View menu: layout, theme, colour, filter and zoom, the panels, and the slideshow.
    fn menu_view(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        self.layout_menu(ui, act);
        scroll_menu(ui, iconed(ui, theme::icon::PALETTE, tr("Theme")), |ui| {
            self.theme_menu(ui)
        });
        scroll_menu(
            ui,
            iconed(ui, theme::icon::COLOUR, tr("Colour Management")),
            |ui| self.colour_settings(ui),
        );
        scroll_menu(ui, iconed(ui, theme::icon::FILTER, tr("Filter")), |ui| {
            use theme::icon::{CIRCLE, HALF, HALF_TILT, SUN_DIM};
            let icons = [CIRCLE, HALF, HALF_TILT, SUN_DIM];
            for (f, label) in FILTERS.iter().enumerate() {
                let text = iconed(ui, icons[f.min(3)], tr(label));
                #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted below.
                let radio = ui.radio(self.config.view_filter == f, text);
                if noted(&radio, tr(label)).clicked() {
                    self.set_view_filter(f);
                }
            }
            ui.separator();
            menu_item(ui, act, Action::NextFilter);
        });
        scroll_menu(ui, iconed(ui, theme::icon::ZOOM, tr("Zoom")), |ui| {
            menu_item(ui, act, Action::ZoomIn);
            menu_item(ui, act, Action::ZoomOut);
            menu_item(ui, act, Action::Zoom1To1);
            let mode = self.config.fit_mode;
            menu_toggle(ui, act, mode == 0, Action::ZoomFit);
            menu_toggle(ui, act, mode == 1, Action::FitWidth);
            menu_toggle(ui, act, mode == 2, Action::FitHeight);
            menu_toggle(ui, act, mode == 3, Action::FitFill);
        });
        menu_item(ui, act, Action::FullScreen);
        menu_item(ui, act, Action::HideFileList);
        let c = &self.config;
        menu_toggle(ui, act, c.toolbar, Action::Toolbar);
        menu_toggle(ui, act, c.path_bar, Action::PathBar);
        menu_toggle(ui, act, c.folder_tree, Action::FolderTree);
        menu_toggle(ui, act, c.show_subfolders, Action::Subfolders);
        menu_toggle(ui, act, c.show_hidden, Action::HiddenFiles);
        menu_toggle(ui, act, c.thumbnails, Action::Thumbnails);
        menu_toggle(ui, act, c.icons, Action::Icons);
        // Ticked only while the grid shows: from an image opened from it, the item goes back to
        // the grid, as G and Esc do.
        let grid = self.panel.area_grid_shown();
        menu_toggle(ui, act, grid, Action::ThumbnailGrid);
        let size = iconed(ui, theme::icon::SIZE, tr("Thumbnail Size"));
        submenu(ui, size, |ui| self.thumb_size_setting(ui));
        let c = &self.config;
        menu_toggle(ui, act, c.grid_names, Action::GridNames);
        menu_toggle(ui, act, c.info_panel, Action::InfoPanel);
        menu_toggle(ui, act, c.sort_manager, Action::SortManager);
        menu_toggle(
            ui,
            act,
            self.info_panel.exif_window_open(),
            Action::ExifWindow,
        );
        menu_toggle(ui, act, self.pan_view.is_some(), Action::PanView);
        menu_toggle(ui, act, c.osd, Action::FullScreenInfo);
        menu_item(ui, act, Action::Histogram);
        menu_toggle(ui, act, c.histogram_log, Action::HistogramLog);
        menu_item(ui, act, Action::Refresh);
        ui.separator();
        let (icon, label) = if self.slideshow.is_running() {
            (theme::icon::STOP, tr("Stop Slideshow"))
        } else {
            (theme::icon::PLAY, tr("Start Slideshow"))
        };
        let text = iconed(ui, icon, label);
        #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
        menu_button(ui, act, egui::Button::new(text), Action::Slideshow);
        menu_toggle(ui, act, self.slideshow.is_paused(), Action::Pause);
        menu_item(ui, act, Action::SlideshowFaster);
        menu_item(ui, act, Action::SlideshowSlower);
        menu_toggle(ui, act, self.config.slideshow_random, Action::RandomOrder);
    }

    /// The Window menu.
    fn menu_window(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::NewWindow);
        menu_item(ui, act, Action::CloseWindow);
        menu_item(ui, act, Action::WindowTitle);
        ui.separator();
        menu_toggle(ui, act, self.window.has_projector(), Action::Projector);
        let pointer = self.config.projector_pointer;
        menu_toggle(ui, act, pointer, Action::ProjectorPointer);
    }

    /// The Help menu.
    fn menu_help(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        menu_item(ui, act, Action::Help);
        menu_item(ui, act, Action::Shortcuts);
        menu_item(ui, act, Action::KeyboardMap);
        ui.separator();
        menu_item(ui, act, Action::About);
    }

    /// A file's right-click menu; it acts on the selection.
    pub(crate) fn file_menu(&mut self, ui: &mut egui::Ui, list_act: &mut Option<Action>) {
        for action in [
            Action::CopyTo,
            Action::MoveTo,
            Action::CopyPath,
            Action::CopyFiles,
        ] {
            menu_item(ui, list_act, action);
        }
        self.open_with_menu(ui);
        scroll_menu(
            ui,
            iconed(ui, theme::icon::STACK_PLUS, tr("Add to Collection")),
            |ui| {
                for (name, file) in collections::saved_collections() {
                    if icon_button(ui, theme::icon::STACK, &name).clicked() {
                        self.add_to_collection(ui.ctx(), &file);
                    }
                }
                menu_item(ui, list_act, Action::NewCollection);
            },
        );
        if self.folder.collection().is_some() {
            menu_item(ui, list_act, Action::RemoveFromCollection);
        }
        ui.separator();
        menu_item(ui, list_act, Action::Delete);
        menu_item(ui, list_act, Action::DeletePermanently);
    }

    /// Edit → Plugins: run one on the selected files, or the shown one, in the background;
    /// they are read again afterwards, as plugins change them.
    pub(crate) fn plugins_menu(&mut self, ui: &mut egui::Ui) {
        scroll_menu(ui, iconed(ui, theme::icon::PLUGIN, tr("Plugins")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let dir = xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("plugins"));
            if self.plugins.is_empty() {
                ui.weak(tr("No plugins"));
            }
            let mut chosen = None;
            for (k, plugin) in self.plugins.iter().enumerate() {
                if icon_button(ui, theme::icon::PLUGIN, &plugin.name).clicked() {
                    chosen = Some(k);
                }
            }
            // A plugin that is still running, an editor left open or a script that hangs: ended
            // here, with what it started, in the background (it is given a second).
            let running = self.plugins_running.lock().unwrap().clone();
            if !running.is_empty() {
                ui.separator();
            }
            for (name, pid) in running {
                let text = tf!("Stop {name}", name);
                if icon_button(ui, theme::icon::STOP, &text).clicked() {
                    let mut running = self.plugins_running.lock().unwrap();
                    let was = running.len();
                    running.retain(|r| r.1 != pid);
                    // Not when it ended meanwhile: its number may be another's by now.
                    if running.len() < was {
                        std::thread::spawn(move || external::stop_process_group(pid));
                    }
                }
            }
            ui.separator();
            if icon_button(ui, theme::icon::FOLDER_OPEN, tr("Open the Plugins Folder"))
                .on_hover_text(tr(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                ))
                .clicked()
                && let Some(dir) = dir
            {
                let _ = std::fs::create_dir_all(&dir);
                open_url(&dir.to_string_lossy());
            }
            let Some(k) = chosen.filter(|_| !self.list.files().is_empty()) else {
                return;
            };
            let Some(targets) = self.targets() else {
                return;
            };
            let plugin = self.plugins[k].clone();
            let (files, left): (Vec<PathBuf>, Vec<PathBuf>) =
                (targets.iter().map(|&i| self.path(i))).partition(|p| plugin.accepts(p));
            let name = &plugin.name;
            if files.is_empty() {
                self.notice = Some(tf!("{name} takes none of these files", name));
                return;
            }
            if !left.is_empty() {
                let files = count(left.len(), "file");
                self.notice = Some(tf!(
                    "{name} leaves out {files} of a type it does not take",
                    name,
                    files
                ));
            }
            // A turned image still being saved would be written over the plugin's edit. The
            // files are held while the plugin runs, so a delete, move or rename of them waits.
            let Some(held_back) = self.hold_back_while_writing(files.clone()) else {
                return;
            };
            let (dir, running) = (
                self.folder.dir().to_path_buf(),
                self.plugins_running.clone(),
            );
            let report_tx = self.file_operations.report_sender();
            let (tx, ctx) = (self.notice_sender.clone(), ui.ctx().clone());
            // A plugin may be an editor that stays open for hours: not waited for at closing,
            // and no busy pointer meanwhile, as for Open With.
            std::thread::spawn(move || {
                let result = plugin.run(&files, &dir, &running);
                drop(held_back);
                let _ = report_tx.send(OperationReport {
                    read_folder_again: true,
                    changed: files,
                    ..Default::default()
                });
                if let Err(e) = result {
                    let _ = tx.send(format!("{}: {e}", plugin.name));
                }
                ctx.request_repaint();
            });
        });
    }

    /// The Open With submenu. It lists the applications registered for the type of the first
    /// selected file, or of the shown one, and the chosen one opens the selected files (or the
    /// shown one). The right-click menu selects the file it was opened on, so it is that type.
    pub(crate) fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        let first = self.first_target();
        let Some(name) = self.list.files().get(first) else {
            return;
        };
        let ext = Path::new(name)
            .extension()
            .map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
        let path = self.path(first);
        scroll_menu(ui, iconed(ui, theme::icon::APP, tr("Open With")), |ui| {
            // The menu keeps the width of its first frame, "…": names must not wrap to it.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            // Files without an extension share no type, so each is asked about on its own.
            let key = match ext.is_empty() {
                true => path.to_string_lossy().into_owned(),
                false => ext,
            };
            // apps_for runs gio twice: in the background, so the menu never stalls.
            let ask = move || apps_for(&path);
            let Some(apps) = self.open_with.apps(&key, ask, ui.ctx()) else {
                ui.label("…");
                return;
            };
            #[cfg(feature = "portals")]
            if apps.is_empty() && crate::files::external::in_flatpak() {
                // The Flatpak, without the host's applications: the desktop's own chooser.
                if icon_button(ui, theme::icon::APP, tr("Choose Application…")).clicked()
                    && let Some(targets) = self.targets()
                {
                    // Each selected file (or the shown one), the chooser asking for each.
                    let files: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
                    let tx = self.notice_sender.clone();
                    std::thread::spawn(move || {
                        // Cancel ends them all, not only the one asked about.
                        for path in files {
                            match crate::files::portal::open_with(&path) {
                                Ok(true) => {}
                                Ok(false) => break,
                                Err(e) => drop(tx.send(e)),
                            }
                        }
                    });
                }
                return;
            }
            if apps.is_empty() {
                ui.label(tr("No applications found"));
            }
            let mut chosen = None;
            for (name, desktop) in apps.iter() {
                if icon_button(ui, theme::icon::APP, name).clicked() {
                    chosen = Some(desktop.clone());
                }
            }
            if let Some(desktop) = chosen
                && let Some(targets) = self.targets()
            {
                let files: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
                let tx = self.notice_sender.clone();
                // The chosen application may stay open, so it is waited for in a thread of its own.
                std::thread::spawn(move || {
                    let mut gio = external::host_command("gio");
                    gio.arg("launch").arg(&desktop).args(&files);
                    if let Err(told) = external::run_telling("gio", gio) {
                        let _ = tx.send(told);
                    }
                });
            }
        });
    }

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    pub(crate) fn launch(&self, player: &str) {
        if self.list.files().is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.list.shown()), self.notice_sender.clone());
        let mut words = plugins::command_words(player);
        std::thread::spawn(move || {
            // The player stays open as long as it plays, waited for in this thread.
            let run = |name: &str, mut cmd: std::process::Command| {
                cmd.arg(&path);
                external::run_telling(name, cmd)
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it. In a Flatpak both
                // ask the portal for the default application.
                match external::has_program("gio") {
                    true => run("gio", {
                        let mut gio = command("gio");
                        gio.arg("open");
                        gio
                    }),
                    false => run("xdg-open", command("xdg-open")),
                }
            } else {
                // The user's player, on the host in a Flatpak.
                let name = words.remove(0);
                let mut player = external::host_command(&name);
                player.args(&words);
                run(&name, player)
            };
            if let Err(told) = out {
                let _ = tx.send(told);
            }
        });
    }

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow and preferences.
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui, act: &mut Option<Action>) {
        let size = [14.0, 20.0, 28.0][self.config.toolbar_icons.min(2)];
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(size));
            ui.horizontal(|ui| {
                // The icons the menus show for the same actions.
                for action in [Action::Back, Action::Forward, Action::Up, Action::Home] {
                    tool_button(ui, act, action.icon(), action, false);
                }
                tool_button(ui, act, Action::Refresh.icon(), Action::Refresh, false);
                ui.separator();
                tool_button(ui, act, Action::ZoomIn.icon(), Action::ZoomIn, false);
                tool_button(ui, act, Action::ZoomOut.icon(), Action::ZoomOut, false);
                tool_button(ui, act, "1:1", Action::Zoom1To1, false);
                let fit = self.zoom.is_fitted();
                tool_button(ui, act, Action::ZoomFit.icon(), Action::ZoomFit, fit);
                ui.separator();
                let thumbs = self.config.thumbnails;
                tool_button(
                    ui,
                    act,
                    Action::Thumbnails.icon(),
                    Action::Thumbnails,
                    thumbs,
                );
                let show = if self.slideshow.is_running() {
                    theme::icon::STOP
                } else {
                    theme::icon::PLAY
                };
                tool_button(
                    ui,
                    act,
                    show,
                    Action::Slideshow,
                    self.slideshow.is_running(),
                );
                tool_button(
                    ui,
                    act,
                    Action::Preferences.icon(),
                    Action::Preferences,
                    false,
                );
            });
        });
    }

    /// The View menu's toggles of a setting: the path bar, toolbar, panels, overlay, icons,
    /// histogram scale and animation. A stepped through an animation plays it on instead.
    pub(crate) fn toggle_setting(&mut self, action: Action) {
        self.panel.scroll_to_shown();
        let c = &mut self.config;
        let flag = match action {
            Action::PathBar => &mut c.path_bar,
            Action::Toolbar => &mut c.toolbar,
            Action::FullScreenInfo => &mut c.osd,
            Action::Icons => &mut c.icons,
            Action::FolderTree => &mut c.folder_tree,
            Action::HistogramLog => &mut c.histogram_log,
            Action::Animate => {
                // Stepped to a frame: A plays on from there, without turning it off.
                if self.viewing.play_on() {
                    return;
                }
                &mut c.animate
            }
            Action::SortManager => &mut c.sort_manager,
            Action::InfoPanel => &mut c.info_panel,
            // Another action has no setting here, and must not flip one.
            _ => return,
        };
        *flag = !*flag;
        c.save();
    }
}

/// A menu that scrolls when it is taller than the window, instead of running off it.
pub(crate) fn scroll_menu<R>(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let title: egui::WidgetText = title.into();
    let name = title.text().to_owned();
    let content = |ui: &mut egui::Ui| {
        // From where it opens to the bottom of the window; submenus open further down. egui
        // limits a menu to its size the frame before, which a scroll area could only keep or
        // shrink: the limit is set here instead.
        let margin = ui.spacing().menu_margin.bottom as f32 + ui.spacing().item_spacing.y;
        let max = ui.ctx().content_rect().bottom() - ui.next_widget_position().y - margin;
        ui.set_max_height(max);
        egui::ScrollArea::vertical().max_height(max).show(ui, add);
    };
    if egui::containers::menu::is_in_menu(ui) {
        submenu(ui, title, content);
        return;
    }
    // A menu of the menu bar. While one of them is open, pointing at another opens that one,
    // as in GNOME and KDE.
    #[allow(clippy::disallowed_methods)] // The one place a menu of the bar is drawn, noted below.
    let response = egui::containers::menu::MenuButton::new(title)
        .ui(ui, content)
        .0;
    noted(&response, &name);
    let ctx = ui.ctx().clone();
    let id = egui::Popup::default_response_id(&response);
    let others = ctx.data_mut(|d| {
        let ids = d.get_temp_mut_or_default::<Vec<egui::Id>>(egui::Id::unique(BAR_MENUS));
        if !ids.contains(&id) {
            ids.push(id);
        }
        ids.clone()
    });
    let other_open = others
        .iter()
        .any(|&o| o != id && egui::Popup::is_id_open(&ctx, o));
    if response.hovered() && other_open {
        egui::Popup::open_id(&ctx, id);
        ctx.request_repaint();
    }
}

/// Close an open menu of the menu bar once the pointer has moved away from the bar and every
/// open menu for a moment (`away` holds since when). A menu opened from the keyboard stays
/// while the mouse is still.
pub(crate) fn close_menus(ctx: &egui::Context, bar: egui::Rect, away: &mut Option<Instant>) {
    let ids: Vec<egui::Id> =
        ctx.data(|d| d.get_temp(egui::Id::unique(BAR_MENUS)).unwrap_or_default());
    if !ids.iter().any(|&id| egui::Popup::is_id_open(ctx, id)) {
        *away = None;
        return;
    }
    let pointer = ctx.pointer_hover_pos();
    // Over the bar, or over a menu: a layer of its own above the panels.
    let over = pointer.is_some_and(|p| {
        bar.contains(p)
            || ctx
                .layer_id_at(p)
                .is_some_and(|l| l.order != egui::Order::Background)
    });
    if over {
        *away = None;
    } else if away.is_none()
        && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || pointer.is_none())
    {
        *away = Some(Instant::now());
    }
    if let Some(since) = *away {
        let wait = std::time::Duration::from_millis(400);
        match wait.checked_sub(since.elapsed()) {
            Some(left) => ctx.request_repaint_after(left),
            None => {
                for &id in &ids {
                    egui::Popup::close_id(ctx, id);
                }
                *away = None;
                ctx.request_repaint();
            }
        }
    }
}

/// Draws a button of `text` and notes it for the release tests, which click it by its text.
#[allow(clippy::disallowed_methods)] // The one place a plain button is drawn.
pub(crate) fn button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    noted(&ui.button(text), text)
}

/// Draws a button of `icon` before `text`, noted by its text as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a button with an icon is drawn.
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: &str, text: &str) -> egui::Response {
    noted(&ui.button(iconed(ui, icon, text)), text)
}

/// Draws a button of `text` that can be clicked only when `on`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a button that may be off is drawn.
pub(crate) fn button_enabled(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    noted(&ui.add_enabled(on, egui::Button::new(text)), text)
}

/// Draws a small button of `text`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a small button is drawn.
pub(crate) fn small_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    noted(&ui.small_button(text), text)
}

/// Draws a row of `text` that shows whether it is selected (`on`), noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a selectable row is drawn.
pub(crate) fn selectable(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    noted(&ui.selectable_label(on, text), text)
}

/// Draws a row of `text` that sets `current` to `value` when clicked and shows whether it
/// holds it, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a row tied to a value is drawn.
pub(crate) fn choice<T: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    value: T,
    text: &str,
) -> egui::Response {
    noted(&ui.selectable_value(current, value, text), text)
}

/// Draws a radio button of `text`, on when `on`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a radio button is drawn by its text.
pub(crate) fn radio(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    noted(&ui.radio(on, text), text)
}

/// Draws a radio button of `text` that sets `current` to `value` when clicked, noted as
/// `button` does. The response is marked changed when `current` changes, as egui's own
/// radio_value does.
pub(crate) fn radio_choice<T: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    value: T,
    text: &str,
) -> egui::Response {
    let mut response = radio(ui, *current == value, text);
    if response.clicked() && *current != value {
        *current = value;
        response.mark_changed();
    }
    response
}

/// Draws a checkbox of `text` that turns `on` over when clicked, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place that draws egui's own checkbox.
pub(crate) fn checkbox(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    noted(&ui.checkbox(on, text), text)
}

/// Draws a link of `text`, noted as `button` does. The caller opens what it points to.
#[allow(clippy::disallowed_methods)] // The one place a link is drawn.
pub(crate) fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    noted(&ui.link(text), text)
}

/// Draws a button of `title` that opens a submenu drawn by `add`, noted by its title as
/// `button` does.
#[allow(clippy::disallowed_methods)] // The one place egui's own menu button is drawn.
pub(crate) fn submenu<R>(
    ui: &mut egui::Ui,
    title: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let title: egui::WidgetText = title.into();
    let name = title.text().to_owned();
    noted(&ui.menu_button(title, add).response, &name);
}

/// `text` with the Phosphor `icon` before it, as every row of the menus shows: the icon in the
/// weak colour, the text in the widget's own (Color32::PLACEHOLDER), so a selected row keeps
/// its colours. Phosphor's glyphs are all one em wide, so the texts line up.
pub(crate) fn iconed(ui: &egui::Ui, icon: &str, text: &str) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    job.append(icon, 0.0, format(ui.visuals().weak_text_color()));
    job.append(text, 8.0, format(egui::Color32::PLACEHOLDER));
    job
}

/// A menu entry showing the action's shortcut; sets `act` when clicked.
pub(crate) fn menu_item(ui: &mut egui::Ui, act: &mut Option<Action>, action: Action) {
    let text = iconed(ui, action.icon(), action.label());
    #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
    menu_button(ui, act, egui::Button::new(text), action);
}

/// A menu entry with a check mark when `on`.
pub(crate) fn menu_toggle(ui: &mut egui::Ui, act: &mut Option<Action>, on: bool, action: Action) {
    let mut text = iconed(ui, action.icon(), action.label());
    // On the accent colour of a row that is on, the weak grey would hardly show.
    if on {
        text.sections[0].format.color = egui::Color32::PLACEHOLDER;
    }
    #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
    let button = egui::Button::selectable(on, text);
    menu_button(ui, act, button, action);
}

pub(crate) fn menu_button(
    ui: &mut egui::Ui,
    act: &mut Option<Action>,
    mut button: egui::Button,
    action: Action,
) {
    if let Some(text) = shortcut_text(ui.ctx(), action) {
        button = button.shortcut_text(text);
    }
    if noted(&ui.add(button), action.label()).clicked() {
        *act = Some(action);
    }
}

/// A toolbar button with the action's name and shortcut as tooltip.
pub(crate) fn tool_button(
    ui: &mut egui::Ui,
    act: &mut Option<Action>,
    icon: &str,
    action: Action,
    on: bool,
) {
    let tip = match shortcut_text(ui.ctx(), action) {
        Some(keys) => format!("{label} ({keys})", label = action.label()),
        None => action.label().to_owned(),
    };
    #[allow(clippy::disallowed_methods)] // An icon alone, noted below by the action's label.
    let button = egui::Button::selectable(on, icon);
    let response = ui.add(button).on_hover_text(tip);
    if noted(&response, action.label()).clicked() {
        *act = Some(action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits for `open_with`'s answer for `key`, asking with `ask` if it asks again.
    fn answer(open_with: &mut OpenWith, key: &str, ask: fn() -> Apps) -> Apps {
        let ctx = egui::Context::default();
        for _ in 0..2000 {
            if let Some(apps) = open_with.apps(key, ask, &ctx) {
                return apps.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("no answer for {key}");
    }

    #[test]
    fn open_with_asks_again_after_a_panic_or_an_empty_answer() {
        let one = || vec![("Viewer".to_owned(), PathBuf::from("viewer.desktop"))];
        let mut open_with = OpenWith::default();
        // A thread that panicked gives an empty answer, not "…" for good.
        assert!(answer(&mut open_with, "jpg", || panic!("gio broke")).is_empty());
        // Within RETRY the empty answer stands.
        assert!(answer(&mut open_with, "jpg", one).is_empty());
        // After it, the type is asked about again.
        // A computer started less than RETRY ago has no instant that long ago to stand for it.
        let Some(old) = Instant::now().checked_sub(OpenWith::RETRY) else {
            return;
        };
        open_with
            .apps
            .insert("jpg".into(), Answer::Found(Vec::new(), old));
        assert_eq!(answer(&mut open_with, "jpg", one), one());
        // A found application is kept, and not asked about again.
        assert_eq!(answer(&mut open_with, "jpg", Vec::new), one());
    }

    /// An action that is not a View toggle changes no setting. Any action it did not know
    /// turned the info panel on or off.
    #[test]
    fn only_a_view_toggle_changes_its_setting() {
        let dir = std::env::temp_dir().join(format!("glimt-test-toggle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _ctx) = crate::support::test_support::app_on(&dir);
        let panel = app.config.info_panel;
        app.toggle_setting(Action::Next);
        assert_eq!(app.config.info_panel, panel);
        app.toggle_setting(Action::InfoPanel);
        assert_eq!(app.config.info_panel, !panel);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
