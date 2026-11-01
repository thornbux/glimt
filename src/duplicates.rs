//! Find Duplicates, as in Geeqie: groups of images that are the same file, have the same name,
//! size or dimensions, or look alike. Runs over the images of the list in background threads.

use crate::*;
use std::collections::HashMap;
use std::hash::Hasher;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

pub const ALL: [(By, &str); 7] = [
    (By::Checksum, "Checksum (identical files)"),
    (By::Name, "Name"),
    (By::Size, "Size"),
    (By::Dimensions, "Dimensions"),
    (By::Similar(95), "Similarity (high, 95%)"),
    (By::Similar(90), "Similarity (90%)"),
    (By::Similar(85), "Similarity (low, 85%)"),
];

/// How far a search is, and a way to stop it.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub cancel: AtomicBool,
}

/// Groups of two or more of `paths` that match `by`, largest groups first. `rotate` and `save`
/// are passed on to the thumbnails used for similarity.
pub fn find(
    paths: Vec<PathBuf>,
    by: By,
    rotate: bool,
    save: bool,
    p: &Progress,
) -> Vec<Vec<PathBuf>> {
    // The same file reached twice, by a link to it or through a linked folder, would be its own
    // "duplicate", and checked and trashed the only copy would go: each file once.
    let real = parallel(&paths, p, |path| path.canonicalize().ok());
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<PathBuf> = (paths.into_iter().zip(real))
        .filter(|(path, real)| seen.insert(real.clone().unwrap_or_else(|| path.clone())))
        .map(|(path, _)| path)
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
            let sizes = parallel(&paths, p, size);
            groups(paths, sizes)
        }
        By::Checksum => {
            // Only files of the same size can be the same; most are never read.
            let sizes = parallel(&paths, p, size);
            let candidates: Vec<PathBuf> = groups(paths, sizes).into_iter().flatten().collect();
            let sums = parallel(&candidates, p, checksum);
            // The hash only finds them: the files of a group are compared byte by byte, as two
            // that differ could share a 64-bit hash, and these are offered for the trash.
            // A second reading of each file: counted in the progress, and stopped by Cancel.
            let found = groups(candidates, sums);
            p.total
                .store(found.iter().map(Vec::len).sum(), Ordering::Relaxed);
            p.done.store(0, Ordering::Relaxed);
            (found.into_iter())
                .take_while(|_| !p.cancel.load(Ordering::Relaxed))
                .flat_map(|group| same_contents(group, p))
                .collect()
        }
        By::Dimensions => {
            let dims = parallel(&paths, p, |path| {
                if crate::files::is_jpeg(path)
                    && let Some(size) = jpeg_size(path)
                {
                    return Some(size);
                }
                crate::decode::image_reader(path)
                    .ok()?
                    .with_guessed_format()
                    .ok()?
                    .into_dimensions()
                    .ok()
            });
            groups(paths, dims)
        }
        By::Similar(percent) => {
            let signatures = parallel(&paths, p, |path| {
                let thumb = crate::thumbs::thumbnail(path, rotate, save, false).ok()?;
                Some(signature(&thumb))
            });
            similar(paths, signatures, percent, p)
        }
    };
    if p.cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    for g in &mut groups {
        g.sort();
    }
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    groups
}

/// A JPEG's width and height from its first 256 KB, where its frame header is: the decoder
/// would read the whole file. `None` if it is not there, so the decoder is asked after all.
fn jpeg_size(path: &Path) -> Option<(u32, u32)> {
    let head = crate::decode::read_head(path, 1 << 18).ok()?;
    head.starts_with(&[0xff, 0xd8])
        .then(|| crate::raw::jpeg_size(&head))?
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

/// `f` of every path, on all cores, counting in `p`; `None` for all once cancelled.
fn parallel<T: Send>(
    paths: &[PathBuf],
    p: &Progress,
    f: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<Option<T>> {
    p.total.store(paths.len(), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let next = AtomicUsize::new(0);
    let threads = crate::background_threads();
    let mut results: Vec<(usize, Option<T>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= paths.len() || p.cancel.load(Ordering::Relaxed) {
                            return out;
                        }
                        // A file that breaks its reader is left out alone, not the worker's
                        // whole share of the list with it.
                        let found =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&paths[i])));
                        out.push((i, found.ok().flatten()));
                        p.done.fetch_add(1, Ordering::Relaxed);
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
/// Counted in `p` file by file, and stopped by its Cancel: one group may hold thousands.
fn same_contents(group: Vec<PathBuf>, p: &Progress) -> Vec<Vec<PathBuf>> {
    let mut alike: Vec<Vec<PathBuf>> = Vec::new();
    for path in group {
        if p.cancel.load(Ordering::Relaxed) {
            break;
        }
        p.done.fetch_add(1, Ordering::Relaxed);
        match alike.iter_mut().find(|a| same_bytes(&a[0], &path)) {
            Some(a) => a.push(path),
            None => alike.push(vec![path]),
        }
    }
    alike.retain(|a| a.len() > 1);
    alike
}

/// Whether files `a` and `b` hold the same bytes; not when one cannot be read.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(mut a), Ok(mut b)) = (crate::decode::open(a), crate::decode::open(b)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0; 1 << 16], vec![0; 1 << 16]);
    loop {
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

/// A 64-bit hash of the whole file.
fn checksum(path: &Path) -> Option<u64> {
    let mut file = crate::decode::open(path).ok()?;
    let mut hasher = std::hash::DefaultHasher::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        match file.read(&mut buf).ok()? {
            0 => return Some(hasher.finish()),
            n => hasher.write(&buf[..n]),
        }
    }
}

/// Side of the small picture two pictures are compared by.
const SIDE: u32 = 16;

/// The picture shrunk to 16×16, its red, green and blue values in a row.
fn signature(img: &image::RgbaImage) -> Vec<u8> {
    let small = image::imageops::resize(img, SIDE, SIDE, image::imageops::FilterType::Triangle);
    small.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

fn mean(s: &[u8]) -> f32 {
    s.iter().map(|&v| v as f32).sum::<f32>() / s.len() as f32
}

/// The means of `N` equal parts of a signature, top to bottom: four bands, or its 16 rows.
fn parts<const N: usize>(s: &[u8]) -> [f32; N] {
    let mut each = s.chunks(s.len().div_ceil(N)).map(mean);
    [(); N].map(|_| each.next().unwrap_or(0.0))
}

fn bands(s: &[u8]) -> [f32; 4] {
    parts(s)
}

/// The sum of the differences of two lists of part means.
fn apart(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// How alike two signatures are, 0–100: 100 less the mean difference of their values.
fn likeness(a: &[u8], b: &[u8]) -> f32 {
    let diff: u32 = a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as u32).sum();
    100.0 - diff as f32 * 100.0 / (255.0 * a.len() as f32)
}

/// Whether two signatures are at least `percent` alike, as `likeness` says; given up on, a row
/// at a time, once they differ too much.
fn alike(a: &[u8], b: &[u8], percent: u8) -> bool {
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

/// A picture to compare: its mean, its place in the list, its band and row means, its signature.
type Item = (f32, usize, [f32; 4], [f32; 16], Vec<u8>);

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
    p: &Progress,
) -> Vec<Vec<PathBuf>> {
    let mut items: Vec<Item> = (signatures.into_iter().enumerate())
        .filter_map(|(i, s)| {
            let s = s?;
            Some((mean(&s), i, bands(&s), parts(&s), s))
        })
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let reach = (100.0 - percent as f32) * 255.0 / 100.0;
    // Those alike enough with `k` after it, of those not in a group when its block began.
    let matches = |k: usize, grouped: &[bool]| -> Vec<usize> {
        let (mean, _, bands, rows, sig) = &items[k];
        let mut out = Vec::new();
        for (n, (other_mean, _, other_bands, other_rows, other)) in
            items.iter().enumerate().skip(k + 1)
        {
            if other_mean - mean > reach {
                break;
            }
            // A little room for rounding: these only rule out pairs that cannot match.
            if !grouped[n]
                && apart(bands, other_bands) <= 4.0 * reach + 0.01
                && apart(rows, other_rows) <= 16.0 * reach + 0.01
                && alike(sig, other, percent)
            {
                out.push(n);
            }
        }
        out
    };
    p.total.store(items.len(), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let mut grouped = vec![false; items.len()];
    let mut found = Vec::new();
    // Small while many match (thousands of copies of one picture), so the matches kept for a
    // block stay few; up to 512 pictures once they match few.
    let (mut k, mut block) = (0, 1);
    while k < items.len() {
        if p.cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let ks: Vec<usize> = (k..(k + block).min(items.len()))
            .filter(|&k| !grouped[k])
            .collect();
        let each = in_block(&ks, crate::background_threads(), |k| matches(k, &grouped));
        let mut kept = 0;
        for (&k, alike) in ks.iter().zip(each) {
            // Taken by a picture before it in the block: not a group of its own.
            if grouped[k] {
                continue;
            }
            // Left out as found by a picture before it, which another group took after all.
            let alike = alike.unwrap_or_else(|| matches(k, &grouped));
            kept += alike.len();
            let mut group = vec![paths[items[k].1].clone()];
            for n in alike {
                if !grouped[n] {
                    grouped[n] = true;
                    group.push(paths[items[n].1].clone());
                }
            }
            if group.len() > 1 {
                found.push(group);
            }
        }
        k += block;
        p.done.store(k.min(items.len()), Ordering::Relaxed);
        block = match kept > 1_000_000 {
            true => (block / 2).max(1),
            false => (block * 2).min(512),
        };
    }
    found
}

/// `matches` of each of `ks` (in order, from low to high), on `threads` at once; `None` for one
/// that a picture before it in the same part found, which then takes it. Without that, a block
/// of 512 that met 50,000 copies of one picture kept every copy 512 times over.
fn in_block(
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

// ---- The Find Duplicates window ----

impl crate::App {
    /// The Find Duplicates window: compares the images of the list in the background, and
    /// lists the groups found; checked files can go to the trash.
    pub(crate) fn duplicates_window(&mut self, ctx: &egui::Context) {
        let Some(mut dups) = self.dups.take() else {
            return;
        };
        match dups.groups_rx.take() {
            Took::Nothing => {}
            Took::Gone => {
                dups.searched = 0;
                self.stopped(tr("Find Duplicates"));
            }
            Took::Answer(groups) => {
                // Stopped: no result, not "0 groups" as if every image had been compared.
                if dups.progress.cancel.load(Ordering::Relaxed) {
                    dups.searched = 0;
                }
                dups.groups = groups;
                dups.sizes.clear();
                dups.checked.clear();
                dups.remove(&HashSet::new());
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
                        count = count(self.list.files.len(), "image")
                    ));
                    let label = ALL.iter().find(|a| a.0 == dups.by).map_or("", |a| a.1);
                    egui::ComboBox::from_id_salt("compare by")
                        .selected_text(tr(label))
                        .show_ui(ui, |ui| {
                            for (by, label) in ALL {
                                ui.selectable_value(&mut dups.by, by, tr(label));
                            }
                        });
                    if dups.groups_rx.is_some() {
                        if ui.button(tr("Stop")).clicked() {
                            dups.progress.cancel.store(true, Ordering::Relaxed);
                        }
                    } else {
                        start = ui
                            .add_enabled(self.list.files.len() > 1, egui::Button::new(tr("Find")))
                            .clicked();
                    }
                });
                if dups.groups_rx.is_some() {
                    let (done, total) = (
                        dups.progress.done.load(Ordering::Relaxed),
                        dups.progress.total.load(Ordering::Relaxed),
                    );
                    ui.label(tf!(
                        "Comparing… {done} of {total}",
                        done = thousands(done),
                        total = thousands(total)
                    ));
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                if dups.searched == 0 {
                    if !self.recursive {
                        ui.weak(tr(
                            "Include Subfolders in the View menu searches the folders below too.",
                        ));
                    }
                    return;
                }
                let files: usize = dups.groups.iter().map(Vec::len).sum();
                ui.label(tf!(
                    "{groups}, {files}, among {images}",
                    groups = count(dups.groups.len(), "group"),
                    files = count(files, "file"),
                    images = count(dups.searched, "image")
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(tr("Check all but the first of each group"))
                        .clicked()
                    {
                        dups.checked = dups
                            .groups
                            .iter()
                            .flat_map(|g| g[1..].iter().cloned())
                            .collect();
                    }
                    if ui.button(tr("Uncheck all")).clicked() {
                        dups.checked.clear();
                    }
                    let n = dups.checked.len();
                    trash = ui
                        .add_enabled(
                            n > 0,
                            egui::Button::new(tf!("Move {n} checked to trash…", n)),
                        )
                        .clicked();
                });
                ui.separator();
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row_h,
                    dups.rows.len(),
                    |ui, range| {
                        for &(g, f) in &dups.rows[range] {
                            let group = &dups.groups[g];
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
                                let mut on = dups.checked.contains(path);
                                if ui.checkbox(&mut on, "").changed() {
                                    match on {
                                        true => dups.checked.insert(path.clone()),
                                        false => dups.checked.remove(path),
                                    };
                                }
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                if ui.link(name).clicked() {
                                    show = Some(path.clone());
                                }
                                let size = *(dups.sizes.entry(path.clone()))
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
            });
        if start {
            let paths: Vec<PathBuf> = (0..self.list.files.len()).map(|i| self.path(i)).collect();
            let (tx, ctx) = (mpsc::channel(), ctx.clone());
            let (by, progress) = (dups.by, Arc::new(Progress::default()));
            let (rotate, save) = (self.config.exif_rotate, self.config.save_thumbnails);
            dups.searched = paths.len();
            dups.progress = progress.clone();
            dups.groups_rx = Receiving::of(tx.1);
            let sender = tx.0;
            std::thread::spawn(move || {
                let _ = sender.send(find(paths, by, rotate, save, &progress));
                ctx.request_repaint();
            });
        }
        if let Some(path) = show {
            match (0..self.list.files.len()).find(|&i| self.is_file(i, &path)) {
                Some(i) => self.go(ctx, i),
                None => self.visit(ctx, path),
            }
        }
        if trash {
            // The checked files of the list, through the usual delete with its confirmation.
            self.list.selected = (0..self.list.files.len())
                .filter(|&i| dups.checked.contains(&self.path(i)))
                .collect();
            self.selection_changed(ctx);
            // With none selected the delete takes the shown image: only when the list still has
            // every checked file, else it would be another one, or too few.
            if self.list.selected.len() == dups.checked.len() {
                self.request_delete(ctx, true);
            } else {
                self.notice = Some(tr("The checked files are no longer all in the list").into());
            }
        }
        if open {
            self.dups = Some(dups);
        } else {
            dups.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    /// One file that breaks its reader is left out alone; every other file keeps its result,
    /// and the count of files done still reaches the total.
    #[test]
    fn a_broken_file_costs_only_itself() {
        let paths: Vec<std::path::PathBuf> = (0..40).map(|i| format!("/p/{i}").into()).collect();
        let p = super::Progress::default();
        let got = super::parallel(&paths, &p, |path| match path.ends_with("7") {
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
            let rows = super::apart(&parts::<16>(&a), &parts::<16>(&b));
            assert!(rows <= 16.0 * reach + 0.01, "{rows} > 16 × {reach}");
        }
    }

    /// A block that meets 5,000 copies of one picture keeps them once for each part, not once
    /// for each picture of the block: a picture found by one before it in its part is not
    /// compared again.
    #[test]
    fn a_block_keeps_a_crowd_once_a_part() {
        let ks: Vec<usize> = (0..512).collect();
        let each = in_block(&ks, 4, |k| (k + 1..5000).collect());
        let kept: Vec<usize> = (each.iter().flatten()).map(Vec::len).collect();
        assert_eq!(kept, [4999, 4871, 4743, 4615]);
        assert_eq!(each.len(), 512);
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
            let mut items: Vec<(f32, usize)> = (sigs.iter().enumerate())
                .filter_map(|(i, s)| Some((mean(s.as_ref()?), i)))
                .collect();
            items.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut grouped = vec![false; items.len()];
            let mut want = Vec::new();
            for k in 0..items.len() {
                if grouped[k] {
                    continue;
                }
                let mut group = vec![paths[items[k].1].clone()];
                for n in k + 1..items.len() {
                    let (a, b) = (sigs[items[k].1].as_ref(), sigs[items[n].1].as_ref());
                    if !grouped[n] && likeness(a.unwrap(), b.unwrap()) >= percent as f32 {
                        grouped[n] = true;
                        group.push(paths[items[n].1].clone());
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
        // Similarity goes through the colour settings, which another test changes.
        let _held = crate::colour::COLOUR_TEST
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glimt-test-dups-{}", std::process::id()));
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
        // A JPEG's size from its frame header: width first.
        let jpeg = dir.join("e.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        assert_eq!(jpeg_size(&jpeg), Some((40, 20)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
