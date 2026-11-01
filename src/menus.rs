//! The menu bar and toolbar, the menus that change settings, Plugins and Open With, and the helpers every menu is drawn with.

use crate::Action;
use crate::COLOR_MANAGEMENT;
use crate::Deleted;
use crate::FILTERS;
use crate::Prompt;
use crate::Receiving;
use crate::VIEW_FILTER;
use crate::apps_for;
use crate::collections;
use crate::command;
use crate::config;
use crate::external;
use crate::noted;
use crate::open_url;
use crate::plugins;
use crate::read_desktop_screen;
use crate::set_colour;
use crate::set_input;
use crate::set_intent;
use crate::set_screen;
use crate::shortcut_text;
use crate::tf;
use crate::theme;
use crate::tr;
use crate::xdg_dir;
use eframe::egui;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;
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
        noted(&ui.menu_button(title, content).response, &name);
        return;
    }
    // A menu of the menu bar. While one of them is open, pointing at another opens that one,
    // as in GNOME and KDE.
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

/// Where the popup IDs of the menu bar's menus are kept, for `scroll_menu` and `close_menus`.
pub(crate) const BAR_MENUS: &str = "menu bar menus";

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
    menu_button(ui, act, egui::Button::new(text), action);
}

/// A menu entry with a check mark when `on`.
pub(crate) fn menu_toggle(ui: &mut egui::Ui, act: &mut Option<Action>, on: bool, action: Action) {
    let mut text = iconed(ui, action.icon(), action.label());
    // On the accent colour of a row that is on, the weak grey would hardly show.
    if on {
        text.sections[0].format.color = egui::Color32::PLACEHOLDER;
    }
    menu_button(ui, act, egui::Button::selectable(on, text), action);
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
    let response = ui
        .add(egui::Button::selectable(on, icon))
        .on_hover_text(tip);
    if noted(&response, action.label()).clicked() {
        *act = Some(action);
    }
}

impl crate::App {
    // ---- Settings and the menus that change them ----

    /// The chosen theme, read from its file again, with the desktop's look where it sets none;
    /// High Contrast while the desktop asks for it, unless that is turned off.
    pub(crate) fn apply_theme(&self, ctx: &egui::Context) {
        let look = self.desktop_look;
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
                "High Contrast" => theme::icon::HALF,
                "Contrast" => theme::icon::HALF_TILT,
                _ => theme::icon::BRUSH,
            };
            // Chosen again too, so a theme file just edited shows.
            let radio = ui.radio(self.config.theme == name, iconed(ui, icon, &name));
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
        if ui.button(tr("Save as Theme…")).clicked() {
            self.prompt = Some((Prompt::SaveTheme, String::new()));
        }
        if ui.button(tr("Open Themes Folder")).clicked()
            && let Some(dir) = theme::dir()
        {
            let _ = std::fs::create_dir_all(&dir);
            open_url(&dir.to_string_lossy());
        }
        if ui.button(tr("Get More Themes…")).clicked() {
            open_url("https://github.com/thornbux/glimt/tree/main/themes");
        }
    }

    /// Preload `n` images on each side.
    pub(crate) fn set_preload(&mut self, n: usize) {
        self.preload.store(n, Ordering::Relaxed);
        self.refill();
        self.config.preload = n;
    }

    /// Decode everything again with or without EXIF rotation.
    pub(crate) fn set_exif_rotate(&mut self, on: bool) {
        self.config.exif_rotate = on;
        self.config.save();
        self.rotate.store(on, Ordering::Relaxed);
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
        if on && self.desktop_screen.is_none() {
            self.desktop_screen_rx = Receiving::of(read_desktop_screen(ctx));
        }
        self.use_screen();
    }

    /// Show through the desktop's screen profile when it counts and colord has one, else through
    /// the chosen one.
    pub(crate) fn use_screen(&mut self) {
        let path = match &self.desktop_screen {
            Some(p) if self.config.desktop_profile => p.clone(),
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
        set_colour(|c| c.own = on);
        self.config.own_profile = on;
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
                scroll_menu(ui, tr("File"), |ui| {
                    menu_item(ui, act, Action::OpenFile);
                    menu_item(ui, act, Action::OpenFolder);
                    menu_item(ui, act, Action::OpenRecursive);
                    menu_item(ui, act, Action::OpenFolders);
                    self.recent_menu(ui);
                    menu_item(ui, act, Action::NewFolder);
                    menu_item(ui, act, Action::Copy);
                    menu_item(ui, act, Action::Move);
                    menu_item(ui, act, Action::Rename);
                    menu_item(ui, act, Action::OpenWith);
                    self.open_with_menu(ui);
                    menu_item(ui, act, Action::SetWallpaper);
                    #[cfg(feature = "video")]
                    menu_item(ui, act, Action::Play);
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
                });
                scroll_menu(ui, tr("Go"), |ui| {
                    menu_item(ui, act, Action::First);
                    menu_item(ui, act, Action::Prev);
                    menu_item(ui, act, Action::Next);
                    menu_item(ui, act, Action::Last);
                    ui.separator();
                    menu_item(ui, act, Action::Back);
                    menu_item(ui, act, Action::Forward);
                    menu_item(ui, act, Action::Home);
                    menu_item(ui, act, Action::Up);
                    ui.separator();
                    menu_item(ui, act, Action::FocusSearch);
                });
                scroll_menu(ui, tr("Select"), |ui| {
                    menu_item(ui, act, Action::SelectAll);
                    menu_item(ui, act, Action::SelectNone);
                    menu_item(ui, act, Action::InvertSelection);
                    ui.separator();
                    menu_item(ui, act, Action::CopyPath);
                    menu_item(ui, act, Action::CopyPathUnquoted);
                    menu_item(ui, act, Action::CopyImage);
                    menu_item(ui, act, Action::CopyFiles);
                });
                scroll_menu(ui, tr("Edit"), |ui| {
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
                    menu_item(ui, act, Action::Keywords);
                    ui.separator();
                    self.plugins_menu(ui);
                    ui.separator();
                    menu_item(ui, act, Action::Preferences);
                });
                scroll_menu(ui, tr("View"), |ui| {
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
                            let radio = ui.radio(self.config.view_filter == f, text);
                            if noted(&radio, tr(label)).clicked() {
                                self.set_view_filter(f);
                            }
                        }
                        ui.separator();
                        menu_item(ui, act, Action::ViewFilter);
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
                    menu_item(ui, act, Action::HideTools);
                    let c = &self.config;
                    menu_toggle(ui, act, c.toolbar, Action::Toolbar);
                    menu_toggle(ui, act, c.path_bar, Action::PathBar);
                    menu_toggle(ui, act, c.folder_tree, Action::FolderTree);
                    menu_toggle(ui, act, c.show_subfolders, Action::Subfolders);
                    menu_toggle(ui, act, c.show_hidden, Action::HiddenFiles);
                    menu_toggle(ui, act, c.thumbnails, Action::Thumbnails);
                    menu_toggle(ui, act, c.icons, Action::Icons);
                    menu_toggle(ui, act, c.exif_panel, Action::ExifPanel);
                    menu_toggle(ui, act, c.sort_manager, Action::SortManager);
                    menu_toggle(ui, act, self.exif_window, Action::ExifWindow);
                    menu_toggle(ui, act, self.pan_window.is_some(), Action::PanView);
                    menu_toggle(ui, act, c.osd, Action::Overlay);
                    menu_item(ui, act, Action::Histogram);
                    menu_toggle(ui, act, c.histogram_log, Action::HistogramLog);
                    menu_item(ui, act, Action::Refresh);
                    ui.separator();
                    let (icon, label) = if self.slideshow.is_some() {
                        (theme::icon::STOP, tr("Stop Slideshow"))
                    } else {
                        (theme::icon::PLAY, tr("Start Slideshow"))
                    };
                    let text = iconed(ui, icon, label);
                    menu_button(ui, act, egui::Button::new(text), Action::Slideshow);
                    menu_toggle(ui, act, self.paused, Action::Pause);
                    menu_item(ui, act, Action::Faster);
                    menu_item(ui, act, Action::Slower);
                    menu_toggle(ui, act, self.config.random, Action::RandomOrder);
                });
                scroll_menu(ui, tr("Window"), |ui| {
                    menu_item(ui, act, Action::NewWindow);
                    menu_item(ui, act, Action::CloseWindow);
                    menu_item(ui, act, Action::WindowTitle);
                    ui.separator();
                    menu_toggle(ui, act, self.projector, Action::Projector);
                    let pointer = self.config.projector_pointer;
                    menu_toggle(ui, act, pointer, Action::ProjectorPointer);
                });
                scroll_menu(ui, tr("Help"), |ui| {
                    menu_item(ui, act, Action::Help);
                    menu_item(ui, act, Action::Shortcuts);
                    menu_item(ui, act, Action::KeyboardMap);
                    ui.separator();
                    menu_item(ui, act, Action::About);
                });
            });
        });
        close_menus(ui.ctx(), bar.response.rect, &mut self.menu_away);
        if self.config.toolbar {
            self.toolbar(ui, act);
        }
    }

    /// A file's right-click menu; it acts on the selection.
    pub(crate) fn file_menu(&mut self, ui: &mut egui::Ui, list_act: &mut Option<Action>) {
        for action in [
            Action::Copy,
            Action::Move,
            Action::CopyPath,
            Action::CopyImage,
            Action::CopyFiles,
        ] {
            menu_item(ui, list_act, action);
        }
        self.open_with_menu(ui);
        scroll_menu(
            ui,
            iconed(ui, theme::icon::STACK_PLUS, tr("Add to Collection")),
            |ui| {
                for (name, file) in collections::list() {
                    if noted(&ui.button(iconed(ui, theme::icon::STACK, &name)), &name).clicked() {
                        self.add_to_collection(&file);
                    }
                }
                menu_item(ui, list_act, Action::NewCollection);
            },
        );
        if self.collection.is_some() {
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
                let button = ui.button(iconed(ui, theme::icon::PLUGIN, &plugin.name));
                if noted(&button, &plugin.name).clicked() {
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
                if noted(&ui.button(iconed(ui, theme::icon::STOP, &text)), &text).clicked() {
                    let mut running = self.plugins_running.lock().unwrap();
                    let was = running.len();
                    running.retain(|r| r.1 != pid);
                    // Not when it ended meanwhile: its number may be another's by now.
                    if running.len() < was {
                        std::thread::spawn(move || external::stop(pid));
                    }
                }
            }
            ui.separator();
            let folder = iconed(ui, theme::icon::FOLDER_OPEN, tr("Open the Plugins Folder"));
            if noted(&ui.button(folder), tr("Open the Plugins Folder"))
                .on_hover_text(tr(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                ))
                .clicked()
                && let Some(dir) = dir
            {
                let _ = std::fs::create_dir_all(&dir);
                open_url(&dir.to_string_lossy());
            }
            let Some(k) = chosen.filter(|_| !self.list.files.is_empty()) else {
                return;
            };
            let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
            // A turned image still being saved would be written over the plugin's edit.
            if !self.none_saving(&files) {
                return;
            }
            let plugin = self.plugins[k].clone();
            let (dir, running) = (self.dir.clone(), self.plugins_running.clone());
            let deleted = self.deleted_tx.clone();
            let (tx, ctx) = (self.notices_tx.clone(), ui.ctx().clone());
            // A plugin may be an editor that stays open for hours: not waited for at closing,
            // and no busy pointer meanwhile, as for Open With.
            std::thread::spawn(move || {
                let result = plugin.run(&files, &dir, &running);
                let _ = deleted.send(Deleted {
                    relist: true,
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

    /// "Open With" submenu: the applications registered for the shown file's type; the chosen
    /// one opens the selected files (or the shown one).
    pub(crate) fn open_with_menu(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.list.files.get(self.list.cur) else {
            return;
        };
        let ext = Path::new(name)
            .extension()
            .map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
        let path = self.path(self.list.cur);
        scroll_menu(ui, iconed(ui, theme::icon::APP, tr("Open With")), |ui| {
            // The menu keeps the width of its first frame, "…": names must not wrap to it.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for (ext, apps) in self.apps_rx.try_iter() {
                self.apps.insert(ext, Some(apps));
            }
            // apps_for runs gio twice: in the background, so the menu never stalls.
            let apps = self.apps.entry(ext.clone()).or_insert_with(|| {
                let (tx, ctx) = (self.apps_tx.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let _ = tx.send((ext, apps_for(&path)));
                    ctx.request_repaint();
                });
                None
            });
            let Some(apps) = apps else {
                ui.label("…");
                return;
            };
            #[cfg(feature = "portals")]
            if apps.is_empty() && crate::portal::in_flatpak() {
                // The Flatpak, without the host's applications: the desktop's own chooser.
                if ui
                    .button(iconed(ui, theme::icon::APP, tr("Choose Application…")))
                    .clicked()
                {
                    // Each selected file (or the shown one), the chooser asking for each.
                    let files: Vec<PathBuf> =
                        self.targets().iter().map(|&i| self.path(i)).collect();
                    let tx = self.notices_tx.clone();
                    std::thread::spawn(move || {
                        // Cancel ends them all, not only the one asked about.
                        for path in files {
                            match crate::portal::open_with(&path) {
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
                if noted(&ui.button(iconed(ui, theme::icon::APP, name)), name).clicked() {
                    chosen = Some(desktop.clone());
                }
            }
            if let Some(desktop) = chosen {
                let files: Vec<PathBuf> = self.targets().iter().map(|&i| self.path(i)).collect();
                let tx = self.notices_tx.clone();
                std::thread::spawn(move || {
                    #[allow(clippy::disallowed_methods)]
                    // the chosen application may stay open: its own thread
                    let out = external::host_command("gio")
                        .arg("launch")
                        .arg(&desktop)
                        .args(&files)
                        .stdout(std::process::Stdio::null())
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => drop(tx.send(String::from_utf8_lossy(&o.stderr).trim().into())),
                        Err(e) => drop(tx.send(format!("gio: {e}"))),
                    }
                });
            }
        });
    }

    /// Open the shown file in `player` (a command line), or in the default application when
    /// `player` is empty. Runs in its own thread, which waits for the program to exit.
    pub(crate) fn launch(&self, player: &str) {
        if self.list.files.is_empty() {
            return;
        }
        let (path, tx) = (self.path(self.list.cur), self.notices_tx.clone());
        let mut words = plugins::words(player);
        std::thread::spawn(move || {
            let run = |name: &str, mut cmd: std::process::Command| {
                // Only its errors are read: a program that prints for hours is not kept.
                #[allow(clippy::disallowed_methods)]
                // the player stays open as long as it plays: its own thread
                let out = cmd.arg(&path).stdout(std::process::Stdio::null()).output();
                out.map_err(|e| format!("{name}: {e}"))
            };
            let out = if words.is_empty() {
                // gio is part of glib; xdg-open covers systems without it. In a Flatpak both
                // ask the portal for the default application.
                match external::has("gio") {
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
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => drop(tx.send(String::from_utf8_lossy(&o.stderr).trim().into())),
                Err(e) => drop(tx.send(e)),
            }
        });
    }
}
