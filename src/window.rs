//! The window itself: its size and place remembered, and the screen saver kept off in full screen.

use crate::format::fill_template;
use crate::{external, remote, tf};
use eframe::egui;
use std::path::Path;
use std::sync::atomic::Ordering;

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
}

impl crate::App {
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
}
