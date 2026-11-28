//! The menu bar and toolbar, the menus that change settings, Tools and Open With, and the helpers
//! every menu is drawn with.

use crate::files::config::xdg_dir;
use crate::files::external::{apps_for, command, open_own_folder, open_url};
use crate::files::file_ops::OperationReport;
use crate::images::colour::{FILTERS, ProfileAsked, ProfileSet, set_colour, set_intent};
use crate::model::selection::Targets;
use crate::support::format::{ShownText, count, escaped_name};
use crate::support::lang::tr;
use crate::support::test_hooks::noted;
use crate::support::threads::lock_even_if_poisoned;
use crate::support::threads::{self, Busy};
use crate::support::threads::{Receiving, Took};
use crate::ui::actions::{Action, shortcut_text};
use crate::{features::tools, files::config, files::external, tf, ui::theme};
use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

/// Where the popup IDs of the menu bar's menus are kept, for `scroll_menu` and `close_menus`.
pub(crate) const BAR_MENUS: &str = "menu bar menus";

/// The most files that Open With Default App opens without asking first, as many as Nautilus
/// opens without asking.
const OPENED_WITHOUT_ASKING: usize = 5;

/// Open With's applications: each one's name and desktop file.
type Apps = Vec<(String, PathBuf)>;

/// Open With's programs for each file type, read in the background.
#[derive(Default)]
pub(crate) struct OpenWith {
    /// The applications by file extension, or by the whole path for a file without one. An
    /// entry is asked for once in the background, and then kept with the time it came.
    apps: HashMap<String, AppsLookup>,
}

/// One entry of `OpenWith`: the question still running, or its answer and when it came.
enum AppsLookup {
    Asking(Receiving<Apps>),
    Found(Apps, Instant),
}

impl OpenWith {
    /// How long an empty answer is kept. gio may have timed out on a sleeping share, or the
    /// thread may have panicked, so the question is asked again after this.
    const RETRY: std::time::Duration = std::time::Duration::from_secs(60);

    /// Finds the applications for the file type `key` with `ask`, in a thread of its own the
    /// first time. Returns `None` while that thread runs.
    fn apps_for_type(
        &mut self,
        key: &str,
        ask: impl FnOnce() -> Apps + Send + 'static,
        ctx: &egui::Context,
    ) -> Option<&Apps> {
        let stale = match self.apps.get(key) {
            None => true,
            Some(AppsLookup::Asking(_)) => false,
            Some(AppsLookup::Found(apps, at)) => apps.is_empty() && at.elapsed() >= Self::RETRY,
        };
        if stale {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            threads::spawn(
                Busy::NoPointer(
                    "the applications of Open With, a look-up nobody waits for, shown when they come",
                ),
                move || {
                    let _ = tx.send(ask());
                    ctx.request_repaint();
                },
            );
            self.apps
                .insert(key.to_owned(), AppsLookup::Asking(Receiving::of(rx)));
        }
        let entry = self.apps.get_mut(key)?;
        if let AppsLookup::Asking(rx) = entry {
            // A thread that panicked counts as an empty answer, which is asked again later.
            match rx.take() {
                Took::Nothing => return None,
                Took::Answer(apps) => *entry = AppsLookup::Found(apps, Instant::now()),
                Took::Gone => *entry = AppsLookup::Found(Vec::new(), Instant::now()),
            }
        }
        match entry {
            AppsLookup::Found(apps, _) => Some(apps),
            AppsLookup::Asking(_) => None,
        }
    }
}

/// The ways a shown text is drawn other than a button: a label, a weak, strong or heading label,
/// a tooltip, a combo box's text, a field's hint, a slider's text, a part of a laid out text, a
/// text painted by hand, a window's title, a collapsing header and what a screen reader says.
/// Each takes a `ShownText`, so a raw name or a `String` cannot be drawn, and `clippy.toml`
/// refuses the egui call each of them makes. egui's `From<String>` and `From<&str>` for
/// `RichText` and `WidgetText` stay open, as an impl's method has no path that clippy can
/// refuse, and refusing `From::from` itself would refuse every conversion. So review holds a
/// `"…".into()` given to an egui widget.
pub(crate) trait DrawShown {
    /// Draws `text` as a label.
    fn show_label(&mut self, text: impl AsRef<ShownText>) -> egui::Response;
    /// Draws `text` as a label in the weak colour.
    fn show_weak(&mut self, text: impl AsRef<ShownText>) -> egui::Response;
    /// Draws `text` as a heading.
    fn show_heading(&mut self, text: impl AsRef<ShownText>) -> egui::Response;
    /// Draws `text` as a label in the strong colour.
    fn show_strong(&mut self, text: impl AsRef<ShownText>) -> egui::Response;
    /// Draws `text` as a label in the look that `style` gives it, such as a colour, a size or
    /// the code font (`egui::RichText::code`).
    fn show_styled(
        &mut self,
        text: impl AsRef<ShownText>,
        style: impl FnOnce(egui::RichText) -> egui::RichText,
    ) -> egui::Response;
}

impl DrawShown for egui::Ui {
    #[allow(clippy::disallowed_methods)] // The one place a label of text is drawn.
    fn show_label(&mut self, text: impl AsRef<ShownText>) -> egui::Response {
        self.label(text.as_ref().as_str())
    }
    #[allow(clippy::disallowed_methods)] // The one place a label of text is drawn.
    fn show_weak(&mut self, text: impl AsRef<ShownText>) -> egui::Response {
        self.weak(text.as_ref().as_str())
    }
    #[allow(clippy::disallowed_methods)] // The one place a label of text is drawn.
    fn show_heading(&mut self, text: impl AsRef<ShownText>) -> egui::Response {
        self.heading(text.as_ref().as_str())
    }
    #[allow(clippy::disallowed_methods)] // The one place a label of text is drawn.
    fn show_strong(&mut self, text: impl AsRef<ShownText>) -> egui::Response {
        self.strong(text.as_ref().as_str())
    }
    #[allow(clippy::disallowed_methods)] // The one place a label of a styled text is drawn.
    fn show_styled(
        &mut self,
        text: impl AsRef<ShownText>,
        style: impl FnOnce(egui::RichText) -> egui::RichText,
    ) -> egui::Response {
        self.add(egui::Label::new(style(rich(text))))
    }
}

/// A tooltip of a shown text, which egui's own `on_hover_text` would take as any `String`.
pub(crate) trait ShownTip {
    /// Shows `text` while the pointer is on the widget.
    fn tip(self, text: impl AsRef<ShownText>) -> egui::Response;
    /// Shows `text` beside the pointer while it is on the widget.
    fn tip_at_pointer(self, text: impl AsRef<ShownText>) -> egui::Response;
}

impl ShownTip for egui::Response {
    #[allow(clippy::disallowed_methods)] // The one place a tooltip of text is drawn.
    fn tip(self, text: impl AsRef<ShownText>) -> egui::Response {
        self.on_hover_text(text.as_ref().as_str())
    }
    #[allow(clippy::disallowed_methods)] // The one place a tooltip at the pointer is drawn.
    fn tip_at_pointer(self, text: impl AsRef<ShownText>) -> egui::Response {
        self.on_hover_text_at_pointer(text.as_ref().as_str())
    }
}

/// The grey text an empty text field shows, as a shown text.
pub(crate) trait ShownHint {
    /// Returns the field showing `text` while it is empty.
    fn hint(self, text: impl AsRef<ShownText>) -> Self;
}

impl ShownHint for egui::TextEdit<'_> {
    #[allow(clippy::disallowed_methods)] // The one place a field's hint is set.
    fn hint(self, text: impl AsRef<ShownText>) -> Self {
        self.hint_text(text.as_ref().as_str())
    }
}

/// The text beside a slider, as a shown text.
pub(crate) trait ShownSlider {
    /// Returns the slider with `text` beside it.
    fn shown_text(self, text: impl AsRef<ShownText>) -> Self;
}

impl ShownSlider for egui::Slider<'_> {
    #[allow(clippy::disallowed_methods)] // The one place a slider's text is set.
    fn shown_text(self, text: impl AsRef<ShownText>) -> Self {
        self.text(text.as_ref().as_str())
    }
}

/// The unit after a number to drag or slide, such as " %", as a shown text.
pub(crate) trait ShownUnit {
    /// Returns the field with `text` after its number.
    fn unit(self, text: impl AsRef<ShownText>) -> Self;
}

impl ShownUnit for egui::Slider<'_> {
    #[allow(clippy::disallowed_methods)] // The one place a slider's unit is set.
    fn unit(self, text: impl AsRef<ShownText>) -> Self {
        self.suffix(text.as_ref().as_str())
    }
}

impl ShownUnit for egui::DragValue<'_> {
    #[allow(clippy::disallowed_methods)] // The one place a dragged number's unit is set.
    fn unit(self, text: impl AsRef<ShownText>) -> Self {
        self.suffix(text.as_ref().as_str())
    }
}

/// The text on a progress bar, as a shown text.
pub(crate) trait ShownProgress {
    /// Returns the bar with `text` on it.
    fn progress_text(self, text: impl AsRef<ShownText>) -> Self;
}

impl ShownProgress for egui::ProgressBar {
    #[allow(clippy::disallowed_methods)] // The one place a progress bar's text is set.
    fn progress_text(self, text: impl AsRef<ShownText>) -> Self {
        self.text(text.as_ref().as_str())
    }
}

/// A part of a laid out text, as a shown text.
pub(crate) trait ShownLayout {
    /// Adds `text` at the end, after `leading_space` points, in `format`.
    fn append_shown(&mut self, text: &ShownText, leading_space: f32, format: egui::TextFormat);
}

impl ShownLayout for egui::text::LayoutJob {
    #[allow(clippy::disallowed_methods)] // The one place a part of a laid out text is added.
    fn append_shown(&mut self, text: &ShownText, leading_space: f32, format: egui::TextFormat) {
        self.append(text.as_str(), leading_space, format);
    }
}

/// The text a closed combo box shows, as a shown text.
pub(crate) trait ShownChoice {
    /// Returns the combo box showing `text` while it is closed.
    fn selected(self, text: impl AsRef<ShownText>) -> Self;
}

impl ShownChoice for egui::ComboBox<'_> {
    #[allow(clippy::disallowed_methods)] // The one place a combo box's text is set.
    fn selected(self, text: impl AsRef<ShownText>) -> Self {
        self.selected_text(text.as_ref().as_str())
    }
}

/// A text painted by hand, as a shown text.
pub(crate) trait PaintShown {
    /// Paints `text` at `at`, placed by `anchor`, in `font` and `colour`, and returns where.
    fn show_text(
        &self,
        at: egui::Pos2,
        anchor: egui::Align2,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
    ) -> egui::Rect;
    /// Lays out `text` on one line in `font` and `colour`, to be measured or painted.
    fn layout_shown(
        &self,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
    ) -> std::sync::Arc<egui::Galley>;
    /// Lays out `text` in `font` and `colour`, broken into lines of at most `width`.
    fn layout_shown_wrapped(
        &self,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
        width: f32,
    ) -> std::sync::Arc<egui::Galley>;
}

impl PaintShown for egui::Painter {
    #[allow(clippy::disallowed_methods)] // The one place a text is painted by hand.
    fn show_text(
        &self,
        at: egui::Pos2,
        anchor: egui::Align2,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
    ) -> egui::Rect {
        self.text(at, anchor, text.as_ref().as_str(), font, colour)
    }
    #[allow(clippy::disallowed_methods)] // The one place a text is laid out on one line.
    fn layout_shown(
        &self,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
    ) -> std::sync::Arc<egui::Galley> {
        self.layout_no_wrap(text.as_ref().to_string(), font, colour)
    }
    #[allow(clippy::disallowed_methods)] // The one place a text is laid out in lines.
    fn layout_shown_wrapped(
        &self,
        text: impl AsRef<ShownText>,
        font: egui::FontId,
        colour: egui::Color32,
        width: f32,
    ) -> std::sync::Arc<egui::Galley> {
        self.layout(text.as_ref().to_string(), font, colour, width)
    }
}

impl From<ShownText> for egui::WidgetText {
    fn from(text: ShownText) -> Self {
        text.as_str().into()
    }
}

impl From<&ShownText> for egui::WidgetText {
    fn from(text: &ShownText) -> Self {
        text.as_str().into()
    }
}

impl From<ShownText> for egui::RichText {
    fn from(text: ShownText) -> Self {
        rich(text)
    }
}

impl From<&ShownText> for egui::RichText {
    fn from(text: &ShownText) -> Self {
        rich(text)
    }
}

/// The title of a menu or submenu: a shown text, or one with an icon before it (`with_icon`).
pub(crate) struct MenuTitle(egui::WidgetText);

impl From<ShownText> for MenuTitle {
    fn from(text: ShownText) -> Self {
        MenuTitle(text.as_str().into())
    }
}

impl From<&ShownText> for MenuTitle {
    fn from(text: &ShownText) -> Self {
        MenuTitle(text.as_str().into())
    }
}

impl From<egui::text::LayoutJob> for MenuTitle {
    fn from(text: egui::text::LayoutJob) -> Self {
        MenuTitle(text.into())
    }
}

impl crate::App {
    // ---- Settings and the menus that change them ----

    /// The chosen theme, from its file as last read, with the desktop's look where it sets none;
    /// High Contrast while the desktop asks for it, unless that is turned off.
    pub(crate) fn apply_theme(&self, ctx: &egui::Context) {
        let look = self.desktop_settings.look();
        let name = match look.high_contrast && self.config.follow_contrast {
            true => theme::HIGH_CONTRAST,
            false => &self.config.theme,
        };
        let chosen = self.desktop_settings.theme(name);
        theme::apply(ctx, &chosen, look.dark, look.accent);
        let desktop = look.text_scale.filter(|_| self.config.follow_text_scale);
        ctx.set_zoom_factor(self.config.ui_scale as f32 / 100.0 * desktop.unwrap_or(1.0));
    }

    /// View → Theme: the themes to choose from, then where to make and find more.
    pub(crate) fn theme_menu(&mut self, ui: &mut egui::Ui) {
        for name in self.desktop_settings.theme_names(ui.ctx()) {
            let icon = match name.as_str() {
                "Dark" => theme::icon::MOON,
                "Light" => theme::icon::SUN,
                "Desktop" => theme::icon::MONITOR,
                "Nord" => theme::icon::SNOWFLAKE,
                theme::HIGH_CONTRAST => theme::icon::HALF,
                theme::HIGH_CONTRAST_LIGHT => theme::icon::HALF_TILT,
                _ => theme::icon::BRUSH,
            };
            // Chosen again too, so a theme file just edited shows, as the open menu reads the
            // files again every second.
            let shown = theme::shown_name(&name);
            #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted below.
            let radio = ui.radio(self.config.theme == name, with_icon(ui, icon, shown));
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
            && let Some(dir) = theme::own_themes_dir()
        {
            let notices = self.notice_sender.clone();
            threads::spawn(
                Busy::NoPointer(
                    "the file manager may stay open for hours, and nothing waits for it",
                ),
                move || open_own_folder(&dir, &notices),
            );
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
    pub(crate) fn set_auto_rotate(&mut self, on: bool) {
        self.config.auto_rotate = on;
        self.config.save();
        self.decoding.set_auto_rotate(on);
        // The thumbnails too, which are turned by it as well.
        self.redecode();
    }

    /// Decode everything again through another view filter.
    pub(crate) fn set_view_filter(&mut self, f: usize) {
        self.config.view_filter = f;
        self.config.save();
        crate::images::colour::use_view_filter(f);
        self.redecode();
    }

    /// Decode everything again with or without colour management.
    pub(crate) fn set_color_management(&mut self, on: bool) {
        self.config.color_management = on;
        self.config.save();
        crate::images::colour::use_color_management(on);
        self.redecode();
    }

    /// Show everything through the screen profile in `path` (empty: none, the screen is sRGB).
    /// The file is read in the background, and the setting changes when it could be
    /// (`receive_profile_changes`).
    pub(crate) fn set_screen_profile(&mut self, ctx: &egui::Context, path: String) {
        let asked = ProfileAsked::Screen { path, chosen: true };
        self.decoding.profile_changes().ask(&self.busy, ctx, asked);
    }

    /// Whether the desktop's screen profile goes before the chosen one; read from colord when on.
    pub(crate) fn set_desktop_profile(&mut self, on: bool, ctx: &egui::Context) {
        self.config.desktop_profile = on;
        self.config.save();
        if on {
            self.desktop_settings.ask_screen_profile(ctx);
        }
        self.choose_screen_profile(ctx);
    }

    /// Show through the desktop's screen profile when it counts and colord has one, else through
    /// the chosen one. Its file is read in the background.
    pub(crate) fn choose_screen_profile(&mut self, ctx: &egui::Context) {
        let path = match self.desktop_settings.screen_profile() {
            Some(p) if self.config.desktop_profile => p.to_owned(),
            _ => self.config.screen_profile.clone(),
        };
        let asked = ProfileAsked::Screen {
            path,
            chosen: false,
        };
        self.decoding.profile_changes().ask(&self.busy, ctx, asked);
    }

    /// The profile for images without one: srgb, adobe, p3 or an ICC file, read in the
    /// background.
    pub(crate) fn set_input_profile(&mut self, ctx: &egui::Context, name: String) {
        let asked = ProfileAsked::Input { name };
        self.decoding.profile_changes().ask(&self.busy, ctx, asked);
    }

    /// Takes the colour profiles read and set in the background: a chosen one is kept in the
    /// settings once it could be read, and the images are decoded again in its colours. A file
    /// that is not an RGB profile is told, and the profile before it stays.
    pub(crate) fn receive_profile_changes(&mut self, ctx: &egui::Context) {
        while let Some((set, _pointer)) = self.decoding.profile_changes().next() {
            match set {
                ProfileSet::Screen { answer: Err(e), .. }
                | ProfileSet::Input { answer: Err(e), .. } => {
                    self.notice.give_error(e);
                }
                ProfileSet::Screen {
                    path, chosen: true, ..
                } => {
                    self.config.screen_profile = path;
                    self.config.save();
                    self.choose_screen_profile(ctx);
                }
                ProfileSet::Screen {
                    answer: Ok(true), ..
                } => self.redecode(),
                ProfileSet::Screen { .. } => {}
                ProfileSet::Input { name, .. } => {
                    self.config.input_profile = name;
                    self.config.save();
                    self.redecode();
                }
            }
        }
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
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        let bar = egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                scroll_menu(ui, tr("File"), |ui| self.menu_file(ui, chosen));
                scroll_menu(ui, tr("Go"), |ui| self.menu_go(ui, chosen));
                scroll_menu(ui, tr("Select"), |ui| self.menu_select(ui, chosen));
                scroll_menu(ui, tr("Edit"), |ui| self.menu_edit(ui, chosen));
                scroll_menu(ui, tr("View"), |ui| self.menu_view(ui, chosen));
                scroll_menu(ui, tr("Window"), |ui| self.menu_window(ui, chosen));
                scroll_menu(ui, tr("Help"), |ui| self.menu_help(ui, chosen));
            });
        });
        self.window.close_menus(ui.ctx(), bar.response.rect);
        if self.config.toolbar {
            self.toolbar(ui, chosen);
        }
    }

    /// The File menu.
    fn menu_file(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::OpenFile);
        menu_item(ui, chosen, Action::OpenFolder);
        menu_item(ui, chosen, Action::OpenRecursive);
        menu_item(ui, chosen, Action::OpenFolders);
        self.recent_menu(ui);
        menu_item(ui, chosen, Action::NewFolder);
        menu_item(ui, chosen, Action::CopyTo);
        menu_item(ui, chosen, Action::CopyForSharing);
        menu_item(ui, chosen, Action::MoveTo);
        menu_item(ui, chosen, Action::Rename);
        menu_item(ui, chosen, Action::OpenWith);
        self.open_with_menu(ui);
        menu_item(ui, chosen, Action::SetWallpaper);
        #[cfg(feature = "video")]
        menu_item(ui, chosen, Action::PlayVideo);
        menu_item(ui, chosen, Action::SaveAs);
        menu_item(ui, chosen, Action::Print);
        // Only for a file of several frames or pages: the menu stays as it was
        // for every other one.
        if self.frame_count() > 1 {
            menu_item(ui, chosen, Action::SaveFrame);
            menu_item(ui, chosen, Action::ExportFrames);
        }
        ui.separator();
        menu_item(ui, chosen, Action::Delete);
        menu_item(ui, chosen, Action::DeletePermanently);
        ui.separator();
        self.collections_menu(ui, chosen);
        menu_item(ui, chosen, Action::FindDuplicates);
        menu_item(ui, chosen, Action::CheckImages);
        menu_item(ui, chosen, Action::BackUp);
        ui.separator();
        menu_item(ui, chosen, Action::CloseWindow);
        menu_item(ui, chosen, Action::Quit);
    }

    /// The Go menu.
    fn menu_go(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::First);
        menu_item(ui, chosen, Action::Previous);
        menu_item(ui, chosen, Action::Next);
        menu_item(ui, chosen, Action::Last);
        ui.separator();
        menu_item(ui, chosen, Action::Back);
        menu_item(ui, chosen, Action::Forward);
        menu_item(ui, chosen, Action::HomeFolder);
        menu_item(ui, chosen, Action::ParentFolder);
        ui.separator();
        menu_item(ui, chosen, Action::FocusSearch);
    }

    /// The Select menu.
    fn menu_select(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::SelectAll);
        menu_item(ui, chosen, Action::SelectNone);
        menu_item(ui, chosen, Action::InvertSelection);
        ui.separator();
        menu_item(ui, chosen, Action::CopyPath);
        menu_item(ui, chosen, Action::CopyPathUnquoted);
        menu_item(ui, chosen, Action::CopyFilesToClipboard);
    }

    /// The Edit menu.
    fn menu_edit(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::Undo);
        ui.separator();
        #[cfg(feature = "marks")]
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::STAR, tr("Marks and Stars")),
            |ui| {
                for action in [
                    Action::Mark1,
                    Action::Mark2,
                    Action::Mark3,
                    Action::Mark4,
                    Action::Mark5,
                    Action::Mark6,
                ] {
                    menu_item(ui, chosen, action);
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
                    menu_item(ui, chosen, action);
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
                    menu_item(ui, chosen, action);
                }
            },
        );
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::ORIENTATION, tr("Orientation")),
            |ui| {
                for action in [
                    Action::RotateCw,
                    Action::RotateCcw,
                    Action::Rotate180,
                    Action::Mirror,
                    Action::Flip,
                ] {
                    menu_item(ui, chosen, action);
                }
                ui.separator();
                menu_item(ui, chosen, Action::SaveOrientation);
            },
        );
        menu_item(ui, chosen, Action::Crop);
        // On the shown image, so not in the Select menu beside the copies of the selection.
        menu_item(ui, chosen, Action::CopyImage);
        menu_item(ui, chosen, Action::FocusKeywords);
        ui.separator();
        self.tools_menu(ui);
        ui.separator();
        menu_item(ui, chosen, Action::Preferences);
    }

    /// The View menu: layout, theme, colour, filter and zoom, the panels, and the slideshow.
    fn menu_view(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        self.layout_menu(ui, chosen);
        scroll_menu(ui, with_icon(ui, theme::icon::PALETTE, tr("Theme")), |ui| {
            self.theme_menu(ui)
        });
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::COLOUR, tr("Colour Management")),
            |ui| self.colour_settings(ui),
        );
        scroll_menu(ui, with_icon(ui, theme::icon::FILTER, tr("Filter")), |ui| {
            use theme::icon::{CIRCLE, HALF, HALF_TILT, SUN_DIM};
            let icons = [CIRCLE, HALF, HALF_TILT, SUN_DIM];
            for (f, label) in FILTERS.iter().enumerate() {
                let text = with_icon(ui, icons[f.min(3)], tr(label));
                #[allow(clippy::disallowed_methods)] // It has an icon before the text, noted below.
                let radio = ui.radio(self.config.view_filter == f, text);
                if noted(&radio, &tr(label)).clicked() {
                    self.set_view_filter(f);
                }
            }
            ui.separator();
            menu_item(ui, chosen, Action::NextFilter);
        });
        scroll_menu(ui, with_icon(ui, theme::icon::ZOOM, tr("Zoom")), |ui| {
            menu_item(ui, chosen, Action::ZoomIn);
            menu_item(ui, chosen, Action::ZoomOut);
            menu_item(ui, chosen, Action::Zoom1To1);
            let mode = self.config.fit_mode;
            menu_toggle(ui, chosen, mode == 0, Action::ZoomFit);
            menu_toggle(ui, chosen, mode == 1, Action::FitWidth);
            menu_toggle(ui, chosen, mode == 2, Action::FitHeight);
            menu_toggle(ui, chosen, mode == 3, Action::FitFill);
        });
        menu_item(ui, chosen, Action::FullScreen);
        menu_item(ui, chosen, Action::HideFileList);
        let c = &self.config;
        menu_toggle(ui, chosen, c.toolbar, Action::Toolbar);
        menu_toggle(ui, chosen, c.path_bar, Action::PathBar);
        menu_toggle(ui, chosen, c.folder_tree, Action::FolderTree);
        menu_toggle(ui, chosen, c.include_subfolders, Action::Subfolders);
        menu_toggle(ui, chosen, c.show_hidden, Action::HiddenFiles);
        menu_toggle(ui, chosen, c.thumbnails, Action::Thumbnails);
        menu_toggle(ui, chosen, c.icon_grid, Action::IconGrid);
        // Ticked only while the grid shows: from an image opened from it, the item goes back to
        // the grid, as G and Esc do.
        let grid = self.panel.is_area_grid_shown();
        menu_toggle(ui, chosen, grid, Action::ImageAreaGrid);
        let size = with_icon(ui, theme::icon::SIZE, tr("Thumbnail Size"));
        submenu(ui, size, |ui| self.thumbnail_size_setting(ui));
        let c = &self.config;
        menu_toggle(ui, chosen, c.grid_names, Action::GridNames);
        menu_toggle(ui, chosen, c.info_panel, Action::InfoPanel);
        menu_toggle(ui, chosen, c.sort_manager, Action::SortManager);
        menu_toggle(
            ui,
            chosen,
            self.info_panel.exif_window_open(),
            Action::ExifWindow,
        );
        menu_toggle(ui, chosen, self.pan_view.is_some(), Action::PanView);
        menu_toggle(ui, chosen, c.full_screen_info, Action::FullScreenInfo);
        menu_item(ui, chosen, Action::Histogram);
        menu_toggle(ui, chosen, c.histogram_log, Action::HistogramLog);
        menu_item(ui, chosen, Action::Refresh);
        ui.separator();
        let (icon, label) = if self.slideshow.is_running() {
            (theme::icon::STOP, tr("Stop Slideshow"))
        } else {
            (theme::icon::PLAY, tr("Start Slideshow"))
        };
        let text = with_icon(ui, icon, label);
        #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
        menu_button(ui, chosen, egui::Button::new(text), Action::Slideshow);
        menu_toggle(
            ui,
            chosen,
            self.slideshow.is_paused(),
            Action::PauseSlideshow,
        );
        menu_item(ui, chosen, Action::SlideshowFaster);
        menu_item(ui, chosen, Action::SlideshowSlower);
        menu_toggle(
            ui,
            chosen,
            self.config.slideshow_random,
            Action::RandomOrder,
        );
    }

    /// The Window menu.
    fn menu_window(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::NewWindow);
        menu_item(ui, chosen, Action::CloseWindow);
        menu_item(ui, chosen, Action::WindowTitle);
        menu_item(ui, chosen, Action::WindowName);
        ui.separator();
        menu_toggle(ui, chosen, self.window.has_projector(), Action::Projector);
        let pointer = self.config.projector_pointer;
        menu_toggle(ui, chosen, pointer, Action::ProjectorPointer);
    }

    /// The Help menu.
    fn menu_help(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        menu_item(ui, chosen, Action::Help);
        menu_item(ui, chosen, Action::Shortcuts);
        menu_item(ui, chosen, Action::KeyboardMap);
        ui.separator();
        menu_item(ui, chosen, Action::About);
    }

    /// A file's right-click menu; it acts on the selection.
    pub(crate) fn file_menu(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        for action in [
            Action::CopyTo,
            Action::CopyForSharing,
            Action::MoveTo,
            Action::CopyPath,
            Action::CopyFilesToClipboard,
        ] {
            menu_item(ui, chosen, action);
        }
        menu_item(ui, chosen, Action::OpenWith);
        self.open_with_menu(ui);
        scroll_menu(
            ui,
            with_icon(ui, theme::icon::STACK_PLUS, tr("Add to Collection")),
            |ui| {
                let all = self.folder.saved_collections().listed(ui.ctx());
                for (name, file) in all.into_iter().flatten() {
                    if icon_button(ui, theme::icon::STACK, escaped_name(&name)).clicked()
                        && let Some(targets) = self.targets()
                    {
                        let added_to = crate::model::collections::CollectionAddedTo::Saved(file);
                        self.add_to_collection(ui.ctx(), targets, added_to);
                    }
                }
                menu_item(ui, chosen, Action::NewCollection);
            },
        );
        if self.folder.collection().is_some() {
            menu_item(ui, chosen, Action::RemoveFromCollection);
        }
        ui.separator();
        menu_item(ui, chosen, Action::Delete);
        menu_item(ui, chosen, Action::DeletePermanently);
    }

    /// Edit → Tools: run one on the selected files, or the shown one, in the background. The
    /// files are read again afterwards, as tools change them.
    pub(crate) fn tools_menu(&mut self, ui: &mut egui::Ui) {
        scroll_menu(ui, with_icon(ui, theme::icon::TOOL, tr("Tools")), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            // The folder to open is looked for only when it is opened, in its thread.
            let glimt_home = xdg_dir("XDG_DATA_HOME", ".local/share", "glimt");
            let tools = self.tools.listed().to_vec();
            if tools.is_empty() {
                ui.show_weak(tr("No tools"));
            }
            let mut chosen = None;
            for (k, tool) in tools.iter().enumerate() {
                if icon_button(ui, theme::icon::TOOL, tool.shown_name()).clicked() {
                    chosen = Some(k);
                }
            }
            // A tool that is still running, an editor left open or a script that hangs, is
            // ended here with what it started, in the background (it is given a second).
            let running = lock_even_if_poisoned(&self.tools_running).clone();
            if !running.is_empty() {
                ui.separator();
            }
            for (name, pid) in running {
                let text = tf!(
                    "Stop {name}",
                    name = ShownText::from_file_text_on_one_line(&name)
                );
                if icon_button(ui, theme::icon::STOP, &text).clicked() {
                    let mut running = lock_even_if_poisoned(&self.tools_running);
                    let was = running.len();
                    running.retain(|r| r.1 != pid);
                    // Not when it ended meanwhile: its number may be another's by now.
                    if running.len() < was {
                        // The tool's own thread tells when the tool ends.
                        threads::spawn(Busy::NoPointer("a tool's Stop"), move || {
                            external::stop_process_group(pid)
                        });
                    }
                }
            }
            ui.separator();
            if icon_button(ui, theme::icon::FOLDER_OPEN, tr("Open the Tools Folder"))
                .tip(tr(
                    "Desktop files there, as Geeqie's, show in this menu after a restart",
                ))
                .clicked()
                && let Some(home) = glimt_home
            {
                let notices = self.notice_sender.clone();
                threads::spawn(
                    Busy::NoPointer(
                        "the file manager may stay open for hours, and nothing waits for it",
                    ),
                    move || open_own_folder(&home.join("tools"), &notices),
                );
            }
            let Some(k) = chosen.filter(|_| !self.list.files().is_empty()) else {
                return;
            };
            let Some(targets) = self.targets() else {
                return;
            };
            self.run_tool(ui.ctx(), tools[k].clone(), targets);
        });
    }

    /// Runs `tool` on the files at `targets`, in the background.
    fn run_tool(&mut self, ctx: &egui::Context, tool: tools::Tool, targets: Targets) {
        let listed: Vec<_> = targets.iter().map(|&i| self.listed_file(i)).collect();
        let targets: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        // A turned image still being saved would be written over the tool's edit. The
        // files are held while the tool runs, so a delete, move or rename of them waits.
        // The thread lets go of the files the tool does not take once it has sorted them.
        let Some(held_back) = self.hold_back_while_writing(targets) else {
            return;
        };
        let (dir, running) = (self.folder.dir().to_path_buf(), self.tools_running.clone());
        let report_tx = self.file_operations.report_sender();
        let (tx, ctx) = (self.notice_sender.clone(), ctx.clone());
        let awaited = self.decoding.images_awaited().clone();
        threads::spawn(
            Busy::NoPointer(
                "a tool may be an editor that stays open for hours, so it is not waited for at \
             closing either, as for Open With",
            ),
            move || {
                // Each file is looked at on a share only after the images the window waits
                // for, as every other look in the background is (#294).
                let mut gate = crate::model::listing::ReadsFirst::for_files_anywhere(&awaited);
                for file in &listed {
                    gate.before_a_look(file.path());
                }
                // A file whose name another file has taken since the list was read is not given
                // to the tool, which may change it.
                let (targets, refused) = crate::files::file_io::still_listed(listed);
                if let Some(first) = refused.into_iter().next() {
                    tx.send_failed_write(first);
                }
                // The types a tool takes are looked up in the system's files of file types, the
                // first time, so the files are sorted here and not in the window's thread.
                let (files, left): (Vec<PathBuf>, Vec<PathBuf>) =
                    targets.into_iter().partition(|p| tool.accepts(p));
                held_back.hold_only(&files);
                let name = ShownText::from_file_text_on_one_line(&tool.name);
                if files.is_empty() {
                    let _ = tx.send_error(tf!("{name} takes none of these files", name));
                    return;
                }
                if !left.is_empty() {
                    let files = count(left.len(), "file");
                    let _ = tx.send(tf!(
                        "{name} leaves out {files} of a type it does not take",
                        name,
                        files
                    ));
                }
                let result = tool.run(&files, &dir, &running);
                drop(held_back);
                let _ = report_tx.send(OperationReport {
                    read_folder_again: true,
                    changed: files,
                    ..Default::default()
                });
                if let Err(e) = result {
                    let _ = tx.send_error(tf!("{name}: {e}", name = tool.shown_name(), e));
                }
                ctx.request_repaint();
            },
        );
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
        scroll_menu(ui, with_icon(ui, theme::icon::APP, tr("Open With")), |ui| {
            // The menu keeps the width of its first frame, "…": names must not wrap to it.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            // Files without an extension share no type, so each is asked about on its own.
            let key = match ext.is_empty() {
                true => path.to_string_lossy().into_owned(),
                false => ext,
            };
            // apps_for runs gio twice: in the background, so the menu never stalls.
            let asked = self
                .open_with
                .apps_for_type(&key, move || apps_for(&path), ui.ctx());
            let Some(apps) = asked else {
                ui.show_label(ShownText::untranslated("…"));
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
                    threads::spawn(
                        Busy::NoPointer(
                            "the application chosen may stay open for hours, and nothing waits for it",
                        ),
                        move || {
                            // Cancel ends them all, not only the one asked about.
                            for path in files {
                                match crate::files::portal::open_with(&path) {
                                    Ok(true) => {}
                                    Ok(false) => break,
                                    Err(e) => drop(tx.send_error(e)),
                                }
                            }
                        },
                    );
                }
                return;
            }
            if apps.is_empty() {
                ui.show_label(tr("No applications found"));
            }
            let mut chosen = None;
            for (name, desktop) in apps.iter() {
                if icon_button(
                    ui,
                    theme::icon::APP,
                    ShownText::from_file_text_on_one_line(name),
                )
                .clicked()
                {
                    chosen = Some(desktop.clone());
                }
            }
            if let Some(desktop) = chosen
                && let Some(targets) = self.targets()
            {
                let files: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
                let tx = self.notice_sender.clone();
                // The chosen application may stay open, so it is waited for in a thread of its own.
                threads::spawn(
                    Busy::NoPointer(
                        "the application chosen may stay open for hours, and nothing waits for it",
                    ),
                    move || {
                        let mut gio = external::host_command("gio");
                        gio.arg("launch").arg(&desktop).args(&files);
                        if let Err(told) = external::run_telling("gio", gio) {
                            let _ = tx.send_error(told);
                        }
                    },
                );
            }
        });
    }

    /// Opens the selected files, or the shown one, in the desktop's default application, as
    /// the Open With submenu opens them in the one chosen there. Each file starts a program of
    /// its own, so more than a few are asked about first. Before, Ctrl+A and this started a
    /// program for each of thousands of files.
    pub(crate) fn open_targets_in_default_app(&mut self, targets: Targets) {
        let files: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        match files.len() > OPENED_WITHOUT_ASKING {
            true => self.dialogs.ask_open_files(files),
            false => self.open_in("", files),
        }
    }

    /// Opens each of `files` in `command_line`, or in the default application when
    /// `command_line` is empty. It runs in its own thread, which waits for each program to exit.
    pub(crate) fn open_in(&self, command_line: &str, files: Vec<PathBuf>) {
        let tx = self.notice_sender.clone();
        let words: Vec<String> = (tools::command_words(command_line).into_iter())
            .map(|word| word.text)
            .collect();
        threads::spawn(
            Busy::NoPointer("each program may stay open for hours, and nothing waits for it"),
            move || {
                for path in files {
                    if let Err(told) = open_one_in(&words, &path) {
                        let _ = tx.send_error(told);
                        // In a Flatpak the default application is chosen through the desktop's
                        // portal, and its Cancel is a failure of gio, which ends them all.
                        if words.is_empty() && external::in_flatpak() {
                            break;
                        }
                    }
                }
            },
        );
    }

    /// Geeqie's toolbar: navigation, zoom, thumbnails, slideshow and preferences.
    pub(crate) fn toolbar(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Action>) {
        let size = [14.0, 20.0, 28.0][self.config.toolbar_icons.min(2)];
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.style_mut().override_font_id = Some(egui::FontId::proportional(size));
            ui.horizontal(|ui| {
                // The icons the menus show for the same actions.
                for action in [
                    Action::Back,
                    Action::Forward,
                    Action::ParentFolder,
                    Action::HomeFolder,
                ] {
                    tool_button(ui, chosen, action.icon(), action, false);
                }
                tool_button(ui, chosen, Action::Refresh.icon(), Action::Refresh, false);
                ui.separator();
                tool_button(ui, chosen, Action::ZoomIn.icon(), Action::ZoomIn, false);
                tool_button(ui, chosen, Action::ZoomOut.icon(), Action::ZoomOut, false);
                tool_button(ui, chosen, "1:1", Action::Zoom1To1, false);
                let fit = self.zoom.is_fitted();
                tool_button(ui, chosen, Action::ZoomFit.icon(), Action::ZoomFit, fit);
                ui.separator();
                let thumbs = self.config.thumbnails;
                tool_button(
                    ui,
                    chosen,
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
                    chosen,
                    show,
                    Action::Slideshow,
                    self.slideshow.is_running(),
                );
                tool_button(
                    ui,
                    chosen,
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
            Action::FullScreenInfo => &mut c.full_screen_info,
            Action::IconGrid => &mut c.icon_grid,
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
    title: impl Into<MenuTitle>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let title = title.into();
    let name = title.0.text().to_owned();
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
    let response = egui::containers::menu::MenuButton::new(title.0)
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

/// Returns a window titled `title`, which a caller places and fills.
#[allow(clippy::disallowed_methods)] // The one place a window is made.
pub(crate) fn window<'open>(title: impl AsRef<ShownText>) -> egui::Window<'open> {
    egui::Window::new(title.as_ref().as_str())
}

/// Returns a header of `text` that opens and closes what is under it.
#[allow(clippy::disallowed_methods)] // The one place a collapsing header is made.
pub(crate) fn collapsing(text: impl AsRef<ShownText>) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(text.as_ref().as_str())
}

/// Returns `text` as a rich text, which a caller sizes or colours.
#[allow(clippy::disallowed_methods)] // The one place a rich text is made.
pub(crate) fn rich(text: impl AsRef<ShownText>) -> egui::RichText {
    egui::RichText::new(text.as_ref().as_str())
}

/// Returns what a screen reader says of a widget of `role`: `text`.
#[allow(clippy::disallowed_methods)] // The one place a widget's spoken text is made.
pub(crate) fn spoken(role: egui::Role, enabled: bool, text: &ShownText) -> egui::WidgetInfo {
    egui::WidgetInfo::labeled(role, enabled, text.as_str())
}

/// Returns a layout of `text` on one line in `font` and `colour`, which a caller may cut to a
/// width before it is laid out.
#[allow(clippy::disallowed_methods)] // The one place a text is made a layout of one line.
pub(crate) fn single_line(
    text: &ShownText,
    font: egui::FontId,
    colour: egui::Color32,
) -> egui::text::LayoutJob {
    egui::text::LayoutJob::simple_singleline(text.to_string(), font, colour)
}

/// Returns `builder` with the window's title `title`, which the taskbar, the window list and a
/// screen reader show.
#[allow(clippy::disallowed_methods)] // The one place a new window's title is set.
pub(crate) fn titled(builder: egui::ViewportBuilder, title: &ShownText) -> egui::ViewportBuilder {
    builder.with_title(title.as_str())
}

/// Draws a button of `text` and notes it for the release tests, which click it by its text.
#[allow(clippy::disallowed_methods)] // The one place a plain button is drawn.
pub(crate) fn button(ui: &mut egui::Ui, text: impl AsRef<ShownText>) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.button(text), text)
}

/// Draws a button of `icon` before `text`, noted by its text as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a button with an icon is drawn.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    icon: &'static str,
    text: impl AsRef<ShownText>,
) -> egui::Response {
    let text = text.as_ref();
    noted(&ui.button(with_icon(ui, icon, text)), text)
}

/// Draws a button of `text` that can be clicked only when `on`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a button that may be off is drawn.
pub(crate) fn button_enabled(
    ui: &mut egui::Ui,
    on: bool,
    text: impl AsRef<ShownText>,
) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.add_enabled(on, egui::Button::new(text)), text)
}

/// Draws a small button of `text`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a small button is drawn.
pub(crate) fn small_button(ui: &mut egui::Ui, text: impl AsRef<ShownText>) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.small_button(text), text)
}

/// Draws the caret that opens or closes `name` in a tree, such as the folder tree. A screen
/// reader is told the name and whether it is open, not the icon's glyph.
#[allow(clippy::disallowed_methods)] // The one place a caret is drawn; it has no text to note.
pub(crate) fn caret(ui: &mut egui::Ui, open: bool, name: &ShownText) -> egui::Response {
    let icon = match open {
        true => theme::icon::CARET_DOWN,
        false => theme::icon::CARET_RIGHT,
    };
    let toggle = ui.add(egui::Button::new(icon).frame(false));
    toggle.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::DisclosureTriangle, true, open, name.as_str())
    });
    toggle
}

/// Draws a row of `text` that shows whether it is selected (`on`), noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a selectable row is drawn.
pub(crate) fn selectable(
    ui: &mut egui::Ui,
    on: bool,
    text: impl AsRef<ShownText>,
) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.selectable_label(on, text), text)
}

/// Draws a row of `text` that sets `current` to `value` when clicked and shows whether it
/// holds it, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a row tied to a value is drawn.
pub(crate) fn choice<T: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    value: T,
    text: impl AsRef<ShownText>,
) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.selectable_value(current, value, text), text)
}

/// Draws a radio button of `text`, on when `on`, noted as `button` does.
#[allow(clippy::disallowed_methods)] // The one place a radio button is drawn by its text.
pub(crate) fn radio(ui: &mut egui::Ui, on: bool, text: impl AsRef<ShownText>) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.radio(on, text), text)
}

/// Draws a radio button of `text` that sets `current` to `value` when clicked, noted as
/// `button` does. The response is marked changed when `current` changes, as egui's own
/// radio_value does.
pub(crate) fn radio_choice<T: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    value: T,
    text: impl AsRef<ShownText>,
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
pub(crate) fn checkbox(
    ui: &mut egui::Ui,
    on: &mut bool,
    text: impl AsRef<ShownText>,
) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.checkbox(on, text), text)
}

/// Draws a link of `text`, noted as `button` does. The caller opens what it points to.
#[allow(clippy::disallowed_methods)] // The one place a link is drawn.
pub(crate) fn link(ui: &mut egui::Ui, text: impl AsRef<ShownText>) -> egui::Response {
    let text = text.as_ref().as_str();
    noted(&ui.link(text), text)
}

/// Draws a button of `title` that opens a submenu drawn by `add`, noted by its title as
/// `button` does. Returns the button's response.
#[allow(clippy::disallowed_methods)] // The one place egui's own menu button is drawn.
pub(crate) fn submenu<R>(
    ui: &mut egui::Ui,
    title: impl Into<MenuTitle>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    let title = title.into().0;
    let name = title.text().to_owned();
    noted(&ui.menu_button(title, add).response, &name)
}

/// `text` with the Phosphor `icon` before it, as every row of the menus shows: the icon in the
/// weak colour, the text in the widget's own (Color32::PLACEHOLDER), so a selected row keeps
/// its colours. Phosphor's glyphs are all one em wide, so the texts line up.
pub(crate) fn with_icon(
    ui: &egui::Ui,
    icon: &'static str,
    text: impl AsRef<ShownText>,
) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let icon = ShownText::untranslated(icon);
    job.append_shown(&icon, 0.0, format(ui.visuals().weak_text_color()));
    job.append_shown(text.as_ref(), 8.0, format(egui::Color32::PLACEHOLDER));
    job
}

/// A menu entry showing the action's shortcut; sets `chosen` when clicked.
pub(crate) fn menu_item(ui: &mut egui::Ui, chosen: &mut Option<Action>, action: Action) {
    let text = with_icon(ui, action.icon(), action.label());
    #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
    menu_button(ui, chosen, egui::Button::new(text), action);
}

/// A menu entry with a check mark when `on`.
pub(crate) fn menu_toggle(
    ui: &mut egui::Ui,
    chosen: &mut Option<Action>,
    on: bool,
    action: Action,
) {
    let mut text = with_icon(ui, action.icon(), action.label());
    // On the accent colour of a row that is on, the weak grey would hardly show.
    if on {
        text.sections[0].format.color = egui::Color32::PLACEHOLDER;
    }
    #[allow(clippy::disallowed_methods)] // menu_button notes it by the action's label.
    let button = egui::Button::selectable(on, text);
    menu_button(ui, chosen, button, action);
}

pub(crate) fn menu_button(
    ui: &mut egui::Ui,
    chosen: &mut Option<Action>,
    mut button: egui::Button,
    action: Action,
) {
    if let Some(text) = shortcut_text(ui.ctx(), action) {
        #[allow(clippy::disallowed_methods)] // The one place a shortcut's keys are drawn.
        let with_keys = button.shortcut_text(text);
        button = with_keys;
    }
    if noted(&ui.add(button), &action.label()).clicked() {
        *chosen = Some(action);
    }
}

/// A toolbar button with the action's name and shortcut as tooltip.
pub(crate) fn tool_button(
    ui: &mut egui::Ui,
    chosen: &mut Option<Action>,
    icon: &'static str,
    action: Action,
    on: bool,
) {
    let tip = match shortcut_text(ui.ctx(), action) {
        Some(keys) => tf!("{label} ({keys})", label = action.label(), keys),
        None => action.label(),
    };
    #[allow(clippy::disallowed_methods)] // An icon alone, noted below by the action's label.
    let button = egui::Button::selectable(on, icon);
    let response = ui.add(button).tip(tip);
    if noted(&response, &action.label()).clicked() {
        *chosen = Some(action);
    }
}

/// Opens `path` in the program of `words`, or in the default application when `words` is
/// empty, and waits for that program to exit. A video player stays open while it plays.
fn open_one_in(words: &[String], path: &Path) -> Result<(), ShownText> {
    let Some((name, args)) = words.split_first() else {
        // gio is part of glib, and xdg-open covers systems without it. In a Flatpak, both ask
        // the portal for the default application.
        let name = match external::has_program("gio") {
            true => "gio",
            false => "xdg-open",
        };
        let mut opener = command(name);
        if name == "gio" {
            opener.arg("open");
        }
        opener.arg(path);
        return external::run_telling(name, opener);
    };
    // The user's player runs on the host in a Flatpak.
    let mut player = external::host_command(name);
    player.args(args).arg(path);
    external::run_telling(name, player)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits for `open_with`'s answer for `key`, asking with `ask` if it asks again.
    fn answer(open_with: &mut OpenWith, key: &str, ask: fn() -> Apps) -> Apps {
        let ctx = egui::Context::default();
        for _ in 0..2000 {
            if let Some(apps) = open_with.apps_for_type(key, ask, &ctx) {
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
            .insert("jpg".into(), AppsLookup::Found(Vec::new(), old));
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
