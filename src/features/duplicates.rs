//! Find Duplicates, as in Geeqie: groups of images that are the same file, have the same name,
//! size or dimensions, or look alike. Runs over the images of the list in background threads.

use crate::support::format::{count, human_size, thousands};
use crate::support::lang::tr;
use crate::support::threads::{Receiving, Took, in_parts};
use crate::{features::duplicates, tf};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::hash::Hasher;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

pub const ALL: [(By, &str); 7] = [
    (By::Checksum, "Checksum (identical files)"),
    (By::Name, "Name"),
    (By::Size, "Size"),
    (By::Dimensions, "Dimensions"),
    (By::Similar(95), "Similarity (high, 95%)"),
    (By::Similar(90), "Similarity (90%)"),
    (By::Similar(85), "Similarity (low, 85%)"),
];

/// Side of the small picture two pictures are compared by.
const SIGNATURE_SIDE: u32 = 16;

#[derive(Clone, Copy, PartialEq)]
pub enum By {
    /// The same content, by checksum (only files of the same size are read).
    Checksum,
    Name,
    Size,
    Dimensions,
    /// Looking alike, at least this many percent.
    Similar(u8),
}

/// How far a search is, and a way to stop it.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub is_cancelled: AtomicBool,
}

/// A picture to compare.
struct Picture {
    mean: f32,
    /// Its place in the list.
    at: usize,
    /// The means of its four bands and of its 16 rows.
    bands: [f32; 4],
    rows: [f32; 16],
    signature: Vec<u8>,
}

/// The Find Duplicates window: what to compare by, the search running and its results.
pub(crate) struct FindDuplicates {
    by: duplicates::By,
    progress: Arc<duplicates::Progress>,
    groups_rx: Receiving<Vec<Vec<PathBuf>>>,
    groups: Vec<Vec<PathBuf>>,
    /// (group, file) of each row: the file `usize::MAX` for the group's heading.
    rows: Vec<(usize, usize)>,
    checked: HashSet<PathBuf>,
    /// Images searched, for the line above the results.
    searched: usize,
    /// File sizes, read once when a row is first shown, not on every frame.
    sizes: HashMap<PathBuf, u64>,
}

impl FindDuplicates {
    /// The groups found: their count, buttons to check and uncheck, and the list, where a
    /// click on a name sets `show`. Returns whether Move to Trash was clicked.
    fn results(&mut self, ui: &mut egui::Ui, show: &mut Option<PathBuf>) -> bool {
        let files: usize = self.groups.iter().map(Vec::len).sum();
        ui.label(tf!(
            "{groups}, {files}, among {images}",
            groups = count(self.groups.len(), "group"),
            files = count(files, "file"),
            images = count(self.searched, "image")
        ));
        let mut trash = false;
        ui.horizontal(|ui| {
            if crate::ui::menus::button(ui, tr("Check all but the first of each group")).clicked() {
                self.checked = self
                    .groups
                    .iter()
                    .flat_map(|g| g[1..].iter().cloned())
                    .collect();
            }
            if crate::ui::menus::button(ui, tr("Uncheck all")).clicked() {
                self.checked.clear();
            }
            let n = self.checked.len();
            trash =
                crate::ui::menus::button_enabled(ui, n > 0, &tf!("Move {n} checked to trash…", n))
                    .clicked();
        });
        ui.separator();
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            row_h,
            self.rows.len(),
            |ui, range| {
                for &(g, f) in &self.rows[range] {
                    let group = &self.groups[g];
                    if f == usize::MAX {
                        ui.strong(tf!(
                            "Group {n} ({files})",
                            n = g + 1,
                            files = count(group.len(), "file")
                        ));
                        continue;
                    }
                    let path = &group[f];
                    ui.horizontal(|ui| {
                        let mut on = self.checked.contains(path);
                        if crate::ui::menus::checkbox(ui, &mut on, "").changed() {
                            match on {
                                true => self.checked.insert(path.clone()),
                                false => self.checked.remove(path),
                            };
                        }
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        if crate::ui::menus::link(ui, &name).clicked() {
                            *show = Some(path.clone());
                        }
                        let size = *(self.sizes.entry(path.clone()))
                            .or_insert_with(|| path.metadata().map_or(0, |m| m.len()));
                        ui.weak(format!(
                            "{}  {}",
                            human_size(size),
                            path.parent().unwrap_or(path).display()
                        ));
                    });
                }
            },
        );
        trash
    }

    pub(crate) fn new() -> Self {
        FindDuplicates {
            by: duplicates::By::Checksum,
            progress: Arc::default(),
            groups_rx: Receiving::none(),
            groups: Vec::new(),
            sizes: HashMap::new(),
            rows: Vec::new(),
            checked: HashSet::new(),
            searched: 0,
        }
    }

    /// Stops the search and leaves it at once. A file being read on a stalled share holds its
    /// thread, which then finishes on its own, and nobody waits for its answer.
    fn stop(&mut self) {
        self.progress.is_cancelled.store(true, Ordering::Relaxed);
        self.groups_rx = Receiving::none();
        self.searched = 0;
    }

    /// Take out files that are gone; groups of one are no groups.
    pub(crate) fn remove(&mut self, gone: &HashSet<&Path>) {
        for g in &mut self.groups {
            g.retain(|p| !gone.contains(p.as_path()));
        }
        self.groups.retain(|g| g.len() > 1);
        self.checked.retain(|p| !gone.contains(p.as_path()));
        self.rows = (self.groups.iter().enumerate())
            .flat_map(|(g, files)| {
                std::iter::once((g, usize::MAX)).chain((0..files.len()).map(move |f| (g, f)))
            })
            .collect();
    }
}

impl crate::App {
    /// The Find Duplicates window: compares the images of the list in the background, and
    /// lists the groups found; checked files can go to the trash.
    pub(crate) fn duplicates_window(&mut self, ctx: &egui::Context) {
        let Some(mut find_duplicates) = self.find_duplicates.take() else {
            return;
        };
        match find_duplicates.groups_rx.take() {
            Took::Nothing => {}
            Took::Gone => {
                find_duplicates.searched = 0;
                self.tell_background_stopped(tr("Find Duplicates"));
            }
            Took::Answer(groups) => {
                // Stopped: no result, not "0 groups" as if every image had been compared.
                if find_duplicates
                    .progress
                    .is_cancelled
                    .load(Ordering::Relaxed)
                {
                    find_duplicates.searched = 0;
                }
                find_duplicates.groups = groups;
                find_duplicates.sizes.clear();
                find_duplicates.checked.clear();
                find_duplicates.remove(&HashSet::new());
            }
        }
        let (mut open, mut start, mut trash, mut show) = (true, false, false, None);
        egui::Window::new(tr("Find Duplicates"))
            .open(&mut open)
            .default_size([620.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tf!(
                        "Among the {count} in the list, compare by",
                        count = count(self.list.files().len(), "image")
                    ));
                    let label = ALL
                        .iter()
                        .find(|a| a.0 == find_duplicates.by)
                        .map_or("", |a| a.1);
                    egui::ComboBox::from_id_salt("compare by")
                        .selected_text(tr(label))
                        .show_ui(ui, |ui| {
                            for (by, label) in ALL {
                                crate::ui::menus::choice(
                                    ui,
                                    &mut find_duplicates.by,
                                    by,
                                    tr(label),
                                );
                            }
                        });
                    if find_duplicates.groups_rx.is_some() {
                        if crate::ui::menus::button(ui, tr("Stop")).clicked() {
                            find_duplicates.stop();
                        }
                    } else {
                        let on = self.list.files().len() > 1;
                        start = crate::ui::menus::button_enabled(ui, on, tr("Find")).clicked();
                    }
                });
                if find_duplicates.groups_rx.is_some() {
                    let (done, total) = (
                        find_duplicates.progress.done.load(Ordering::Relaxed),
                        find_duplicates.progress.total.load(Ordering::Relaxed),
                    );
                    ui.label(tf!(
                        "Comparing… {done} of {total}",
                        done = thousands(done),
                        total = thousands(total)
                    ));
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                if find_duplicates.searched == 0 {
                    if !self.folder.recursive() {
                        ui.weak(tr(
                            "Include Subfolders in the View menu searches the folders below too.",
                        ));
                    }
                    return;
                }
                trash = find_duplicates.results(ui, &mut show);
            });
        if start {
            self.start_find(ctx, &mut find_duplicates);
        }
        if let Some(path) = show {
            match (0..self.list.files().len()).find(|&i| self.is_file(i, &path)) {
                Some(i) => self.show_file(ctx, i),
                None => self.visit(ctx, path),
            }
        }
        if trash {
            self.trash_checked(ctx, &find_duplicates);
        }
        if open {
            self.find_duplicates = Some(find_duplicates);
        } else {
            find_duplicates
                .progress
                .is_cancelled
                .store(true, Ordering::Relaxed);
        }
    }

    /// Find: the images of the list compared by `find_duplicates.by` in the background.
    fn start_find(&mut self, ctx: &egui::Context, find_duplicates: &mut FindDuplicates) {
        let paths: Vec<PathBuf> = (0..self.list.files().len()).map(|i| self.path(i)).collect();
        let (tx, ctx) = (mpsc::channel(), ctx.clone());
        let (by, progress) = (find_duplicates.by, Arc::new(Progress::default()));
        let (rotate, save) = (self.config.exif_rotate, self.config.save_thumbnails);
        find_duplicates.searched = paths.len();
        find_duplicates.progress = progress.clone();
        find_duplicates.groups_rx = Receiving::of(tx.1);
        let sender = tx.0;
        std::thread::spawn(move || {
            let _ = sender.send(find(paths, by, rotate, save, &progress));
            ctx.request_repaint();
        });
    }

    /// Move to Trash: the checked files of the list, through the usual delete with its
    /// confirmation.
    fn trash_checked(&mut self, ctx: &egui::Context, find_duplicates: &FindDuplicates) {
        let checked = (0..self.list.files().len())
            .filter(|&i| find_duplicates.checked.contains(&self.path(i)))
            .collect();
        if !self.select_for_action(ctx, checked) {
            return;
        }
        // With none selected the delete takes the shown image: only when the list still has
        // every checked file, else it would be another one, or too few.
        if self.list.selected().len() == find_duplicates.checked.len() {
            self.request_delete(ctx, true);
        } else {
            self.notice = Some(tr("The checked files are no longer all in the list").into());
        }
    }
}

/// Groups of two or more of `paths` that match `by`, largest groups first. `rotate` and `save`
/// are passed on to the thumbnails used for similarity.
pub fn find(
    paths: Vec<PathBuf>,
    by: By,
    rotate: bool,
    save: bool,
    progress: &Progress,
) -> Vec<Vec<PathBuf>> {
    // The same file reached twice, by a link to it, through a linked folder or through a bind
    // mount, would be its own "duplicate", and checked and trashed the only copy would go: each
    // file once. Hard links of one file are one file too, since trashing one frees nothing.
    let ids = on_all_cores(&paths, progress, crate::images::save::file_id);
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<PathBuf> = (paths.into_iter().zip(ids))
        .filter_map(|(path, id)| {
            seen.insert(id.unwrap_or_else(|| Err(path.clone())))
                .then_some(path)
        })
        .collect();
    let mut groups = match by {
        By::Name => {
            let names = paths
                .iter()
                .map(|p| p.file_name().map(|n| n.to_owned()))
                .collect();
            groups(paths, names)
        }
        By::Size => {
            let sizes = on_all_cores(&paths, progress, size);
            groups(paths, sizes)
        }
        By::Checksum => {
            // Only files of the same size can be the same; most are never read.
            let sizes = on_all_cores(&paths, progress, size);
            let candidates: Vec<PathBuf> = groups(paths, sizes).into_iter().flatten().collect();
            let sums = on_all_cores(&candidates, progress, |path| checksum(path, progress));
            // The hash only finds them: the files of a group are compared byte by byte, as two
            // that differ could share a 64-bit hash, and these are offered for the trash.
            // A second reading of each file: counted in the progress, and stopped by Cancel.
            let found = groups(candidates, sums);
            progress
                .total
                .store(found.iter().map(Vec::len).sum(), Ordering::Relaxed);
            progress.done.store(0, Ordering::Relaxed);
            (found.into_iter())
                .take_while(|_| !progress.is_cancelled.load(Ordering::Relaxed))
                .flat_map(|group| same_contents(group, progress))
                .collect()
        }
        By::Dimensions => {
            let dims = on_all_cores(&paths, progress, crate::images::decode::dimensions);
            groups(paths, dims)
        }
        By::Similar(percent) => {
            let signatures = on_all_cores(&paths, progress, |path| {
                let thumb = crate::images::thumbs::thumbnail(path, rotate, save, false).ok()?;
                Some(signature(&thumb))
            });
            similar(paths, signatures, percent, progress)
        }
    };
    if progress.is_cancelled.load(Ordering::Relaxed) {
        return Vec::new();
    }
    for g in &mut groups {
        g.sort();
    }
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    groups
}

fn size(path: &Path) -> Option<u64> {
    path.metadata().ok().map(|m| m.len())
}

/// The paths with the same key, in groups of two or more (`None`: left out).
fn groups<K: std::hash::Hash + Eq>(paths: Vec<PathBuf>, keys: Vec<Option<K>>) -> Vec<Vec<PathBuf>> {
    let mut by_key: HashMap<K, Vec<PathBuf>> = HashMap::new();
    for (path, key) in paths.into_iter().zip(keys) {
        if let Some(k) = key {
            by_key.entry(k).or_default().push(path);
        }
    }
    by_key.into_values().filter(|g| g.len() > 1).collect()
}

/// Runs `f` on every path, on all cores, counting in `progress`. Returns `None` for all once
/// cancelled.
fn on_all_cores<T: Send>(
    paths: &[PathBuf],
    progress: &Progress,
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    progress.total.store(paths.len(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let next = AtomicUsize::new(0);
    let threads = crate::support::threads::background_threads();
    let mut results: Vec<(usize, Option<T>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= paths.len() || progress.is_cancelled.load(Ordering::Relaxed) {
                            return out;
                        }
                        // A file that breaks its reader is left out alone, not the worker's
                        // whole share of the list with it.
                        let found = crate::support::threads::catch_panic(|| f(&paths[i]));
                        out.push((i, found.flatten()));
                        progress.done.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    results.sort_unstable_by_key(|r| r.0);
    let mut all: Vec<Option<T>> = (0..paths.len()).map(|_| None).collect();
    for (i, v) in results {
        all[i] = v;
    }
    all
}

/// `group` split into the files whose bytes are the same, groups of one left out.
/// Counted in `progress` file by file, and stopped by its Cancel: one group may hold thousands.
fn same_contents(group: Vec<PathBuf>, progress: &Progress) -> Vec<Vec<PathBuf>> {
    let mut alike: Vec<Vec<PathBuf>> = Vec::new();
    for path in group {
        if progress.is_cancelled.load(Ordering::Relaxed) {
            break;
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
        match alike
            .iter_mut()
            .find(|a| same_bytes(&a[0], &path, progress))
        {
            Some(a) => a.push(path),
            None => alike.push(vec![path]),
        }
    }
    alike.retain(|a| a.len() > 1);
    alike
}

/// Whether files `a` and `b` hold the same bytes; not when one cannot be read. The answer is
/// false once `p` is cancelled, which is looked at between the reads of a large file.
fn same_bytes(a: &Path, b: &Path, progress: &Progress) -> bool {
    let (Ok(mut a), Ok(mut b)) = (
        crate::images::decode::open_regular(a),
        crate::images::decode::open_regular(b),
    ) else {
        return false;
    };
    let (mut x, mut y) = (vec![0; 1 << 16], vec![0; 1 << 16]);
    loop {
        if progress.is_cancelled.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(n) = a.read(&mut x) else { return false };
        // A read may return less than asked: as much of `b` as `a` gave.
        if b.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return false;
        }
        if n == 0 {
            // At the end of both: `b` has no byte more.
            return b.read(&mut y[..1]).is_ok_and(|m| m == 0);
        }
    }
}

/// A 64-bit hash of the whole file. Returns `None` once `p` is cancelled, which is looked at
/// between the reads of a large file.
fn checksum(path: &Path, progress: &Progress) -> Option<u64> {
    let mut file = crate::images::decode::open_regular(path).ok()?;
    let mut hasher = std::hash::DefaultHasher::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        if progress.is_cancelled.load(Ordering::Relaxed) {
            return None;
        }
        match file.read(&mut buf).ok()? {
            0 => return Some(hasher.finish()),
            n => hasher.write(&buf[..n]),
        }
    }
}

/// The picture shrunk to 16×16, its red, green and blue values in a row.
fn signature(img: &image::RgbaImage) -> Vec<u8> {
    let small = image::imageops::resize(
        img,
        SIGNATURE_SIDE,
        SIGNATURE_SIDE,
        image::imageops::FilterType::Triangle,
    );
    small.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

fn mean(s: &[u8]) -> f32 {
    s.iter().map(|&v| v as f32).sum::<f32>() / s.len() as f32
}

/// The means of `N` equal parts of a signature, top to bottom: four bands, or its 16 rows.
fn part_means<const N: usize>(s: &[u8]) -> [f32; N] {
    let mut each = s.chunks(s.len().div_ceil(N)).map(mean);
    [(); N].map(|_| each.next().unwrap_or(0.0))
}

fn bands(s: &[u8]) -> [f32; 4] {
    part_means(s)
}

/// The sum of the differences of two lists of part means.
fn difference(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// How alike two signatures are, 0–100: 100 less the mean difference of their values.
fn likeness(a: &[u8], b: &[u8]) -> f32 {
    let diff: u32 = a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as u32).sum();
    100.0 - diff as f32 * 100.0 / (255.0 * a.len() as f32)
}

/// Whether two signatures are at least `percent` alike, as `likeness` says; given up on, a row
/// at a time, once they differ too much.
fn are_alike(a: &[u8], b: &[u8], percent: u8) -> bool {
    let most = ((100 - percent) as u32 * 255 * a.len() as u32) / 100;
    let mut diff = 0;
    for (x, y) in a.chunks(48).zip(b.chunks(48)) {
        diff += x
            .iter()
            .zip(y)
            .map(|(&p, &q)| p.abs_diff(q) as u32)
            .sum::<u32>();
        if diff > most {
            return false;
        }
    }
    likeness(a, b) >= percent as f32
}

/// Groups of pictures at least `percent` alike, each around its first picture as in Geeqie,
/// so a chain of small changes does not make one long group. Pictures are taken in order of
/// mean value, and each is compared only with those whose mean is close enough to match at
/// all: pictures `percent` alike differ by at most that much in mean too, so none are missed.
/// Within that, the means of the four bands and then of the 16 rows rule out most pairs before
/// all 768 values are compared: their differences add up to at most 4 and 16 times as much.
/// The pairs left are still about n × n / 50 (100,000 photos: 86 s on one thread), so a block of
/// pictures finds its matches on all cores, and the groups are then made in order from them,
/// as one thread would make them.
fn similar(
    paths: Vec<PathBuf>,
    signatures: Vec<Option<Vec<u8>>>,
    percent: u8,
    progress: &Progress,
) -> Vec<Vec<PathBuf>> {
    let mut pictures: Vec<Picture> = (signatures.into_iter().enumerate())
        .filter_map(|(i, s)| {
            let s = s?;
            Some(Picture {
                mean: mean(&s),
                at: i,
                bands: bands(&s),
                rows: part_means(&s),
                signature: s,
            })
        })
        .collect();
    pictures.sort_by(|a, b| a.mean.total_cmp(&b.mean));
    let reach = (100.0 - percent as f32) * 255.0 / 100.0;
    // Those alike enough with `k` after it, of those not in a group when its block began.
    let matches = |k: usize, grouped: &[bool]| -> Vec<usize> {
        let this = &pictures[k];
        let mut out = Vec::new();
        for (n, other) in pictures.iter().enumerate().skip(k + 1) {
            if other.mean - this.mean > reach {
                break;
            }
            // A little room for rounding: these only rule out pairs that cannot match.
            if !grouped[n]
                && difference(&this.bands, &other.bands) <= 4.0 * reach + 0.01
                && difference(&this.rows, &other.rows) <= 16.0 * reach + 0.01
                && are_alike(&this.signature, &other.signature, percent)
            {
                out.push(n);
            }
        }
        out
    };
    progress.total.store(pictures.len(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    let mut grouped = vec![false; pictures.len()];
    let mut found = Vec::new();
    let threads = crate::support::threads::background_threads();
    // Small while many match (thousands of copies of one picture), so the matches kept for a
    // block stay few; up to 512 pictures once they match few.
    let (mut k, mut block) = (0, 1);
    while k < pictures.len() {
        if progress.is_cancelled.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let ks: Vec<usize> = (k..(k + block).min(pictures.len()))
            .filter(|&k| !grouped[k])
            .collect();
        let matches_kept = group_block(&ks, threads, &mut grouped, &mut found, matches);
        k += block;
        progress
            .done
            .store(k.min(pictures.len()), Ordering::Relaxed);
        block = match matches_kept > 1_000_000 {
            true => (block / 2).max(1),
            false => (block * 2).min(512),
        };
    }
    (found.into_iter())
        .map(|group| {
            group
                .into_iter()
                .map(|n| paths[pictures[n].at].clone())
                .collect()
        })
        .collect()
}

/// Groups the pictures `ks` of one block (in order, from low to high) as one thread would,
/// from their matches found on `threads` at once, and adds the groups to `found`. Returns how
/// many matches were kept at once. A part's matches count even when its first picture went to
/// a group before it, as they were kept all the same, and the next block is chosen by them.
/// A picture that `block_matches` left out, as one before it in its part found it, needs its own
/// matches when another group took that picture. It is then found again on all cores, with
/// the pictures left out after it, rather than on this thread while the others wait.
fn group_block(
    ks: &[usize],
    threads: usize,
    grouped: &mut [bool],
    found: &mut Vec<Vec<usize>>,
    matches: impl Fn(usize, &[bool]) -> Vec<usize> + Sync,
) -> usize {
    let now: &[bool] = grouped;
    let mut each = block_matches(ks, threads, |k| matches(k, now));
    let mut matches_kept: usize = each.iter().flatten().map(Vec::len).sum();
    for i in 0..ks.len() {
        let k = ks[i];
        // Taken by a picture before it in the block: not a group of its own.
        if grouped[k] {
            continue;
        }
        if each[i].is_none() {
            let left: Vec<usize> = (i..ks.len())
                .filter(|&j| each[j].is_none() && !grouped[ks[j]])
                .collect();
            let again: Vec<usize> = left.iter().map(|&j| ks[j]).collect();
            let now: &[bool] = grouped;
            let found_again = block_matches(&again, threads, |k| matches(k, now));
            matches_kept += found_again.iter().flatten().map(Vec::len).sum::<usize>();
            for (j, alike) in left.into_iter().zip(found_again) {
                each[j] = alike;
            }
        }
        // The first of those found again is never left out, as nothing before it is in its part.
        let alike = each[i].take().unwrap_or_default();
        let mut group = vec![k];
        for n in alike {
            if !grouped[n] {
                grouped[n] = true;
                group.push(n);
            }
        }
        if group.len() > 1 {
            found.push(group);
        }
    }
    matches_kept
}

/// `matches` of each of `ks` (in order, from low to high), on `threads` at once; `None` for one
/// that a picture before it in the same part found, which then takes it. Without that, a block
/// of 512 that met 50,000 copies of one picture kept every copy 512 times over.
fn block_matches(
    ks: &[usize],
    threads: usize,
    matches: impl Fn(usize) -> Vec<usize> + Sync,
) -> Vec<Option<Vec<usize>>> {
    in_parts(ks, threads, |part, _| {
        let mut found = std::collections::HashSet::new();
        (part.iter())
            .map(|&k| {
                if found.contains(&k) {
                    return None;
                }
                let alike = matches(k);
                found.extend(alike.iter().copied());
                Some(alike)
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    /// Move to Trash with a checked file that the search hides trashes nothing, says why, and
    /// leaves the selection as it was. It said the checked files were no longer in the list,
    /// and the user's selection was gone.
    #[test]
    fn checked_files_the_search_hides_are_not_trashed() {
        let dir = std::env::temp_dir().join(format!("glimt-test-dup-hid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        app.list
            .set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.list.select([2].into());
        app.list.set_matches(Some(vec![1, 2]));
        let mut find_duplicates = super::FindDuplicates::new();
        find_duplicates.checked = [dir.join("a.png"), dir.join("b.png")].into();
        app.trash_checked(&ctx, &find_duplicates);
        let hides = "The search hides files this would act on: clear it first";
        assert_eq!(app.notice.as_deref(), Some(crate::support::lang::tr(hides)));
        assert_eq!(app.list.selected(), &[2].into());
        assert!(!app.asking_about_selection());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One file that breaks its reader is left out alone; every other file keeps its result,
    /// and the count of files done still reaches the total.
    #[test]
    fn a_broken_file_costs_only_itself() {
        let paths: Vec<std::path::PathBuf> = (0..40).map(|i| format!("/p/{i}").into()).collect();
        let p = super::Progress::default();
        let got = super::on_all_cores(&paths, &p, |path| match path.ends_with("7") {
            true => panic!("a reader that breaks"),
            false => Some(1),
        });
        assert_eq!(got.len(), 40);
        assert_eq!(got.iter().filter(|g| g.is_none()).count(), 1);
        assert!(got[7].is_none() && got[8].is_some());
    }

    /// Files of one hash are grouped only when their bytes are the same.
    #[test]
    fn compares_the_bytes() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bytes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::path::PathBuf> = ["a", "b", "c"].iter().map(|n| dir.join(n)).collect();
        std::fs::write(&files[0], vec![7u8; 100_000]).unwrap();
        std::fs::write(&files[1], vec![7u8; 100_000]).unwrap();
        let mut other = vec![7u8; 100_000];
        other[99_999] = 8;
        std::fs::write(&files[2], other).unwrap();
        let groups = super::same_contents(files.clone(), &super::Progress::default());
        assert_eq!(groups, [files[..2].to_vec()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One file reached by two paths whose real paths differ, through a bind mount or a hard
    /// link, is no duplicate of itself, so "all but the first" never checks its only copy.
    #[test]
    fn one_file_reached_twice_is_one_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-twice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<std::path::PathBuf> = ["a", "b", "c"].iter().map(|n| dir.join(n)).collect();
        std::fs::write(&files[0], vec![7u8; 1000]).unwrap();
        std::fs::hard_link(&files[0], &files[1]).unwrap();
        std::fs::write(&files[2], vec![7u8; 1000]).unwrap();
        let p = super::Progress::default();
        let groups = super::find(files.clone(), super::By::Checksum, false, false, &p);
        assert_eq!(groups, [vec![files[0].clone(), files[2].clone()]]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Stop leaves a file being read, and the search, at once: the reads look at Cancel, and
    /// the window no longer waits for the thread's answer.
    #[test]
    fn stop_leaves_at_once() {
        let dir = std::env::temp_dir().join(format!("glimt-test-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a");
        std::fs::write(&file, vec![7u8; 100_000]).unwrap();
        let p = super::Progress::default();
        assert!(super::checksum(&file, &p).is_some());
        assert!(super::same_bytes(&file, &file, &p));
        p.is_cancelled.store(true, Ordering::Relaxed);
        assert!(super::checksum(&file, &p).is_none());
        assert!(!super::same_bytes(&file, &file, &p));
        std::fs::remove_dir_all(dir).unwrap();
        let mut find_duplicates = FindDuplicates::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        find_duplicates.groups_rx = Receiving::of(rx);
        find_duplicates.searched = 5;
        find_duplicates.stop();
        assert!(
            find_duplicates
                .progress
                .is_cancelled
                .load(Ordering::Relaxed)
        );
        assert!(!find_duplicates.groups_rx.is_some() && find_duplicates.searched == 0);
    }

    use super::*;

    /// The band and row means never rule out a pair that is alike enough.
    #[test]
    fn bands_never_miss_a_match() {
        let mut seed = 1u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        for _ in 0..2000 {
            let a: Vec<u8> = (0..768).map(|_| next()).collect();
            // Changed by up to ±40 here and there, more in one band than the others.
            let b: Vec<u8> = (a.iter().enumerate())
                .map(|(i, &v)| match (i < 192, next() % 3) {
                    (true, 0) => v.saturating_add(next() % 40),
                    (_, 1) => v.saturating_sub(next() % 20),
                    _ => v,
                })
                .collect();
            let alike = likeness(&a, &b);
            let apart: f32 = (bands(&a).iter().zip(bands(&b)))
                .map(|(x, y)| (x - y).abs())
                .sum();
            let reach = (100.0 - alike) * 255.0 / 100.0;
            assert!(apart <= 4.0 * reach + 0.01, "{apart} > 4 × {reach}");
            let rows = super::difference(&part_means::<16>(&a), &part_means::<16>(&b));
            assert!(rows <= 16.0 * reach + 0.01, "{rows} > 16 × {reach}");
        }
    }

    /// A block that meets 5,000 copies of one picture keeps them once for each part, not once
    /// for each picture of the block: a picture found by one before it in its part is not
    /// compared again.
    #[test]
    fn a_block_keeps_a_crowd_once_a_part() {
        let ks: Vec<usize> = (0..512).collect();
        let each = block_matches(&ks, 4, |k| (k + 1..5000).collect());
        let kept: Vec<usize> = (each.iter().flatten()).map(Vec::len).collect();
        assert_eq!(kept, [4999, 4871, 4743, 4615]);
        assert_eq!(each.len(), 512);
    }

    /// The matches kept for a part count toward the next block's size also when the part's
    /// first picture went to a group before it: here 4 takes 1,000 that are never replayed.
    #[test]
    fn a_block_counts_what_its_parts_held() {
        let ks: Vec<usize> = (0..8).collect();
        let mut grouped = vec![false; 2000];
        let mut found = Vec::new();
        let matches_kept = group_block(&ks, 2, &mut grouped, &mut found, |k, _| match k {
            0 => vec![4],
            4 => (1000..2000).collect(),
            _ => Vec::new(),
        });
        assert_eq!(found, [[0, 4]]);
        assert_eq!(matches_kept, 1001);
    }

    /// Pictures left out of a part, as its first picture found them, are found again on all
    /// cores when another group takes that first picture, not one at a time on this thread.
    #[test]
    fn matches_left_out_are_found_again_in_parallel() {
        let ks: Vec<usize> = (0..8).collect();
        let mut grouped = vec![false; 8];
        let mut found = Vec::new();
        let on = std::sync::Mutex::new(Vec::new());
        let here = std::thread::current().id();
        group_block(&ks, 2, &mut grouped, &mut found, |k, _| {
            if k > 4 {
                on.lock().unwrap().push(std::thread::current().id());
            }
            match k {
                0 => vec![4],
                4 => vec![5, 6, 7],
                5 => vec![6],
                _ => Vec::new(),
            }
        });
        assert_eq!(found, [[0, 4], [5, 6]]);
        let on = on.into_inner().unwrap();
        assert!(!on.is_empty() && !on.contains(&here), "{on:?}");
    }

    /// Groups made on all cores are those one thread makes comparing every pair, also with
    /// thousands of copies of one picture and pictures that chain.
    #[test]
    fn groups_as_one_thread_makes_them() {
        let mut seed = 5u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 16
        };
        // Pictures, and near copies of the one before.
        let mut sigs: Vec<Option<Vec<u8>>> = Vec::new();
        for k in 0..3000 {
            let sig: Vec<u8> = match (k % 3, sigs.last()) {
                (2, Some(Some(last))) => last
                    .iter()
                    .map(|&v| v.saturating_add((next() % 12) as u8))
                    .collect(),
                _ => {
                    // Of a few brightnesses, each row its own.
                    let base = 60 + (next() % 4) as i32 * 40;
                    let rows: Vec<i32> = (0..16).map(|_| (next() % 200) as i32 - 100).collect();
                    (0..768)
                        .map(|i| (base + rows[i / 48]).clamp(0, 255) as u8)
                        .collect()
                }
            };
            sigs.push(Some(sig));
        }
        // Then 2,000 copies of one, and one that could not be read.
        let first = sigs[0].clone();
        sigs.extend((0..2000).map(|_| first.clone()));
        sigs.push(None);
        let paths: Vec<PathBuf> = (0..sigs.len()).map(|i| format!("/p/{i}").into()).collect();
        for percent in [95, 90, 85] {
            let got = similar(paths.clone(), sigs.clone(), percent, &Progress::default());
            // One thread, every pair, in the same order of means.
            let mut pictures: Vec<(f32, usize)> = (sigs.iter().enumerate())
                .filter_map(|(i, s)| Some((mean(s.as_ref()?), i)))
                .collect();
            pictures.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut grouped = vec![false; pictures.len()];
            let mut want = Vec::new();
            for k in 0..pictures.len() {
                if grouped[k] {
                    continue;
                }
                let mut group = vec![paths[pictures[k].1].clone()];
                for n in k + 1..pictures.len() {
                    let (a, b) = (sigs[pictures[k].1].as_ref(), sigs[pictures[n].1].as_ref());
                    if !grouped[n] && likeness(a.unwrap(), b.unwrap()) >= percent as f32 {
                        grouped[n] = true;
                        group.push(paths[pictures[n].1].clone());
                    }
                }
                if group.len() > 1 {
                    want.push(group);
                }
            }
            assert!(want.len() > 10, "{percent}%: {} groups", want.len());
            assert_eq!(got, want, "{percent}%");
        }
    }

    /// Similarity on many pictures, timed: run by hand with photos in GLIMT_PHOTOS, as
    /// `GLIMT_PHOTOS=~/Pictures cargo test --release similarity_at_scale -- --ignored
    /// --nocapture`. Each picture is a crop of one of them at its own place and size, so each has
    /// a photo's light and shapes, and few are alike.
    #[test]
    #[ignore]
    fn similarity_at_scale() {
        let dir = std::env::var("GLIMT_PHOTOS").expect("GLIMT_PHOTOS, a folder of photos");
        let photos: Vec<image::RgbaImage> = (walkdir_images(std::path::Path::new(&dir)).iter())
            .filter_map(|p| image::open(p).ok())
            .map(|i| i.thumbnail(256, 256).to_rgba8())
            .filter(|i| i.width().min(i.height()) > 32)
            .collect();
        println!("{} photos", photos.len());
        let mut seed = 13u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 8
        };
        for n in [5_000, 20_000, 50_000, 100_000] {
            let sigs: Vec<Option<Vec<u8>>> = (0..n)
                .map(|k| {
                    let photo = &photos[k % photos.len()];
                    let (w, h) = photo.dimensions();
                    let side = 32 + next() % (w.min(h) - 32);
                    let (x, y) = (next() % (w - side + 1), next() % (h - side + 1));
                    Some(signature(
                        &image::imageops::crop_imm(photo, x, y, side, side).to_image(),
                    ))
                })
                .collect();
            let paths: Vec<PathBuf> = (0..n).map(|i| format!("/p/{i}").into()).collect();
            for percent in [95, 90, 85] {
                let start = std::time::Instant::now();
                let groups = similar(paths.clone(), sigs.clone(), percent, &Progress::default());
                println!(
                    "{n:>7} {percent}%: {:>6.0} ms, {} groups",
                    start.elapsed().as_secs_f64() * 1000.0,
                    groups.len()
                );
            }
        }
    }

    /// `find` by similarity over every file in `GLIMT_PHOTOS`, on all threads: the signatures
    /// made, then read from the thumbnail cache.
    #[test]
    #[ignore]
    fn find_similar_at_scale() {
        let dir = std::env::var("GLIMT_PHOTOS").expect("GLIMT_PHOTOS, a folder of photos");
        let paths = walkdir_images(std::path::Path::new(&dir));
        let n = paths.len();
        // Saved to the cache the first time and read from it the second.
        for _ in 0..2 {
            let start = std::time::Instant::now();
            let groups = find(
                paths.clone(),
                By::Similar(90),
                true,
                true,
                &Progress::default(),
            );
            let s = start.elapsed().as_secs_f64();
            println!(
                "{n} files: {s:.1} s, {:.2} ms each, {} groups",
                s * 1000.0 / n as f64,
                groups.len()
            );
        }
    }

    /// The JPEG and PNG files in `dir` and below.
    fn walkdir_images(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir_images(&p));
            } else if (p.extension().and_then(|e| e.to_str()))
                .is_some_and(|e| matches!(e.to_lowercase().as_str(), "jpg" | "jpeg" | "png"))
            {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn finds_duplicates() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-find_duplicates-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let (a, b, c, d) = (
            dir.join("a.png"),
            dir.join("sub/a.png"),
            dir.join("c.png"),
            dir.join("d.png"),
        );
        let gradient =
            image::RgbaImage::from_fn(64, 48, |x, _| image::Rgba([x as u8 * 4, 0, 0, 255]));
        gradient.save(&a).unwrap();
        std::fs::copy(&a, &b).unwrap();
        // c looks like a but is another file; d is something else.
        image::RgbaImage::from_fn(64, 48, |x, y| {
            image::Rgba([x as u8 * 4, (y == 0) as u8, 0, 255])
        })
        .save(&c)
        .unwrap();
        image::RgbaImage::from_pixel(64, 48, image::Rgba([0, 0, 255, 255]))
            .save(&d)
            .unwrap();
        let all = vec![a.clone(), b.clone(), c.clone(), d.clone()];
        let p = Progress::default();
        assert_eq!(
            find(all.clone(), By::Checksum, false, false, &p),
            [[a.clone(), b.clone()]]
        );
        assert_eq!(
            find(all.clone(), By::Name, false, false, &p),
            [[a.clone(), b.clone()]]
        );
        assert_eq!(find(all.clone(), By::Dimensions, false, false, &p).len(), 1);
        assert_eq!(
            find(all.clone(), By::Similar(95), false, false, &p),
            [[a.clone(), c, b]]
        );
        assert_eq!(p.done.load(Ordering::Relaxed), 4);
        // A link to a is not a duplicate of it.
        let link = dir.join("0link.png");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let linked = vec![a.clone(), link, d.clone()];
        assert!(find(linked, By::Checksum, false, false, &p).is_empty());
        // Nor is a reached through a linked folder.
        let folder = dir.join("linked");
        std::os::unix::fs::symlink(&dir, &folder).unwrap();
        let twice = vec![a.clone(), folder.join("a.png"), d.clone()];
        assert!(find(twice, By::Checksum, false, false, &p).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
