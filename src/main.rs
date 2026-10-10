//! glimt: rask og minnegjerrig bildefremviser.
//!
//! Bilder dekodes i bakgrunnstråder til skjermstørrelse; full oppløsning hentes bare
//! når brukeren zoomer forbi det skjermversjonen dekker. Bare et lite vindu rundt
//! gjeldende bilde holdes i minnet.

use eframe::egui;
use image::ImageDecoder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "tif", "tiff"];
/// Antall bilder som holdes dekodet på hver side av det viste.
const AHEAD: usize = 2;

struct Decoded {
    idx: usize,
    full: bool,
    result: Result<(egui::ColorImage, [usize; 2]), String>,
}

struct Slot {
    tex: egui::TextureHandle,
    full: bool,
    /// Originalstørrelse etter EXIF-rotasjon.
    orig: [usize; 2],
}

struct App {
    files: Arc<Vec<PathBuf>>,
    cur: usize,
    current: Arc<AtomicUsize>,
    /// Maks dekodestørrelse i fysiske piksler: [skjerm, maks teksturside].
    limits: Arc<Mutex<[[u32; 2]; 2]>>,
    /// (bildeindeks, full oppløsning)
    jobs: mpsc::Sender<(usize, bool)>,
    done: mpsc::Receiver<Decoded>,
    slots: HashMap<usize, Slot>,
    pending: HashSet<(usize, bool)>,
    errors: HashMap<usize, String>,
    /// Fysiske skjermpiksler per originalpiksel; `None` = tilpass til vinduet.
    zoom: Option<f32>,
    pan: egui::Vec2,
    /// Startet ved oppstart; tas ut når første bilde vises.
    started: Option<Instant>,
}

fn list_images(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    v.sort();
    v
}

fn decode(path: &Path, max: [u32; 2]) -> Result<(egui::ColorImage, [usize; 2]), String> {
    let mut dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let orig = [img.width() as usize, img.height() as usize];
    if img.width() > max[0] || img.height() > max[1] {
        img = img.thumbnail(max[0], max[1]);
    }
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), orig))
}

impl App {
    fn new(cc: &eframe::CreationContext, files: Vec<PathBuf>, start: usize) -> Self {
        let files = Arc::new(files);
        let current = Arc::new(AtomicUsize::new(start));
        let limits = Arc::new(Mutex::new([[1920, 1080], [8192, 8192]]));
        let (jobs, job_rx) = mpsc::channel::<(usize, bool)>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        for _ in 0..workers {
            let (files, current, limits, job_rx, done_tx, ctx) = (
                files.clone(),
                current.clone(),
                limits.clone(),
                job_rx.clone(),
                done_tx.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, full)) = job_rx.lock().unwrap().recv() else { return };
                    // Avbrudd: hopp over bilder brukeren allerede har bladd forbi.
                    let cur = current.load(Ordering::Relaxed);
                    if idx.abs_diff(cur) > AHEAD || (full && idx != cur) {
                        let _ = done_tx.send(Decoded { idx, full, result: Err(String::new()) });
                        continue;
                    }
                    let max = limits.lock().unwrap()[full as usize];
                    let result = decode(&files[idx], max);
                    let _ = done_tx.send(Decoded { idx, full, result });
                    ctx.request_repaint();
                }
            });
        }
        let mut app = App {
            files,
            cur: start,
            current,
            limits,
            jobs,
            done,
            slots: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            zoom: None,
            pan: egui::Vec2::ZERO,
            started: Some(Instant::now()),
        };
        app.go(&cc.egui_ctx, start);
        app
    }

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            let _ = self.jobs.send((idx, full));
        }
    }

    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        self.cur = idx.min(self.files.len() - 1);
        self.current.store(self.cur, Ordering::Relaxed);
        self.zoom = None;
        self.pan = egui::Vec2::ZERO;
        let cur = self.cur;
        // Kast teksturer utenfor vinduet, og fullversjoner av andre bilder enn det viste.
        self.slots.retain(|&i, s| i.abs_diff(cur) <= AHEAD && (!s.full || i == cur));
        self.request(cur, false);
        for d in 1..=AHEAD {
            if cur + d < self.files.len() {
                self.request(cur + d, false);
            }
            if let Some(i) = cur.checked_sub(d) {
                self.request(i, false);
            }
        }
        let name = self.files[cur].file_name().unwrap_or_default().to_string_lossy();
        let title = format!("{name} ({}/{}) — glimt", cur + 1, self.files.len());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&(d.idx, d.full));
            let wanted = d.idx.abs_diff(self.cur) <= AHEAD && (!d.full || d.idx == self.cur);
            match d.result {
                Ok((img, orig)) if wanted => {
                    // En skjermversjon skal ikke erstatte en fullversjon som allerede er lastet.
                    if !d.full && self.slots.get(&d.idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let tex = ctx.load_texture(format!("img{}", d.idx), img, egui::TextureOptions::LINEAR);
                    self.slots.insert(d.idx, Slot { tex, full: d.full, orig });
                    if d.idx == self.cur && let Some(t) = self.started.take() {
                        eprintln!("første bilde: {} ms", t.elapsed().as_millis());
                    }
                }
                Err(e) if !e.is_empty() => {
                    self.errors.insert(d.idx, e);
                }
                _ => {}
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context, fit: f32) {
        use egui::Key;
        let last = self.files.len().saturating_sub(1);
        let pressed = |k| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::ArrowRight) || pressed(Key::Space) || pressed(Key::PageDown) {
            self.go(ctx, (self.cur + 1).min(last));
        } else if pressed(Key::ArrowLeft) || pressed(Key::Backspace) || pressed(Key::PageUp) {
            self.go(ctx, self.cur.saturating_sub(1));
        } else if pressed(Key::Home) {
            self.go(ctx, 0);
        } else if pressed(Key::End) {
            self.go(ctx, last);
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        if pressed(Key::F) || pressed(Key::F11) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        if pressed(Key::Escape) {
            if fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if pressed(Key::Num1) {
            self.zoom = Some(1.0);
        }
        if pressed(Key::Num0) || pressed(Key::Z) {
            self.zoom = None;
            self.pan = egui::Vec2::ZERO;
        }
        let step = if pressed(Key::Plus) || pressed(Key::Equals) {
            1.25
        } else if pressed(Key::Minus) {
            0.8
        } else {
            1.0
        };
        let wheel = ctx.input(|i| i.zoom_delta() * (1.0 + i.smooth_scroll_delta.y / 400.0));
        let factor = step * wheel;
        if factor != 1.0 {
            self.zoom = Some((self.zoom.unwrap_or(fit) * factor).clamp(0.02, 32.0));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let screen = ctx.input(|i| i.viewport().monitor_size.unwrap_or(i.viewport_rect().size()));
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        *self.limits.lock().unwrap() = [
            [(screen.x * ppp) as u32, (screen.y * ppp) as u32],
            [max_side, max_side],
        ];
        self.receive(ctx);

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ui, |ui| {
            let area = ui.max_rect();
            let response = ui.allocate_rect(area, egui::Sense::drag());
            let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                (area.width() * ppp / s.orig[0] as f32).min(area.height() * ppp / s.orig[1] as f32).min(1.0)
            });
            self.keys(ctx, fit);
            if response.dragged() {
                self.pan += response.drag_delta();
            }
            let scale = self.zoom.unwrap_or(fit);

            if let Some(slot) = self.slots.get(&self.cur) {
                // Skjermversjonen holder ikke ved denne zoomen: hent full oppløsning.
                if !slot.full && scale > slot.tex.size()[0] as f32 / slot.orig[0] as f32 + 1e-3 {
                    self.request(self.cur, true);
                }
                let slot = &self.slots[&self.cur];
                let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                egui::Image::new(&slot.tex).paint_at(ui, rect);
            } else {
                let text = if self.files.is_empty() {
                    "Ingen bilder i mappa".to_owned()
                } else if let Some(e) = self.errors.get(&self.cur) {
                    format!("Kunne ikke åpne {}:\n{e}", self.files[self.cur].display())
                } else {
                    String::new()
                };
                ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, text,
                    egui::FontId::proportional(16.0), egui::Color32::GRAY);
            }
        });
    }
}

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| ".".into());
    let (dir, file) = if arg.is_dir() {
        (arg, None)
    } else {
        (arg.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf(), Some(arg))
    };
    let files = list_images(&dir);
    let start = file
        .and_then(|f| f.canonicalize().ok())
        .and_then(|f| files.iter().position(|p| p.canonicalize().ok().as_ref() == Some(&f)))
        .unwrap_or(0);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("glimt").with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native("glimt", options, Box::new(move |cc| Ok(Box::new(App::new(cc, files, start)))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_images_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join("glimt-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200).save(dir.join("a.png")).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(super::list_images(&dir), vec![dir.join("a.png")]);
        let (img, orig) = super::decode(&dir.join("a.png"), [100, 100]).unwrap();
        assert_eq!((img.size, orig), ([100, 50], [400, 200]));
    }
}
