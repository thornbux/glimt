//! The window itself: its size and place remembered, and the screen saver kept off in full screen.

use crate::files::config::WindowState;
use crate::support::format::fill_template;
use crate::ui::actions::{Action, KIOSK_ACTIONS};
use crate::{features::remote, files::external, tf};
use eframe::egui;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

/// How long the mouse is still in full screen before the pointer hides.
const POINTER_STILL: std::time::Duration = std::time::Duration::from_secs(2);

/// What ends a screen saver inhibitor: its process killed, or the portal's request closed.
pub(crate) type Ender = Box<dyn FnOnce() + Send>;

/// The window: its state remembered, the kiosk, the projector, the screen saver, the pointer and
/// the title.
pub(crate) struct Window {
    /// Window geometry seen in the last frame, saved at exit.
    state: WindowState,
    /// --kiosk: full screen, only the image, only actions that show and browse.
    kiosk: bool,
    /// `--remote quit` came: a kiosk may close.
    kiosk_quit: bool,
    /// Window → Projector: a second window with only the image, following this one.
    projector: bool,
    /// The projector's image area in pixels, which images are decoded to cover too.
    projector_size: [u32; 2],
    /// The screen saver inhibitor running while in full screen, and how many times full screen
    /// was turned on or off: the inhibitor is looked for and started in a thread, and one that
    /// comes after full screen ended again is ended at once.
    inhibitor: Arc<Mutex<(u64, Option<Ender>)>>,
    /// Whether it was asked for: tried once, not on every frame where none could be started.
    inhibit_on: bool,
    /// Since when the pointer has been away from an open menu of the menu bar.
    menu_away: Option<Instant>,
    /// The window had the focus last frame; getting it makes this Glimt the one `--remote`
    /// talks to.
    has_focus: bool,
    /// When the mouse last moved, or full screen began: the pointer hides after `POINTER_STILL`.
    pointer_moved: std::time::Instant,
    /// Window → Set Window Title: this window's title template; empty, the one in Preferences.
    title: String,
    /// Close asked for while files were being written (`work_left`): 1, it waits for them; 2,
    /// they are stopped after the file at hand and it waits for that; a third close ends at once.
    closing: u8,
    /// The image and turn that a close last tried to save. A close saves a turn only when it
    /// differs, so a failed save is not tried again at every close, but a newer turn is saved.
    /// A save that worked forgets it, so the same turn made again is saved too.
    close_saved: Option<(std::path::PathBuf, (u8, bool))>,
}

impl Window {
    /// The window at start, a kiosk's if `kiosk`: held in full screen until --remote quit.
    pub(crate) fn new(kiosk: bool) -> Self {
        Window {
            state: WindowState::default(),
            kiosk,
            kiosk_quit: false,
            projector: false,
            projector_size: [0, 0],
            inhibitor: Arc::default(),
            inhibit_on: false,
            menu_away: None,
            has_focus: true,
            pointer_moved: Instant::now(),
            title: String::new(),
            closing: 0,
            close_saved: None,
        }
    }

    pub(crate) fn is_kiosk(&self) -> bool {
        self.kiosk
    }

    /// `--remote quit` came, or Ctrl+Q where it cannot: a kiosk may close.
    pub(crate) fn let_kiosk_quit(&mut self) {
        self.kiosk_quit = true;
    }

    /// Whether `action` may run: a kiosk shows and browses, and does not change files, settings
    /// or the window. Once it may close, Quit passes too, but nothing else does.
    pub(crate) fn allows(&self, action: Action) -> bool {
        let quits = action == Action::Quit && self.kiosk_quit;
        !self.kiosk || KIOSK_ACTIONS.contains(&action) || quits
    }

    /// Window → Projector, open or closed; its size is learnt anew.
    pub(crate) fn toggle_projector(&mut self) {
        self.projector = !self.projector;
        self.projector_size = [0, 0];
    }

    /// The projector window was closed.
    pub(crate) fn close_projector(&mut self) {
        self.projector = false;
        self.projector_size = [0, 0];
    }

    pub(crate) fn has_projector(&self) -> bool {
        self.projector
    }

    /// The projector's image area in pixels.
    pub(crate) fn projector_size(&self) -> [u32; 2] {
        self.projector_size
    }

    /// The projector's image area is `size` now: the size before when it changed.
    pub(crate) fn projector_resized(&mut self, size: [u32; 2]) -> Option<[u32; 2]> {
        (size != self.projector_size).then(|| std::mem::replace(&mut self.projector_size, size))
    }

    /// The window's size and place, as kept for the next start.
    pub(crate) fn state_text(&self) -> String {
        self.state.to_text()
    }

    /// Window → Set Window Title's template for this window; empty, the one in Preferences.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// How many times closing was asked for while files were being written.
    pub(crate) fn closes_asked(&self) -> u8 {
        self.closing
    }

    /// Forgets the turn a close tried to save, because a save worked. A turn made after it,
    /// also the same one again, is then saved at the next close.
    pub(crate) fn turn_saved(&mut self) {
        self.close_saved = None;
    }

    /// The window has the focus or not; true when it just got it.
    pub(crate) fn focus(&mut self, focus: bool) -> bool {
        let gained = focus && !self.has_focus;
        self.has_focus = focus;
        gained
    }

    /// Closes an open menu of the menu bar when the pointer has stayed away from it.
    pub(crate) fn close_menus(&mut self, ctx: &egui::Context, bar: egui::Rect) {
        crate::ui::menus::close_menus(ctx, bar, &mut self.menu_away);
    }
}

impl crate::App {
    /// Keeps a kiosk in full screen and the screensaver off in full screen, and remembers the
    /// window's size and place; whether the window is full screen.
    pub(crate) fn track_window(&mut self, ctx: &egui::Context) -> bool {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        if self.window.kiosk && !self.window.kiosk_quit {
            // Closing the window is refused, and full screen comes back if the desktop ends it.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            if ctx.input(|i| i.viewport().fullscreen) == Some(false) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        }
        // Keywords typed and not yet saved, and with Save Orientation on a turn of the image
        // shown, are written now, so closing waits for them. The image is never left on the way
        // out, which is where a turn is saved otherwise. A turn is tried once. A save that
        // failed would otherwise start again at the Close sent when it ends, and count as the
        // user's second close.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_meta(ctx);
            let shown = Some((
                self.viewing.turned_file().to_path_buf(),
                self.viewing.orient(),
            ));
            if self.config.save_orientation && self.window.close_saved != shown {
                self.window.close_saved = shown;
                self.save_orientation(ctx);
            }
        }
        // Files still being copied, moved, renamed or saved: closed when they are done, not
        // halfway; a second close stops them after the file at hand, a third does not wait.
        if ctx.input(|i| i.viewport().close_requested())
            && self.work_left()
            && self.window.closing < 2
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.window.closing += 1;
            if self.window.closing == 2 {
                self.file_operations.stop_jobs();
            }
            self.notice = Some(
                match self.window.closing {
                    1 => crate::support::lang::tr(
                        "Glimt closes when the file operations are done; close again to stop them",
                    ),
                    _ => crate::support::lang::tr(
                        "Stopping after the files at hand; close once more to end at once",
                    ),
                }
                .into(),
            );
        }
        self.inhibit(fullscreen && self.config.inhibit_screensaver);
        // Geometry of the normal window only, so leaving full screen or maximized restores it.
        // The size from egui's own area: the viewport's inner_rect needs the window position,
        // which Wayland does not give, so it stayed empty there and the size was never saved.
        let (size, outer, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (
                i.viewport_rect().size(),
                v.outer_rect,
                v.maximized.unwrap_or(false),
            )
        });
        self.window.state.maximized = maximized;
        if !fullscreen && !maximized {
            // In the desktop's units, as the window opens with them: egui's shrink with the
            // size chosen in Preferences, and a window kept at 200 % opened half as large.
            let zoom = ctx.zoom_factor();
            self.window.state.size = Some(size * zoom);
            // Wayland reports no position.
            self.window.state.position = outer
                .map(|r| (r.min.to_vec2() * zoom).to_pos2())
                .or(self.window.state.position);
        }
        fullscreen
    }

    /// Keep the screen saver off while `on`: gnome-session-inhibit (GNOME), else systemd-inhibit.
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash. In
    /// the Flatpak, which has neither, the inhibit portal, held by a connection of Glimt's.
    pub(crate) fn inhibit(&mut self, on: bool) {
        if on == self.window.inhibit_on {
            return;
        }
        self.window.inhibit_on = on;
        let mut held = self
            .window
            .inhibitor
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        held.0 += 1;
        let turn = held.0;
        let old = held.1.take();
        drop(held);
        if let Some(end) = old {
            end();
        }
        if !on {
            return;
        }
        let commands: [&[&str]; 2] = [
            &[
                "gnome-session-inhibit",
                "--inhibit",
                "idle",
                "--reason",
                "Glimt full screen",
                "cat",
            ],
            &[
                "systemd-inhibit",
                "--what=idle",
                "--who=Glimt",
                "--why=Full screen",
                "cat",
            ],
        ];
        // The first that is there, on the host in a Flatpak: asked of the shell, through
        // flatpak-spawn there, so in a thread, not each time full screen starts on the UI thread.
        let inhibitor = self.window.inhibitor.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "portals")]
            let portal = (crate::files::external::in_flatpak())
                .then(|| crate::files::portal::inhibit_idle("Glimt full screen"))
                .and_then(|held| held.map_err(|e| eprintln!("inhibit portal: {e}")).ok());
            #[cfg(not(feature = "portals"))]
            let portal: Option<std::sync::mpsc::Sender<()>> = None;
            let end: Ender = if let Some(held) = portal {
                Box::new(move || drop(held))
            } else {
                let found = commands.iter().find(|c| external::host_has(c[0]));
                let Some(mut child) = found.and_then(|c| {
                    external::host_command(c[0])
                        .args(&c[1..])
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .ok()
                }) else {
                    return;
                };
                Box::new(move || {
                    let _ = child.kill();
                    #[allow(clippy::disallowed_methods)]
                    // killed just now, reaped in its own thread
                    std::thread::spawn(move || child.wait());
                })
            };
            let mut held = inhibitor.lock().unwrap_or_else(|e| e.into_inner());
            // Full screen ended or started again meanwhile: this one is not wanted.
            if held.0 == turn {
                held.1 = Some(end);
            } else {
                drop(held);
                end();
            }
        });
    }

    pub(crate) fn title(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title_text()));
    }

    /// Window → Set Window Title: this window's own template, until it closes; empty, the one
    /// in Preferences again.
    pub(crate) fn set_window_title(&mut self, ctx: &egui::Context, template: &str) {
        self.window.title = template.to_owned();
        self.title(ctx);
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no
    /// images. Or this window's own template, else the one in Preferences, unless its fields
    /// are all empty.
    pub(crate) fn title_text(&self) -> String {
        let (name, path) = match (
            self.list.files().get(self.list.cur()),
            self.folder.collection(),
        ) {
            (Some(_), _) => (
                self.name(self.list.cur()).into_owned(),
                self.path(self.list.cur()).display().to_string(),
            ),
            (None, Some(file)) => (file.display().to_string(), file.display().to_string()),
            (None, None) => (
                self.folder.dir().display().to_string(),
                self.folder.dir().display().to_string(),
            ),
        };
        // The shown image's own folder: a collection and several folders are listed as "/".
        let folder = match self.list.files().get(self.list.cur()) {
            Some(_) => self.path(self.list.cur()).parent().map(Path::to_path_buf),
            None => self
                .folder
                .collection()
                .and_then(|f| f.parent())
                .map(Path::to_path_buf),
        };
        let folder = folder
            .unwrap_or_else(|| self.folder.dir().to_path_buf())
            .display()
            .to_string();
        // A named Glimt says which it is.
        let app = match remote::given_name() {
            Some(n) => tf!("Glimt ({n})", n),
            None => "Glimt".to_owned(),
        };
        let template = match self.window.title.as_str() {
            "" => &self.config.window_title,
            own => own,
        };
        let own = fill_template(template, |field| match field {
            "name" => name.clone(),
            "path" => path.clone(),
            "folder" => folder.clone(),
            "app" => app.clone(),
            _ => String::new(),
        });
        if !own.trim().is_empty() {
            own
        } else if self.config.app_name_first {
            format!("{app} - {name}")
        } else {
            format!("{name} - {app}")
        }
    }

    /// In full screen, with the setting on, the pointer hidden once the mouse has been still for
    /// `POINTER_STILL`; never while work goes on (the busy pointer shows it; the shown image
    /// decoded does not count, or it would flash at each image of a slideshow), nor while a window,
    /// menu or popup shows (Help, the EXIF window, pan view, a dialog): one reads it, and wants
    /// the pointer there at once.
    pub(crate) fn hide_still_pointer(&mut self, ctx: &egui::Context, fullscreen: bool) {
        let moved = ctx.input(|i| {
            i.pointer.delta() != egui::Vec2::ZERO
                || i.pointer.any_down()
                || i.smooth_scroll_delta != egui::Vec2::ZERO
        });
        if moved || !fullscreen {
            self.window.pointer_moved = std::time::Instant::now();
        }
        if fullscreen && self.config.hide_pointer && self.working_besides_image() {
            // A job may end without a repaint; look again, for the pointer to hide then.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if !self.pointer_hides(fullscreen) || window_shown(ctx) {
            return;
        }
        match POINTER_STILL.checked_sub(self.window.pointer_moved.elapsed()) {
            Some(left) if !left.is_zero() => ctx.request_repaint_after(left),
            _ => ctx.set_cursor_icon(egui::CursorIcon::None),
        }
    }

    /// Whether the pointer may be hidden while still: full screen, the setting on, no work going
    /// on but the shown image's decode, and no dialog open.
    pub(crate) fn pointer_hides(&self, fullscreen: bool) -> bool {
        fullscreen
            && self.config.hide_pointer
            && !self.working_besides_image()
            && !self.dialog_open()
    }

    /// Esc: as in Geeqie it never quits. It closes help, goes back to the grid from an image
    /// opened from it, stops the slideshow and leaves full screen.
    pub(crate) fn escape(&mut self, ctx: &egui::Context, fullscreen: bool) {
        self.help = None;
        // A crop belongs to the image shown, so it ends when the grid comes back over it.
        // Otherwise the next Enter in the grid saved the part instead of opening an image.
        if self.panel.back_to_grid() {
            self.viewing.stop_crop();
        }
        if self.slideshow.is_running() {
            self.stop_slideshow(ctx);
        }
        if fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }
}

/// Whether a window, menu or popup was on the screen in this frame or the last; the panels
/// of the main window do not count.
fn window_shown(ctx: &egui::Context) -> bool {
    use egui::Order::{Foreground, Middle};
    ctx.memory(|m| {
        (m.areas().visible_layer_ids().iter()).any(|l| matches!(l.order, Middle | Foreground))
    })
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::{app_on, contract_folder, settle};

    /// A kiosk that may close lets Quit through, but still refuses what changes the settings.
    #[test]
    fn a_kiosk_about_to_quit_stays_locked() {
        use crate::ui::actions::Action;
        let mut window = super::Window::new(true);
        window.let_kiosk_quit();
        assert!(window.allows(Action::Quit));
        assert!(!window.allows(Action::HiddenFiles) && !window.allows(Action::Delete));
        assert!(window.allows(Action::Next));
    }

    /// The window title from the template in Preferences, and the default order when it is
    /// empty or all its fields are.
    #[test]
    fn window_title_from_the_template() {
        let dir = std::env::temp_dir().join(format!("glimt-test-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(4, 4).save(dir.join("a.png")).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert_eq!(app.title_text(), "a.png - Glimt");
        app.config.window_title = "%app%: %name% in %folder%".into();
        assert_eq!(
            app.title_text(),
            format!("Glimt: a.png in {}", dir.display())
        );
        app.config.window_title = "%path%".into();
        assert_eq!(app.title_text(), dir.join("a.png").display().to_string());
        app.config.window_title = "%nothing%".into();
        assert_eq!(app.title_text(), "a.png - Glimt");
        // This window's own title before the one in Preferences, until it is emptied.
        app.config.window_title = "%app%".into();
        app.set_window_title(&ctx, "Left: %name%");
        assert_eq!(app.title_text(), "Left: a.png");
        app.set_window_title(&ctx, "");
        assert_eq!(app.title_text(), "Glimt");
        // In a collection, listed as "/": the image's own folder; parts joined without spaces
        // doubled.
        let gqv = dir.join("c.gqv");
        let text = format!("#GQview collection\n\"{}\"\n", dir.join("a.png").display());
        std::fs::write(&gqv, text).unwrap();
        let (mut col, _) = app_on(&gqv);
        col.config.window_title = "%folder% | %name%".into();
        assert_eq!(col.title_text(), format!("{} · a.png", dir.display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// In full screen the pointer hides while the mouse is still, but not with the setting off,
    /// out of full screen, while work goes on (the list sorted anew after a slideshow, a file
    /// job), or with a dialog open.
    #[test]
    fn hides_the_pointer_only_when_nothing_goes_on() {
        let dir = contract_folder("pointer");
        let (mut app, ctx) = app_on(&dir);
        assert!(settle(&mut app, &ctx, |a| !a.working()));
        app.config.hide_pointer = true;
        assert!(app.pointer_hides(true));
        assert!(!app.pointer_hides(false));
        let job = crate::support::threads::Working::new(&app.busy);
        assert!(!app.pointer_hides(true));
        drop(job);
        app.list(&ctx);
        assert!(app.folder.is_listing() && !app.pointer_hides(true));
        assert!(settle(&mut app, &ctx, |a| a.pointer_hides(true)));
        app.prefs.show();
        assert!(!app.pointer_hides(true));
        app.prefs = Default::default();
        app.config.hide_pointer = false;
        assert!(!app.pointer_hides(true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A turn that cannot be saved is tried once at closing. The Close sent when the save ends
    /// starts no second save, which would count as a second close and stop the jobs.
    #[test]
    fn closing_saves_a_turn_once() {
        use eframe::egui::{RawInput, ViewportEvent, ViewportId, ViewportInfo};
        let dir = std::env::temp_dir().join(format!("glimt-test-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::files::file_io::create_file(&dir.join("a.png"), b"not a PNG").unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.save_orientation = true;
        app.viewing.turn((1, false), dir.join("a.png"));
        let close = |app: &mut crate::App| {
            let info = ViewportInfo {
                events: vec![ViewportEvent::Close],
                ..Default::default()
            };
            let input = RawInput {
                viewports: std::iter::once((ViewportId::ROOT, info)).collect(),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                app.track_window(ui.ctx());
            });
        };
        close(&mut app);
        let a = dir.join("a.png");
        assert!(app.file_operations.is_saving(&a));
        // The save's thread lets go of the file just after it tells the result.
        let over = |app: &crate::App| !app.file_operations.is_saving(&a);
        assert!(settle(&mut app, &ctx, over));
        close(&mut app);
        assert!(!app.file_operations.is_saving(&a));
        // A turn made after that, while the window waits for other work, is saved.
        app.viewing.turn((2, false), a.clone());
        close(&mut app);
        assert!(app.file_operations.is_saving(&a));
        assert!(settle(&mut app, &ctx, over));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A window, a menu or a popup on the screen keeps the pointer; the panels do not.
    #[test]
    fn a_window_on_the_screen_keeps_the_pointer() {
        let ctx = eframe::egui::Context::default();
        let pass = |window: bool| {
            let _ = ctx.run_ui(Default::default(), |ui| {
                eframe::egui::CentralPanel::default().show(ui, |ui| ui.label("image"));
                if window {
                    eframe::egui::Window::new("Help").show(ui.ctx(), |ui| ui.label("text"));
                }
            });
            super::window_shown(&ctx)
        };
        assert!(!pass(false) && !pass(false));
        assert!(pass(true));
        // The frame after it closed still counts it (shown last frame), the next not.
        let _ = pass(false);
        assert!(!pass(false));
    }

    /// In full screen the pointer hides once still for `POINTER_STILL`, and comes back when the
    /// mouse moves, outside full screen, or while a dialog is open.
    #[test]
    fn a_still_pointer_hides_after_the_timer() {
        use eframe::egui::{CursorIcon, Event, RawInput, pos2};
        let dir = std::env::temp_dir().join(format!("glimt-test-pointer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        let pass = |app: &mut crate::App, events: Vec<Event>, fullscreen: bool| {
            let input = RawInput {
                events,
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| app.hide_still_pointer(ui.ctx(), fullscreen));
            out.platform_output.cursor_icon == CursorIcon::None
        };
        let long_ago = || std::time::Instant::now() - super::POINTER_STILL * 2;
        // Just moved: shown, until the timer has run.
        app.window.pointer_moved = std::time::Instant::now();
        assert!(!pass(&mut app, vec![], true));
        app.window.pointer_moved = long_ago();
        assert!(pass(&mut app, vec![], true));
        // The mouse moves: shown again, and the timer starts anew.
        let moved = |x| vec![Event::PointerMoved(pos2(x, 10.0))];
        let _ = pass(&mut app, moved(10.0), true);
        assert!(!pass(&mut app, moved(20.0), true));
        assert!(app.window.pointer_moved.elapsed() < super::POINTER_STILL);
        // Outside full screen it never hides, and the timer starts when full screen begins.
        app.window.pointer_moved = long_ago();
        assert!(!pass(&mut app, vec![], false));
        assert!(!pass(&mut app, vec![], true));
        // A dialog keeps it.
        app.window.pointer_moved = long_ago();
        app.prefs.show();
        assert!(!pass(&mut app, vec![], true));
        app.prefs = Default::default();
        assert!(pass(&mut app, vec![], true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The shown image still decoded keeps the busy pointer, but not the pointer from hiding in
    /// full screen: a slideshow would flash it at each image not preloaded.
    #[test]
    fn the_shown_image_decoded_lets_the_pointer_hide() {
        let dir = std::env::temp_dir().join(format!("glimt-test-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        app.list.set_files(vec!["a.png".into()]);
        assert!(app.working() && app.pointer_hides(true));
        app.busy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(!app.pointer_hides(true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Work in full screen keeps the pointer, and the window looks again soon, so the pointer
    /// hides when the work ends even if nothing else repaints.
    #[test]
    fn the_pointer_hides_after_work_without_a_repaint() {
        let dir = std::env::temp_dir().join(format!("glimt-test-work-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        app.config.hide_pointer = true;
        app.window.pointer_moved = std::time::Instant::now() - super::POINTER_STILL * 2;
        let pass = |app: &mut crate::App| {
            let out = ctx.run_ui(Default::default(), |ui| {
                app.hide_still_pointer(ui.ctx(), true)
            });
            let delay = out.viewport_output[&eframe::egui::ViewportId::ROOT].repaint_delay;
            (
                out.platform_output.cursor_icon == eframe::egui::CursorIcon::None,
                delay,
            )
        };
        // A thread the window started (the desktop's look, the folder's sizes) asks for one
        // repaint when it ends, which can come at any pass. So a pass that waits is looked for
        // among a few, and only a window that never waits fails.
        let waits = |app: &mut crate::App| {
            (0..5)
                .map(|_| pass(app))
                .find(|&(_, d)| d > std::time::Duration::from_secs(1))
        };
        // egui repaints at once for a few passes after a change (the first, the cursor).
        app.busy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..3 {
            let _ = pass(&mut app);
        }
        let (hidden, delay) = pass(&mut app);
        assert!(
            !hidden && delay <= std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        app.busy.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        assert!(waits(&mut app).is_some_and(|(hidden, _)| hidden));
        // The shown image decoded does not keep the pointer, so it needs no look again either.
        app.list.set_files(vec!["a.png".into()]);
        assert!(app.working());
        assert!(waits(&mut app).is_some_and(|(hidden, _)| hidden));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
