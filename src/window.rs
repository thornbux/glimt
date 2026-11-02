//! The window itself: its size and place remembered, and the screen saver kept off in full screen.

use crate::format::fill_template;
use crate::{external, remote, tf};
use eframe::egui;
use std::path::Path;
use std::sync::atomic::Ordering;

/// How long the mouse is still in full screen before the pointer hides.
const POINTER_STILL: std::time::Duration = std::time::Duration::from_secs(2);

/// What ends a screen saver inhibitor: its process killed, or the portal's request closed.
pub(crate) type Ender = Box<dyn FnOnce() + Send>;

impl crate::App {
    /// Keeps a kiosk in full screen and the screensaver off in full screen, and remembers the
    /// window's size and place; whether the window is full screen.
    pub(crate) fn track_window(&mut self, ctx: &egui::Context) -> bool {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        if self.kiosk && !self.kiosk_quit {
            // Closing the window is refused, and full screen comes back if the desktop ends it.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            if ctx.input(|i| i.viewport().fullscreen) == Some(false) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        }
        // Keywords typed and not yet saved: written now, so closing waits for them.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_meta(ctx);
        }
        // Files still being copied, moved, renamed or saved: closed when they are done, not
        // halfway; a second close stops them after the file at hand, a third does not wait.
        if ctx.input(|i| i.viewport().close_requested()) && self.work_left() && self.closing < 2 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing += 1;
            if self.closing == 2 {
                (self.file_jobs.iter()).for_each(|job| job.stop.store(true, Ordering::Relaxed));
            }
            self.notice = Some(
                match self.closing {
                    1 => "Glimt closes when the file operations are done; close again to stop them",
                    _ => "Stopping after the files at hand; close once more to end at once",
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
        self.window_state.maximized = maximized;
        if !fullscreen && !maximized {
            // In the desktop's units, as the window opens with them: egui's shrink with the
            // size chosen in Preferences, and a window kept at 200 % opened half as large.
            let zoom = ctx.zoom_factor();
            self.window_state.size = Some(size * zoom);
            // Wayland reports no position.
            self.window_state.pos = outer
                .map(|r| (r.min.to_vec2() * zoom).to_pos2())
                .or(self.window_state.pos);
        }
        fullscreen
    }

    /// Keep the screen saver off while `on`: gnome-session-inhibit (GNOME), else systemd-inhibit.
    /// The inhibitor runs `cat` on a pipe from Glimt, so it ends with Glimt even after a crash. In
    /// the Flatpak, which has neither, the inhibit portal, held by a connection of Glimt's.
    pub(crate) fn inhibit(&mut self, on: bool) {
        if on == self.inhibit_on {
            return;
        }
        self.inhibit_on = on;
        let mut held = self.inhibitor.lock().unwrap_or_else(|e| e.into_inner());
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
        let inhibitor = self.inhibitor.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "portals")]
            let portal = (crate::portal::in_flatpak())
                .then(|| crate::portal::inhibit_idle("Glimt full screen"))
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
        self.window_title = template.to_owned();
        self.title(ctx);
    }

    /// "image name - Glimt" (or "Glimt - image name", Preferences); the folder when it has no
    /// images. Or this window's own template, else the one in Preferences, unless its fields
    /// are all empty.
    pub(crate) fn title_text(&self) -> String {
        let (name, path) = match (self.list.files.get(self.list.cur), &self.collection) {
            (Some(_), _) => (
                self.name(self.list.cur).into_owned(),
                self.path(self.list.cur).display().to_string(),
            ),
            (None, Some(file)) => (file.display().to_string(), file.display().to_string()),
            (None, None) => (
                self.dir.display().to_string(),
                self.dir.display().to_string(),
            ),
        };
        // The shown image's own folder: a collection and several folders are listed as "/".
        let folder = match self.list.files.get(self.list.cur) {
            Some(_) => self.path(self.list.cur).parent().map(Path::to_path_buf),
            None => self
                .collection
                .as_ref()
                .and_then(|f| f.parent())
                .map(Path::to_path_buf),
        };
        let folder = folder
            .unwrap_or_else(|| self.dir.clone())
            .display()
            .to_string();
        // A named Glimt says which it is.
        let app = match remote::given_name() {
            Some(n) => tf!("Glimt ({n})", n),
            None => "Glimt".to_owned(),
        };
        let template = match self.window_title.as_str() {
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
            self.pointer_moved = std::time::Instant::now();
        }
        if fullscreen && self.config.hide_pointer && self.working_besides_image() {
            // A job may end without a repaint; look again, for the pointer to hide then.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if !self.pointer_hides(fullscreen) || window_shown(ctx) {
            return;
        }
        match POINTER_STILL.checked_sub(self.pointer_moved.elapsed()) {
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
    use crate::tests::{app_on, contract_folder, settle};

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
        let job = crate::threads::Working::new(&app.busy);
        assert!(!app.pointer_hides(true));
        drop(job);
        app.list(&ctx);
        assert!(app.listing_rx.is_some() && !app.pointer_hides(true));
        assert!(settle(&mut app, &ctx, |a| a.pointer_hides(true)));
        app.preferences_open = true;
        assert!(!app.pointer_hides(true));
        app.preferences_open = false;
        app.config.hide_pointer = false;
        assert!(!app.pointer_hides(true));
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
        app.pointer_moved = std::time::Instant::now();
        assert!(!pass(&mut app, vec![], true));
        app.pointer_moved = long_ago();
        assert!(pass(&mut app, vec![], true));
        // The mouse moves: shown again, and the timer starts anew.
        let moved = |x| vec![Event::PointerMoved(pos2(x, 10.0))];
        let _ = pass(&mut app, moved(10.0), true);
        assert!(!pass(&mut app, moved(20.0), true));
        assert!(app.pointer_moved.elapsed() < super::POINTER_STILL);
        // Outside full screen it never hides, and the timer starts when full screen begins.
        app.pointer_moved = long_ago();
        assert!(!pass(&mut app, vec![], false));
        assert!(!pass(&mut app, vec![], true));
        // A dialog keeps it.
        app.pointer_moved = long_ago();
        app.preferences_open = true;
        assert!(!pass(&mut app, vec![], true));
        app.preferences_open = false;
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
        app.list.files = vec!["a.png".into()];
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
        app.pointer_moved = std::time::Instant::now() - super::POINTER_STILL * 2;
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
        let (hidden, delay) = pass(&mut app);
        assert!(
            hidden && delay > std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        // The shown image decoded does not keep the pointer, so it needs no look again either.
        app.list.files = vec!["a.png".into()];
        assert!(app.working());
        let (hidden, delay) = pass(&mut app);
        assert!(
            hidden && delay > std::time::Duration::from_secs(1),
            "{delay:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
