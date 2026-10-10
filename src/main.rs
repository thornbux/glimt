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
/// Standard antall bilder som holdes dekodet på hver side av det viste.
const DEFAULT_AHEAD: usize = 2;
/// Standard sekunder mellom bildene i lysbildefremvisning.
const DEFAULT_DELAY: u64 = 3;
const MAX_DELAY: u64 = 60;

struct Decoded {
    idx: usize,
    full: bool,
    /// Filen jobben gjaldt; avviser resultater fra en mappe som er byttet ut.
    path: PathBuf,
    /// `None` = hoppet over fordi brukeren har bladd forbi.
    result: Option<Result<(egui::ColorImage, [usize; 2]), String>>,
}

struct Slot {
    tex: egui::TextureHandle,
    /// Filen teksturen er laget fra; avgjør om den kan beholdes når fillista byttes.
    path: PathBuf,
    full: bool,
    /// Originalstørrelse etter EXIF-rotasjon.
    orig: [usize; 2],
}

struct App {
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    cur: usize,
    current: Arc<AtomicUsize>,
    /// Antall bilder forhåndslastet på hver side (Innstillinger).
    ahead: Arc<AtomicUsize>,
    show_settings: bool,
    config: Config,
    /// Maks dekodestørrelse i fysiske piksler: [skjerm, maks teksturside].
    limits: Arc<Mutex<[[u32; 2]; 2]>>,
    /// (bildeindeks, full oppløsning, fil)
    jobs: mpsc::Sender<(usize, bool, PathBuf)>,
    done: mpsc::Receiver<Decoded>,
    slots: HashMap<usize, Slot>,
    pending: HashSet<(usize, bool)>,
    errors: HashMap<usize, String>,
    /// Fysiske skjermpiksler per originalpiksel; `None` = tilpass til vinduet.
    zoom: Option<f32>,
    pan: egui::Vec2,
    /// Startet ved oppstart; tas ut når første bilde vises.
    started: Option<Instant>,
    /// Svar fra en åpen fildialog (zenity).
    dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Mappelesing i bakgrunnen: (undermapper, bilder).
    listing: Option<mpsc::Receiver<(Vec<PathBuf>, Vec<PathBuf>)>>,
    /// Rull trevisningen til gjeldende bilde ved neste tegning.
    scroll_to_cur: bool,
    /// Lysbildefremvisning: når forrige bilde ble vist; `None` = av.
    slideshow: Option<Instant>,
    /// Fillista er stokket for tilfeldig fremvisning.
    random: bool,
}

/// Én gjennomgang av mappa: (undermapper, bilder), begge sortert på navn.
/// Filtypen kommer fra katalogoppføringen, så det blir ingen `stat` per fil (bare symlenker slås opp).
fn list_folder(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext.is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str())) {
            images.push(path);
        } else if !entry.file_name().as_encoded_bytes().starts_with(b".")
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()))
        {
            dirs.push(path);
        }
    }
    sort_by_name(&mut dirs);
    sort_by_name(&mut images);
    (dirs, images)
}

/// Stier i samme mappe: bytevis sammenligning av hele stien gir navnerekkefølge, og er
/// ~9× raskere enn Path::cmp/file_name(), som tolker stien på nytt i hver sammenligning.
fn sort_by_name(paths: &mut [PathBuf]) {
    paths.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
}

/// `$XDG_CONFIG_HOME/glimt/config`, ellers `~/.config/glimt/config`.
fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("glimt").join("config"))
}

/// Innstillinger, lagret som `nøkkel=verdi`-linjer. Ukjente eller ugyldige linjer ignoreres.
#[derive(Debug, PartialEq)]
struct Config {
    preload: usize,
    /// Sekunder mellom bildene i lysbildefremvisning.
    delay: u64,
    /// Start i mappa fra forrige gang når ingen mappe er gitt på kommandolinja.
    remember_dir: bool,
    last_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            preload: DEFAULT_AHEAD,
            delay: DEFAULT_DELAY,
            remember_dir: false,
            last_dir: None,
        }
    }
}

impl Config {
    fn parse(text: &str) -> Self {
        let mut c = Config::default();
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            let value = value.trim();
            match key.trim() {
                "preload" => c.preload = value.parse().map_or(c.preload, |n: usize| n.min(20)),
                "delay" => {
                    c.delay = value
                        .parse()
                        .map_or(c.delay, |n: u64| n.clamp(1, MAX_DELAY))
                }
                "remember_dir" => c.remember_dir = value == "true",
                "last_dir" if !value.is_empty() => c.last_dir = Some(value.into()),
                _ => {}
            }
        }
        c
    }

    fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Config::default, |text| Config::parse(&text))
    }

    fn save(&self) {
        let Some(p) = config_path() else { return };
        // ponytail: stien lagres som UTF-8 (tapsfritt for vanlige stier); bytt til OsString-bytes ved behov.
        let mut text = format!(
            "preload={}\ndelay={}\nremember_dir={}\n",
            self.preload, self.delay, self.remember_dir
        );
        if let Some(dir) = &self.last_dir {
            text += &format!("last_dir={}\n", dir.display());
        }
        let result =
            std::fs::create_dir_all(p.parent().unwrap()).and_then(|_| std::fs::write(&p, text));
        if let Err(e) = result {
            eprintln!("kunne ikke lagre {}: {e}", p.display());
        }
    }
}

/// Fisher–Yates med xorshift seedet fra std sin tilfeldige hash-nøkkel (ingen rand-avhengighet).
fn shuffle<T>(v: &mut [T]) {
    use std::hash::{BuildHasher, Hasher};
    let mut x = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn decode(path: &Path, max: [u32; 2]) -> Result<(egui::ColorImage, [usize; 2]), String> {
    let mut dec = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let orig = [img.width() as usize, img.height() as usize];
    if img.width() > max[0] || img.height() > max[1] {
        img = img.thumbnail(max[0], max[1]);
    }
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((
        egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
        orig,
    ))
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>) -> Self {
        let current = Arc::new(AtomicUsize::new(0));
        let config = Config::load();
        let path = path
            .or_else(|| {
                config
                    .last_dir
                    .clone()
                    .filter(|d| config.remember_dir && d.is_dir())
            })
            .unwrap_or_else(|| ".".into());
        let ahead = Arc::new(AtomicUsize::new(config.preload));
        let limits = Arc::new(Mutex::new([[1920, 1080], [8192, 8192]]));
        let (jobs, job_rx) = mpsc::channel::<(usize, bool, PathBuf)>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        for _ in 0..workers {
            let (current, ahead, limits, job_rx, done_tx, ctx) = (
                current.clone(),
                ahead.clone(),
                limits.clone(),
                job_rx.clone(),
                done_tx.clone(),
                cc.egui_ctx.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let Ok((idx, full, path)) = job_rx.lock().unwrap().recv() else {
                        return;
                    };
                    // Avbrudd: hopp over bilder brukeren allerede har bladd forbi.
                    let cur = current.load(Ordering::Relaxed);
                    if idx.abs_diff(cur) > ahead.load(Ordering::Relaxed) || (full && idx != cur) {
                        let _ = done_tx.send(Decoded {
                            idx,
                            full,
                            path,
                            result: None,
                        });
                        continue;
                    }
                    let max = limits.lock().unwrap()[full as usize];
                    let result = Some(decode(&path, max));
                    let _ = done_tx.send(Decoded {
                        idx,
                        full,
                        path,
                        result,
                    });
                    ctx.request_repaint();
                }
            });
        }
        let mut app = App {
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            current,
            ahead,
            show_settings: false,
            config,
            slideshow: None,
            random: false,
            limits,
            jobs,
            done,
            slots: HashMap::new(),
            pending: HashSet::new(),
            errors: HashMap::new(),
            zoom: None,
            pan: egui::Vec2::ZERO,
            started: Some(Instant::now()),
            dialog: None,
            listing: None,
            scroll_to_cur: true,
        };
        app.open(&cc.egui_ctx, path);
        // Små mapper er lest før første frame; store holder ikke oppstarten igjen mer enn 50 ms.
        app.receive_listing(&cc.egui_ctx, std::time::Duration::from_millis(50));
        app
    }

    /// Åpne en mappe, eller mappa til en fil med den fila valgt.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (dir, file) = if path.is_dir() {
            (path, None)
        } else {
            (
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
                Some(path),
            )
        };
        // Kanonisk sti én gang her, så ⬆ .. og «husk mappa» ikke trenger systemkall senere.
        let dir = dir.canonicalize().unwrap_or(dir);
        // Vis en oppgitt fil med en gang; mappa leses i bakgrunnen og fletter inn lista etterpå.
        let shown = file
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| dir.join(n));
        self.subdirs.clear();
        self.random = false;
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        let (tx, rx) = mpsc::channel();
        let (list_dir, ctx2) = (dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            // Mottakeren er borte hvis en annen mappe er åpnet i mellomtiden; da forkastes svaret.
            let _ = tx.send(list_folder(&list_dir));
            ctx2.request_repaint();
        });
        self.listing = Some(rx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} — glimt",
            dir.display()
        )));
        self.dir = dir;
    }

    /// Ta imot mappelista hvis den er klar, eventuelt etter å ha ventet opptil `wait`.
    fn receive_listing(&mut self, ctx: &egui::Context, wait: std::time::Duration) {
        let Some(Ok((dirs, files))) = self.listing.as_ref().map(|rx| rx.recv_timeout(wait)) else {
            return;
        };
        self.listing = None;
        self.subdirs = dirs;
        self.random = false;
        let shown = self.files.get(self.cur).cloned();
        self.replace_files(ctx, files, shown);
    }

    /// Bytt fillista og gå til `shown` (ellers første bilde). Teksturen som vises nå beholdes
    /// hvis den er laget fra samme fil. All indeksbasert tilstand nullstilles; jobber i arbeid
    /// avvises på filsti når de kommer.
    fn replace_files(&mut self, ctx: &egui::Context, files: Vec<PathBuf>, shown: Option<PathBuf>) {
        let slot = self.slots.remove(&self.cur);
        self.files = files;
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        let cur = shown
            .and_then(|p| self.files.iter().position(|f| *f == p))
            .unwrap_or(0);
        if let Some(slot) = slot.filter(|s| self.files.get(cur) == Some(&s.path)) {
            self.slots.insert(cur, slot);
        }
        self.go(ctx, cur);
    }

    /// Vis zenitys fildialog i en egen tråd så vinduet ikke fryser.
    fn pick(&mut self, ctx: &egui::Context, folder: bool) {
        use std::os::unix::ffi::OsStringExt;
        let (tx, rx) = mpsc::channel();
        let (ctx, start) = (ctx.clone(), self.dir.join(""));
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("zenity");
            cmd.arg("--file-selection").arg("--filename").arg(start);
            if folder {
                cmd.arg("--directory");
            }
            let picked = cmd
                .output()
                .map_err(|e| eprintln!("zenity: {e}"))
                .ok()
                .filter(|o| o.status.success())
                .map(|mut o| {
                    o.stdout.pop_if(|b| *b == b'\n');
                    PathBuf::from(std::ffi::OsString::from_vec(o.stdout))
                });
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.dialog = Some(rx);
    }

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            let _ = self.jobs.send((idx, full, self.files[idx].clone()));
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
        self.scroll_to_cur = true;
        self.refill();
        self.title(ctx);
    }

    /// Stokk fillista (gjeldende bilde først) eller sorter den tilbake, og behold bildet som vises.
    fn set_random(&mut self, ctx: &egui::Context, on: bool) {
        if self.files.is_empty() {
            return;
        }
        let mut files = std::mem::take(&mut self.files);
        let shown = files[self.cur].clone();
        if on {
            files.swap(0, self.cur);
            shuffle(&mut files[1..]);
        } else {
            sort_by_name(&mut files);
        }
        self.random = on;
        self.replace_files(ctx, files, Some(shown));
    }

    fn start_slideshow(&mut self, ctx: &egui::Context, random: bool) {
        if random != self.random {
            self.set_random(ctx, random);
        }
        self.slideshow = Some(Instant::now());
        self.title(ctx);
    }

    fn stop_slideshow(&mut self, ctx: &egui::Context) {
        self.slideshow = None;
        if self.random {
            self.set_random(ctx, false);
        }
        self.title(ctx);
    }

    fn title(&self, ctx: &egui::Context) {
        let Some(file) = self.files.get(self.cur) else {
            return;
        };
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let show = match (self.slideshow.is_some(), self.random) {
            (true, true) => format!("🔀 {} s · ", self.config.delay),
            (true, false) => format!("▶ {} s · ", self.config.delay),
            _ => String::new(),
        };
        let (n, total) = (self.cur + 1, self.files.len());
        let title = format!("{show}{name} ({n}/{total}) — glimt");
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Bytt bilde når tiden er ute. Venter på et forhåndslastet bilde som fortsatt
    /// dekodes, så fremvisningen ikke viser svart skjerm.
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some(last) = self.slideshow else { return };
        if self.files.is_empty() {
            return;
        }
        let next = (self.cur + 1) % self.files.len();
        let delay = std::time::Duration::from_secs(self.config.delay);
        let left = delay.saturating_sub(last.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
        } else if self.random && next == 0 && self.files.len() > 2 {
            // Ny runde: stokk på nytt i stedet for å gjenta samme rekkefølge.
            self.set_random(ctx, true);
        } else if !self.pending.contains(&(next, false)) {
            self.go(ctx, next);
            self.slideshow = Some(Instant::now());
        }
    }

    /// Kast teksturer utenfor forhåndslastingsvinduet, og fullversjoner av andre bilder
    /// enn det viste; be om de som mangler.
    fn refill(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let (cur, ahead) = (self.cur, self.ahead.load(Ordering::Relaxed));
        self.slots
            .retain(|&i, s| i.abs_diff(cur) <= ahead && (!s.full || i == cur));
        self.request(cur, false);
        for d in 1..=ahead {
            if cur + d < self.files.len() {
                self.request(cur + d, false);
            }
            if let Some(i) = cur.checked_sub(d) {
                self.request(i, false);
            }
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(d) = self.done.try_recv() {
            self.pending.remove(&(d.idx, d.full));
            let wanted = self.files.get(d.idx) == Some(&d.path)
                && d.idx.abs_diff(self.cur) <= self.ahead.load(Ordering::Relaxed)
                && (!d.full || d.idx == self.cur);
            match d.result {
                Some(Ok((img, orig))) if wanted => {
                    // En skjermversjon skal ikke erstatte en fullversjon som allerede er lastet.
                    if !d.full && self.slots.get(&d.idx).is_some_and(|s| s.full) {
                        continue;
                    }
                    let tex = ctx.load_texture(
                        format!("img{}", d.idx),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.slots.insert(
                        d.idx,
                        Slot {
                            tex,
                            path: d.path,
                            full: d.full,
                            orig,
                        },
                    );
                    if d.idx == self.cur
                        && let Some(t) = self.started.take()
                    {
                        eprintln!("første bilde: {} ms", t.elapsed().as_millis());
                    }
                }
                Some(Err(e)) if wanted => {
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
        // S = i rekkefølge, R = tilfeldig; samme tast igjen stopper, den andre bytter modus.
        if pressed(Key::S) || pressed(Key::R) {
            let random = pressed(Key::R);
            if self.slideshow.is_some() && self.random == random {
                self.stop_slideshow(ctx);
            } else {
                self.start_slideshow(ctx, random);
            }
        }
        // ↑/↓ justerer tiden mens fremvisningen går.
        let step = pressed(Key::ArrowUp) as i64 - pressed(Key::ArrowDown) as i64;
        if self.slideshow.is_some() && step != 0 {
            self.config.delay = self
                .config
                .delay
                .saturating_add_signed(step)
                .clamp(1, MAX_DELAY);
            self.config.save();
            self.title(ctx);
        }
        if pressed(Key::Escape) {
            if self.slideshow.is_some() {
                self.stop_slideshow(ctx);
            } else if fullscreen {
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
    /// Siste mappe skrives én gang ved avslutning, ikke ved hvert mappebytte.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.config.remember_dir {
            self.config.last_dir = Some(self.dir.clone());
            self.config.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let screen = ctx.input(|i| {
            i.viewport()
                .monitor_size
                .unwrap_or(i.viewport_rect().size())
        });
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        *self.limits.lock().unwrap() = [
            [(screen.x * ppp) as u32, (screen.y * ppp) as u32],
            [max_side, max_side],
        ];
        self.receive(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.slideshow_tick(ctx);
        if let Some(Ok(picked)) = self.dialog.as_ref().map(mpsc::Receiver::try_recv) {
            self.dialog = None;
            if let Some(path) = picked {
                self.open(ctx, path);
            }
        }

        // Fullskjerm som i Geeqie: bare bildet, ingen meny eller paneler.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
        if !fullscreen {
            egui::Panel::top("meny").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("Fil", |ui| {
                        if ui.button("Åpne fil…").clicked() {
                            self.pick(ctx, false);
                        }
                        if ui.button("Åpne mappe…").clicked() {
                            self.pick(ctx, true);
                        }
                        ui.separator();
                        if ui.button("Lukk").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.menu_button("Rediger", |ui| {
                        if ui.button("Innstillinger…").clicked() {
                            self.show_settings = true;
                        }
                    });
                    ui.menu_button("Visning", |ui| {
                        if ui.button("Fullskjerm (F, F11, Esc ut)").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                        }
                    });
                });
            });
        }

        let mut open = self.show_settings;
        egui::Window::new("Innstillinger")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let mut n = self.ahead.load(Ordering::Relaxed);
                let slider = egui::Slider::new(&mut n, 0..=20).text("bilder på hver side");
                ui.label("Forhåndslast neste og forrige bilde:");
                if ui.add(slider).changed() {
                    self.ahead.store(n, Ordering::Relaxed);
                    self.refill();
                    self.config.preload = n;
                    self.config.save();
                }
                let [w, h] = self.limits.lock().unwrap()[0];
                let mb = (2 * n as u64 + 1) * w as u64 * h as u64 * 4 / 1_000_000;
                ui.weak(format!(
                    "Opptil ≈ {mb} MB med bilder i skjermoppløsning {w}×{h}"
                ));
                ui.separator();
                ui.label("Lysbildefremvisning (S = i rekkefølge, R = tilfeldig, ↑/↓ = tid):");
                let delay = egui::Slider::new(&mut self.config.delay, 1..=MAX_DELAY)
                    .text("sekunder per bilde");
                if ui.add(delay).changed() {
                    self.config.save();
                    self.title(ctx);
                }
                ui.separator();
                let remember = egui::Checkbox::new(
                    &mut self.config.remember_dir,
                    "Start i mappa fra forrige gang",
                );
                if ui.add(remember).changed() {
                    self.config.save();
                }
            });
        self.show_settings = open;

        // Venstre side som i Geeqie: mappetre i ett nivå øverst, filnavn under.
        let (mut open_dir, mut go_to) = (None, None);
        if !fullscreen {
            egui::Panel::left("venstre")
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| {
                    egui::Panel::top("mapper")
                        .resizable(true)
                        .default_size(200.0)
                        .show(ui, |ui| {
                            // Rad 0 er ⬆ .. (når det finnes en foreldermappe), resten undermapper.
                            let up = self.dir.parent().map(Path::to_path_buf);
                            let rows = up.is_some() as usize + self.subdirs.len();
                            let row_h = ui.spacing().interact_size.y;
                            egui::ScrollArea::vertical()
                                .id_salt("mapper")
                                .auto_shrink(false)
                                .show_rows(ui, row_h, rows, |ui, range| {
                                    for row in range {
                                        let (label, target) = match (row, &up) {
                                            (0, Some(parent)) => ("⬆ ..".to_owned(), parent),
                                            _ => {
                                                let d = &self.subdirs[row - up.is_some() as usize];
                                                let name = d.file_name().unwrap_or_default();
                                                (format!("📁 {}", name.to_string_lossy()), d)
                                            }
                                        };
                                        if ui.selectable_label(false, label).clicked() {
                                            open_dir = Some(target.clone());
                                        }
                                    }
                                });
                        });
                    // Bare synlige rader tegnes, så lista er like rask med 10 som med 300 000 filer.
                    let row_h = ui.spacing().interact_size.y;
                    let pitch = row_h + ui.spacing().item_spacing.y;
                    let mut area = egui::ScrollArea::vertical()
                        .id_salt("filer")
                        .auto_shrink(false);
                    if self.scroll_to_cur {
                        // Rull bare når gjeldende bilde er utenfor synsfeltet, og legg det midt i.
                        let (top, view) = (self.cur as f32 * pitch, ui.available_height());
                        let off =
                            egui::scroll_area::State::load(ctx, ui.make_persistent_id("filer"))
                                .map_or(0.0, |s| s.offset.y);
                        if top < off || top + pitch > off + view {
                            area = area.vertical_scroll_offset((top - view / 2.0).max(0.0));
                        }
                    }
                    area.show_rows(ui, row_h, self.files.len(), |ui, rows| {
                        for i in rows {
                            let name = self.files[i]
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy();
                            if ui.selectable_label(i == self.cur, name).clicked() {
                                go_to = Some(i);
                            }
                        }
                    });
                });
        }
        self.scroll_to_cur = false;
        if let Some(d) = open_dir {
            self.open(ctx, d);
        } else if let Some(i) = go_to {
            self.go(ctx, i);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    (area.width() * ppp / s.orig[0] as f32)
                        .min(area.height() * ppp / s.orig[1] as f32)
                        .min(1.0)
                });
                // Taster går til innstillingsvinduet mens det er åpent.
                if !self.show_settings {
                    self.keys(ctx, fit);
                }
                if response.dragged() {
                    self.pan += response.drag_delta();
                }
                if response.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.unwrap_or(fit);

                if let Some(slot) = self.slots.get(&self.cur) {
                    // Skjermversjonen holder ikke ved denne zoomen: hent full oppløsning.
                    if !slot.full && scale > slot.tex.size()[0] as f32 / slot.orig[0] as f32 + 1e-3
                    {
                        self.request(self.cur, true);
                    }
                    let slot = &self.slots[&self.cur];
                    let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                    let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                    egui::Image::new(&slot.tex).paint_at(ui, rect);
                } else {
                    let text = if self.files.is_empty() {
                        if self.listing.is_some() {
                            "Leser mappa…"
                        } else {
                            "Ingen bilder i mappa"
                        }
                        .to_owned()
                    } else if let Some(e) = self.errors.get(&self.cur) {
                        format!("Kunne ikke åpne {}:\n{e}", self.files[self.cur].display())
                    } else {
                        String::new()
                    };
                    ui.painter().text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(16.0),
                        egui::Color32::GRAY,
                    );
                }
            });
    }
}

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("glimt")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "glimt",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, arg)))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_folder_and_decodes_to_screen_size() {
        let dir = std::env::temp_dir().join("glimt-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(400, 200)
            .save(dir.join("a.png"))
            .unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let (dirs, images) = super::list_folder(&dir);
        assert_eq!(images, vec![dir.join("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        let (img, orig) = super::decode(&dir.join("a.png"), [100, 100]).unwrap();
        assert_eq!((img.size, orig), ([100, 50], [400, 200]));
    }

    #[test]
    fn shuffle_keeps_all_elements() {
        let mut v: Vec<u32> = (0..100).collect();
        super::shuffle(&mut v);
        assert_ne!(v, (0..100).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn parses_config() {
        use super::Config;
        assert_eq!(Config::parse("# kommentar\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        assert_eq!(Config::parse("delay=0").delay, 1);
        assert_eq!(Config::parse("delay=7").delay, 7);
        let c = Config::parse("remember_dir=true\nlast_dir=/tmp/a b\n");
        assert!(c.remember_dir);
        assert_eq!(c.last_dir, Some("/tmp/a b".into()));
        assert_eq!(Config::parse("preload=x\nukjent=1"), Config::default());
    }
}
