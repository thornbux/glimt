//! glimt: rask og minnegjerrig bildefremviser.
//!
//! Bilder dekodes i bakgrunnstråder til skjermstørrelse; full oppløsning hentes bare
//! når brukeren zoomer forbi det skjermversjonen dekker. Bare et lite vindu rundt
//! gjeldende bilde holdes i minnet.

use eframe::egui;
use image::ImageDecoder;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
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

unsafe extern "C" {
    /// glibc: gi frigjort heap-minne tilbake til operativsystemet.
    fn malloc_trim(pad: usize) -> i32;
}

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
    /// Bildene i `dir`, bare filnavnet (hele stien lages ved behov med `path`): sparer minne
    /// i store mapper, der stien ellers ville gjentas for hver fil.
    files: Vec<OsString>,
    cur: usize,
    current: Arc<AtomicUsize>,
    /// Antall bilder forhåndslastet på hver side (Innstillinger).
    ahead: Arc<AtomicUsize>,
    show_settings: bool,
    /// Bekreftelsesdialog for permanent sletting er åpen.
    confirm: bool,
    /// Skala fra forrige frame (fysiske piksler per bildepiksel), til zoom-prosenten i statuslinja.
    last_scale: f32,
    /// Sist viste bilde; vises videre til det neste er dekodet, så blaing aldri gir svart skjerm.
    last_drawn: Option<(egui::TextureHandle, [usize; 2])>,
    /// Merkede filer (Ctrl/Shift+klikk i fillista) som Delete gjelder.
    selected: std::collections::BTreeSet<usize>,
    config: Config,
    /// Maks dekodestørrelse i fysiske piksler: [bildeområdet i vinduet, maks teksturside].
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
    /// Feilmeldinger fra bakgrunnsoppgaver (sletting), og den som vises nå.
    notices_tx: mpsc::Sender<String>,
    notices: mpsc::Receiver<String>,
    notice: Option<String>,
    /// Samlet størrelse på bildene i mappa, summert i bakgrunnen etter mappelesingen.
    /// Bare summer sendes (ingen tabell per fil), så minnebruken ikke vokser med mappa.
    total_size: u64,
    /// Delsummer fra bakgrunnen; `None` når alle er mottatt.
    size_rx: Option<mpsc::Receiver<u64>>,
    /// Byte frigjort av slettinger i bakgrunnen.
    freed_tx: mpsc::Sender<u64>,
    freed_rx: mpsc::Receiver<u64>,
    /// Størrelse på de merkede filene; beregnes i bakgrunnen når merkingen endres.
    selected_size: Option<u64>,
    selected_rx: Option<mpsc::Receiver<u64>>,
    /// Mappelesing i bakgrunnen: (undermapper, bilder).
    listing: Option<mpsc::Receiver<(Vec<PathBuf>, Vec<OsString>)>>,
    /// Rull trevisningen til gjeldende bilde ved neste tegning.
    scroll_to_cur: bool,
    /// Lysbildefremvisning: når forrige bilde ble vist; `None` = av.
    slideshow: Option<Instant>,
    /// Fillista er stokket for tilfeldig fremvisning.
    random: bool,
}

fn is_image(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Én gjennomgang av mappa: (undermapper, filnavn på bildene), begge sortert på navn.
/// Filtypen kommer fra katalogoppføringen, så det blir ingen `stat` per fil (bare symlenker slås opp).
fn list_folder(dir: &Path) -> (Vec<PathBuf>, Vec<OsString>) {
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if is_image(&path) {
            images.push(entry.file_name());
        } else if !entry.file_name().as_encoded_bytes().starts_with(b".")
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()))
        {
            dirs.push(path);
        }
    }
    sort_by_name(&mut dirs);
    images.sort_unstable(); // bytevis på Unix
    (dirs, images)
}

/// Stier i samme mappe: bytevis sammenligning av hele stien gir navnerekkefølge, og er
/// ~9× raskere enn Path::cmp/file_name(), som tolker stien på nytt i hver sammenligning.
fn sort_by_name(paths: &mut [PathBuf]) {
    paths.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
}

/// `$XDG_CONFIG_HOME/glimt/config`, ellers `~/.config/glimt/config`.
fn config_path() -> Option<PathBuf> {
    xdg_file("XDG_CONFIG_HOME", ".config", "config")
}

/// `$XDG_STATE_HOME/glimt/state`, ellers `~/.local/state/glimt/state`.
/// Tilstand (som siste mappe) er ikke innstillinger, og hører hjemme her etter XDG.
fn state_path() -> Option<PathBuf> {
    xdg_file("XDG_STATE_HOME", ".local/state", "state")
}

fn xdg_file(var: &str, home_fallback: &str, name: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_fallback)))?;
    Some(base.join("glimt").join(name))
}

fn parse_last_dir(state: &str) -> Option<PathBuf> {
    state.lines().find_map(|l| {
        l.strip_prefix("last_dir=")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// Innstillinger, lagret som `nøkkel=verdi`-linjer. Ukjente eller ugyldige linjer ignoreres.
#[derive(Debug, PartialEq)]
struct Config {
    preload: usize,
    /// Sekunder mellom bildene i lysbildefremvisning.
    delay: u64,
    /// Start i mappa fra forrige gang når ingen mappe er gitt på kommandolinja.
    remember_dir: bool,
    /// Sletting flytter til papirkurven (standard); ellers slettes fila permanent.
    trash: bool,
    /// Spør før permanent sletting.
    confirm_delete: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            preload: DEFAULT_AHEAD,
            delay: DEFAULT_DELAY,
            remember_dir: false,
            trash: true,
            confirm_delete: true,
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
                "trash" => c.trash = value == "true",
                "confirm_delete" => c.confirm_delete = value == "true",
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
        let text = format!(
            "preload={}\ndelay={}\nremember_dir={}\ntrash={}\nconfirm_delete={}\n",
            self.preload, self.delay, self.remember_dir, self.trash, self.confirm_delete
        );
        write_atomic(&p, &text);
    }
}

/// Skriv til en midlertidig fil og gi den nytt navn, så fila aldri blir halvskrevet ved krasj.
/// Prosess-ID i navnet, så flere samtidige økter ikke skriver til samme midlertidige fil.
fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("kunne ikke lagre {}: {e}", path.display());
    }
}

/// Ny indeks for `i` når de (sorterte) indeksene i `removed` fjernes fra lista; `None` hvis `i` selv fjernes.
fn shifted(i: usize, removed: &[usize]) -> Option<usize> {
    removed.binary_search(&i).err().map(|before| i - before)
}

/// Fjern oppføringene for `removed` (sortert) og flytt resten ned tilsvarende.
fn without_indices<T>(map: HashMap<usize, T>, removed: &[usize]) -> HashMap<usize, T> {
    map.into_iter()
        .filter_map(|(i, v)| Some((shifted(i, removed)?, v)))
        .collect()
}

/// Summen av filstørrelsene (én stat per fil); filer som ikke kan leses telles ikke.
fn total_size(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Størrelse som i filbehandleren: «850 kB», «2,3 MB», «1,2 GB» (desimal, norsk komma).
fn human_size(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1_000_000_000 => (b as f64 / 1e9, "GB"),
        b if b >= 1_000_000 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => return format!("{b} B"),
    };
    format!("{value:.1} {unit}").replace('.', ",")
}

/// Heltall med mellomrom som tusenskille: 300000 → «300 000».
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
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
                let state = std::fs::read_to_string(state_path()?).ok()?;
                parse_last_dir(&state).filter(|d| config.remember_dir && d.is_dir())
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
                    // glibc gir ikke frigjort minne fra trådenes malloc-arenaer tilbake av seg selv;
                    // dekoding etterlater flere bildestore buffere per tråd.
                    // SAFETY: malloc_trim har ingen forutsetninger utover en fungerende glibc-malloc.
                    unsafe { malloc_trim(0) };
                    ctx.request_repaint();
                }
            });
        }
        let (notices_tx, notices) = mpsc::channel();
        let (freed_tx, freed_rx) = mpsc::channel();
        let mut app = App {
            notices_tx,
            notices,
            notice: None,
            dir: PathBuf::new(),
            subdirs: Vec::new(),
            files: Vec::new(),
            cur: 0,
            current,
            ahead,
            show_settings: false,
            confirm: false,
            selected: Default::default(),
            last_drawn: None,
            last_scale: 1.0,
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
            total_size: 0,
            size_rx: None,
            freed_tx,
            freed_rx,
            selected_size: None,
            selected_rx: None,
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
            .map(OsString::from);
        self.subdirs.clear();
        self.random = false;
        self.dir = dir.clone();
        self.replace_files(ctx, shown.iter().cloned().collect(), shown);
        let (tx, rx) = mpsc::channel();
        let (size_tx, size_rx) = mpsc::channel();
        let (list_dir, ctx2) = (dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            // Mottakeren er borte hvis en annen mappe er åpnet i mellomtiden; da stopper vi.
            if tx.send(list_folder(&list_dir)).is_err() {
                return;
            }
            ctx2.request_repaint();
            // Størrelsene etterpå: les mappa én gang til og summer fra katalogoppføringene, i
            // bolker, så statuslinja fylles ut gradvis uten en kopi av hele fillista i minnet.
            let images = std::fs::read_dir(&list_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| is_image(&e.path()));
            let mut sizes = images
                .filter_map(|e| Some(e.metadata().ok()?.len()))
                .peekable();
            while sizes.peek().is_some() {
                if size_tx.send(sizes.by_ref().take(10_000).sum()).is_err() {
                    return;
                }
                ctx2.request_repaint();
            }
        });
        self.listing = Some(rx);
        self.size_rx = Some(size_rx);
        self.total_size = 0;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} — glimt",
            dir.display()
        )));
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

    /// Slett gjeldende bilde: ut av lista med en gang (naboenes teksturer beholdes, så neste
    /// bilde vises uten ny dekoding), og flytt fila til papirkurven i bakgrunnen.
    /// Delete-tasten / Fil → Slett: til papirkurven med en gang, permanent etter bekreftelse.
    fn request_delete(&mut self, ctx: &egui::Context) {
        if self.files.is_empty() {
            return;
        }
        if !self.config.trash && self.config.confirm_delete {
            self.confirm = true;
        } else {
            self.delete_targets_now(ctx);
        }
    }

    /// Sletting gjelder de merkede filene, ellers bildet som vises. Sortert.
    fn delete_targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            vec![self.cur]
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn delete_targets_now(&mut self, ctx: &egui::Context) {
        let targets = self.delete_targets();
        let paths: Vec<PathBuf> = targets.iter().map(|&i| self.path(i)).collect();
        self.remove_indices(ctx, &targets);
        let (tx, ctx, trash) = (self.notices_tx.clone(), ctx.clone(), self.config.trash);
        let freed_tx = self.freed_tx.clone();
        std::thread::spawn(move || {
            // Størrelsen leses før sletting, så statuslinja kan trekke den fra.
            let _ = freed_tx.send(total_size(&paths));
            ctx.request_repaint();
            let mut errors = Vec::new();
            if trash {
                // Mange filer per gio-kall, så tusenvis av filer ikke blir tusenvis av prosesser.
                for chunk in paths.chunks(500) {
                    match std::process::Command::new("gio")
                        .arg("trash")
                        .arg("--")
                        .args(chunk)
                        .output()
                    {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => errors.push(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
            } else {
                for path in &paths {
                    if let Err(e) = std::fs::remove_file(path) {
                        errors.push(format!("{}: {e}", path.display()));
                    }
                }
            }
            if let Some(first) = errors.first() {
                let _ = tx.send(format!("Sletting feilet ({} feil): {first}", errors.len()));
                ctx.request_repaint();
            }
        });
    }

    /// Fjern de (sorterte) indeksene fra lista i én gjennomgang og flytt indeksbasert tilstand
    /// tilsvarende. Naboenes teksturer beholdes, så neste bilde vises uten ny dekoding.
    fn remove_indices(&mut self, ctx: &egui::Context, removed: &[usize]) {
        let mut idx = 0;
        self.files.retain(|_| {
            idx += 1;
            removed.binary_search(&(idx - 1)).is_err()
        });
        self.slots = without_indices(std::mem::take(&mut self.slots), removed);
        self.errors = without_indices(std::mem::take(&mut self.errors), removed);
        self.selected.clear();
        // Jobber i arbeid har gamle indekser og avvises på filsti; refill ber om dem på nytt.
        self.pending.clear();
        // Neste gjenværende fil etter den som ble vist (eller den siste).
        let cur = self.cur - removed.partition_point(|&r| r < self.cur);
        if self.files.is_empty() {
            self.cur = 0;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "{} — glimt",
                self.dir.display()
            )));
        } else {
            self.go(ctx, cur.min(self.files.len() - 1));
        }
    }

    fn receive_sizes(&mut self) {
        while let Ok(freed) = self.freed_rx.try_recv() {
            // ponytail: sletting mens summen fortsatt beregnes kan gi et lite avvik.
            self.total_size = self.total_size.saturating_sub(freed);
        }
        if let Some(Ok(size)) = self.selected_rx.as_ref().map(mpsc::Receiver::try_recv) {
            self.selected_size = Some(size);
            self.selected_rx = None;
        }
        let Some(rx) = &self.size_rx else { return };
        loop {
            match rx.try_recv() {
                Ok(sum) => self.total_size += sum,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        self.size_rx = None;
    }

    /// Kalles når merkingen endres: oppdater tittelen og beregn størrelsen i bakgrunnen.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        self.title(ctx);
        self.selected_size = None;
        self.selected_rx = None;
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().map(|&i| self.path(i)).collect();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // Mottakeren er borte hvis merkingen er endret igjen; da forkastes svaret.
            if tx.send(total_size(&paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        self.selected_rx = Some(rx);
    }

    /// Bytt fillista og gå til `shown` (ellers første bilde). Teksturen som vises nå beholdes
    /// hvis den er laget fra samme fil. All indeksbasert tilstand nullstilles; jobber i arbeid
    /// avvises på filsti når de kommer.
    fn replace_files(
        &mut self,
        ctx: &egui::Context,
        files: Vec<OsString>,
        shown: Option<OsString>,
    ) {
        let slot = self.slots.remove(&self.cur);
        self.files = files;
        self.selected.clear();
        self.slots.clear();
        self.pending.clear();
        self.errors.clear();
        let cur = shown
            .and_then(|p| self.files.iter().position(|f| *f == p))
            .unwrap_or(0);
        if let Some(slot) = slot.filter(|s| self.is_file(cur, &s.path)) {
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

    /// Hele stien til bilde `i`.
    fn path(&self, i: usize) -> PathBuf {
        self.dir.join(&self.files[i])
    }

    /// Er `path` bilde nummer `i` i lista? (Uten å bygge stien.)
    fn is_file(&self, i: usize, path: &Path) -> bool {
        self.files.get(i).is_some_and(|name| {
            path.parent() == Some(self.dir.as_path()) && path.file_name() == Some(name.as_os_str())
        })
    }

    fn request(&mut self, idx: usize, full: bool) {
        let have = self.slots.get(&idx).is_some_and(|s| s.full || !full);
        if !have && !self.errors.contains_key(&idx) && self.pending.insert((idx, full)) {
            let _ = self.jobs.send((idx, full, self.path(idx)));
        }
    }

    fn go(&mut self, ctx: &egui::Context, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        self.cur = idx.min(self.files.len() - 1);
        self.current.store(self.cur, Ordering::Relaxed);
        self.notice = None;
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
            files.sort_unstable();
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
        let name = file.to_string_lossy();
        let show = match (self.slideshow.is_some(), self.random) {
            (true, true) => format!("🔀 {} s · ", self.config.delay),
            (true, false) => format!("▶ {} s · ", self.config.delay),
            _ => String::new(),
        };
        let (n, total) = (self.cur + 1, self.files.len());
        let marked = match self.selected.len() {
            0 => String::new(),
            m => format!(" · {m} merket"),
        };
        let title = format!("{show}{name} ({n}/{total}){marked} — glimt");
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
            let wanted = self.is_file(d.idx, &d.path)
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
        // ↑/↓ blar i fillista som i Geeqie; under lysbildefremvisning justerer de tiden i stedet.
        let (down, up) = match self.slideshow {
            None => (pressed(Key::ArrowDown), pressed(Key::ArrowUp)),
            Some(_) => (false, false),
        };
        if pressed(Key::ArrowRight) || pressed(Key::Space) || pressed(Key::PageDown) || down {
            self.go(ctx, (self.cur + 1).min(last));
        } else if pressed(Key::ArrowLeft) || pressed(Key::Backspace) || pressed(Key::PageUp) || up {
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
        if pressed(Key::Delete) {
            self.request_delete(ctx);
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
        // ponytail: stien lagres som UTF-8 (tapsfritt for vanlige stier); bytt til OsString-bytes ved behov.
        if self.config.remember_dir
            && let Some(p) = state_path()
        {
            write_atomic(&p, &format!("last_dir={}\n", self.dir.display()));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.limits.lock().unwrap()[1] = [max_side, max_side];
        self.receive(ctx);
        self.receive_listing(ctx, std::time::Duration::ZERO);
        self.receive_sizes();
        while let Ok(msg) = self.notices.try_recv() {
            // Sletting feilet, så fila finnes trolig fortsatt: les mappa på nytt så lista stemmer.
            let keep = match self.files.get(self.cur) {
                Some(name) => self.dir.join(name),
                None => self.dir.clone(),
            };
            self.open(ctx, keep);
            self.notice = Some(msg);
        }
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
                        if ui.button("Slett (Delete)").clicked() {
                            self.request_delete(ctx);
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
                    "Opptil ≈ {mb} MB med bilder i vindusstørrelse {w}×{h}"
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
                ui.separator();
                ui.label("Sletting (Delete):");
                let trash = egui::Checkbox::new(&mut self.config.trash, "Flytt til papirkurven");
                let confirm = egui::Checkbox::new(
                    &mut self.config.confirm_delete,
                    "Bekreft permanent sletting",
                );
                if ui.add(trash).changed() | ui.add_enabled(!self.config.trash, confirm).changed() {
                    self.config.save();
                }
            });

        if self.confirm {
            let what = match self.delete_targets().as_slice() {
                [i] => format!("«{}»", self.files[*i].to_string_lossy()),
                many => format!("{} filer", many.len()),
            };
            let mut delete = false;
            let modal = egui::Modal::new(egui::Id::new("bekreft-sletting")).show(ctx, |ui| {
                ui.label(format!("Slette {what} permanent?"));
                ui.weak("Dette kan ikke angres.");
                ui.horizontal(|ui| {
                    delete = ui.button("Slett (Enter)").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Avbryt (Esc)").clicked() {
                        ui.close();
                    }
                });
            });
            if delete {
                self.confirm = false;
                self.delete_targets_now(ctx);
            } else if modal.should_close() {
                self.confirm = false;
            }
        }
        self.show_settings = open;

        // Statuslinje som i Geeqie: antall bilder og merkede til venstre, gjeldende bilde til høyre.
        if !fullscreen {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let n = self.files.len();
                    let size = if self.size_rx.is_some() {
                        "beregner…".to_owned()
                    } else {
                        human_size(self.total_size)
                    };
                    let mut left = if self.listing.is_some() {
                        "Leser mappa…".to_owned()
                    } else {
                        format!("{} bilder ({size})", thousands(n))
                    };
                    if !self.selected.is_empty() {
                        let m = thousands(self.selected.len());
                        let size = self
                            .selected_size
                            .map_or("beregner…".to_owned(), human_size);
                        left += &format!(", {m} merket ({size})");
                    }
                    ui.label(left);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(slot) = self.slots.get(&self.cur) {
                            let [w, h] = slot.orig;
                            let zoom = (self.last_scale * 100.0).round();
                            ui.label(format!("{w}×{h}  {zoom} %"));
                        }
                        if n > 0 {
                            ui.label(format!("{}/{}", thousands(self.cur + 1), thousands(n)));
                        }
                    });
                });
            });
        }

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
                            let name = self.files[i].to_string_lossy();
                            let marked = i == self.cur || self.selected.contains(&i);
                            if ui.selectable_label(marked, name).clicked() {
                                let m = ui.input(|i| i.modifiers);
                                if m.command {
                                    // Ctrl+klikk: merk/avmerk (bildet som vises tas med første gang).
                                    if self.selected.is_empty() {
                                        self.selected.insert(self.cur);
                                    }
                                    if !self.selected.remove(&i) {
                                        self.selected.insert(i);
                                    }
                                    self.selection_changed(ctx);
                                } else if m.shift {
                                    // Shift+klikk: merk området fra bildet som vises.
                                    self.selected.extend(i.min(self.cur)..=i.max(self.cur));
                                    self.selection_changed(ctx);
                                } else {
                                    self.selected.clear();
                                    self.selected_size = None;
                                    self.selected_rx = None;
                                    go_to = Some(i);
                                }
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
                // Dekod til bildeområdet, ikke hele skjermen: et lite vindu på en 4K-skjerm
                // trenger en brøkdel av minnet (3840×2160 er 33 MB per bilde).
                let view = [(area.width() * ppp) as u32, (area.height() * ppp) as u32];
                self.limits.lock().unwrap()[0] = view;
                let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
                let fit = self.slots.get(&self.cur).map_or(1.0, |s| {
                    (area.width() * ppp / s.orig[0] as f32)
                        .min(area.height() * ppp / s.orig[1] as f32)
                        .min(1.0)
                });
                // Taster går til innstillingsvinduet mens det er åpent.
                if !self.show_settings && !self.confirm {
                    self.keys(ctx, fit);
                }
                if response.dragged() {
                    self.pan += response.drag_delta();
                }
                if response.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
                let scale = self.zoom.unwrap_or(fit);
                self.last_scale = scale;

                // For liten tekstur: i «tilpass» er vinduet gjort større, så dekod på nytt i
                // vindusstørrelse (forrige tekstur vises imens); ved zoom hentes full oppløsning.
                let too_small = self.slots.get(&self.cur).is_some_and(|s| {
                    let needed = (s.orig[0] as f32 * scale).min(s.orig[0] as f32);
                    !s.full && (s.tex.size()[0] as f32) + 1.0 < needed
                });
                if too_small && self.zoom.is_none() {
                    self.slots.remove(&self.cur);
                    self.request(self.cur, false);
                } else if too_small {
                    self.request(self.cur, true);
                }
                if let Some(slot) = self.slots.get(&self.cur) {
                    let size = egui::vec2(slot.orig[0] as f32, slot.orig[1] as f32) * scale / ppp;
                    let rect = egui::Rect::from_center_size(area.center() + self.pan, size);
                    egui::Image::new(&slot.tex).paint_at(ui, rect);
                    self.last_drawn = Some((slot.tex.clone(), slot.orig));
                } else if let Some((tex, orig)) = &self.last_drawn
                    && !self.files.is_empty()
                    && !self.errors.contains_key(&self.cur)
                {
                    let size = egui::vec2(orig[0] as f32, orig[1] as f32);
                    let fit = (area.width() * ppp / size.x)
                        .min(area.height() * ppp / size.y)
                        .min(1.0);
                    let rect = egui::Rect::from_center_size(area.center(), size * fit / ppp);
                    egui::Image::new(tex).paint_at(ui, rect);
                } else {
                    let text = if self.files.is_empty() {
                        if self.listing.is_some() {
                            "Leser mappa…"
                        } else {
                            "Ingen bilder i mappa"
                        }
                        .to_owned()
                    } else if let Some(e) = self.errors.get(&self.cur) {
                        format!("Kunne ikke åpne {}:\n{e}", self.path(self.cur).display())
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
                if let Some(notice) = &self.notice {
                    ui.painter().text(
                        area.center_bottom() - egui::vec2(0.0, 16.0),
                        egui::Align2::CENTER_BOTTOM,
                        notice,
                        egui::FontId::proportional(16.0),
                        egui::Color32::LIGHT_RED,
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
        assert_eq!(images, vec![std::ffi::OsString::from("a.png")]);
        assert_eq!(dirs, vec![dir.join("sub")]);
        let (img, orig) = super::decode(&dir.join("a.png"), [100, 100]).unwrap();
        assert_eq!((img.size, orig), ([100, 50], [400, 200]));
    }

    #[test]
    fn without_indices_shifts_down() {
        let map = std::collections::HashMap::from([(1, 'a'), (2, 'b'), (4, 'c'), (6, 'd')]);
        let expected = std::collections::HashMap::from([(1, 'a'), (3, 'd')]);
        assert_eq!(super::without_indices(map, &[2, 4, 5]), expected);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(850_000), "850,0 kB");
        assert_eq!(super::human_size(2_345_678), "2,3 MB");
        assert_eq!(super::human_size(1_200_000_000), "1,2 GB");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(1000), "1 000");
        assert_eq!(super::thousands(300000), "300 000");
        assert_eq!(super::thousands(1234567), "1 234 567");
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
    fn writes_atomically() {
        let dir = std::env::temp_dir().join("glimt-test-atomic");
        let path = dir.join("config");
        super::write_atomic(&path, "a=1\n");
        super::write_atomic(&path, "a=2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "ingen .tmp-filer igjen"
        );
    }

    #[test]
    fn parses_config() {
        use super::Config;
        assert_eq!(Config::parse("# kommentar\npreload= 5\n").preload, 5);
        assert_eq!(Config::parse("preload=99").preload, 20);
        assert_eq!(Config::parse("delay=0").delay, 1);
        assert_eq!(Config::parse("delay=7").delay, 7);
        assert!(Config::default().trash && !Config::parse("trash=false").trash);
        assert!(Config::parse("remember_dir=true").remember_dir);
        assert_eq!(
            super::parse_last_dir("last_dir=/tmp/a b\n"),
            Some("/tmp/a b".into())
        );
        assert_eq!(super::parse_last_dir("last_dir=\n"), None);
        assert_eq!(Config::parse("preload=x\nukjent=1"), Config::default());
    }
}
