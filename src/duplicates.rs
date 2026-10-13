//! Find Duplicates, as in Geeqie: groups of images that are the same file, have the same name,
//! size or dimensions, or look alike. Runs over the images of the list in background threads.

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
            groups(candidates, sums)
        }
        By::Dimensions => {
            let dims = parallel(&paths, p, |path| {
                if crate::has_ext(path, &["jpg", "jpeg"])
                    && let Some(size) = jpeg_size(path)
                {
                    return Some(size);
                }
                image::ImageReader::open(path)
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
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(1 << 18)
        .read_to_end(&mut head)
        .ok()?;
    head.starts_with(&[0xff, 0xd8])
        .then(|| crate::raw::size(&head))?
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
                        out.push((i, f(&paths[i])));
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

/// A 64-bit hash of the whole file.
fn checksum(path: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).ok()?;
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

/// The means of the four bands of a signature, top to bottom.
fn bands(s: &[u8]) -> [f32; 4] {
    let mut each = s.chunks(s.len().div_ceil(4)).map(mean);
    [(); 4].map(|_| each.next().unwrap_or(0.0))
}

/// How alike two signatures are, 0–100: 100 less the mean difference of their values.
fn likeness(a: &[u8], b: &[u8]) -> f32 {
    let diff: u32 = a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as u32).sum();
    100.0 - diff as f32 * 100.0 / (255.0 * a.len() as f32)
}

/// Groups of pictures at least `percent` alike, each around its first picture as in Geeqie,
/// so a chain of small changes does not make one long group. Pictures are taken in order of
/// mean value, and each is compared only with those whose mean is close enough to match at
/// all: pictures `percent` alike differ by at most that much in mean too, so none are missed.
/// Within that, the means of the four bands of the picture rule out most pairs before all 768
/// values are compared: their differences add up to at most four times as much.
/// ponytail: still a four-step check for every pair about as bright; slow when hundreds of
/// thousands of pictures are.
fn similar(
    paths: Vec<PathBuf>,
    signatures: Vec<Option<Vec<u8>>>,
    percent: u8,
    p: &Progress,
) -> Vec<Vec<PathBuf>> {
    let mut items: Vec<(f32, usize, [f32; 4], Vec<u8>)> = (signatures.into_iter().enumerate())
        .filter_map(|(i, s)| {
            let s = s?;
            Some((mean(&s), i, bands(&s), s))
        })
        .collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let reach = (100.0 - percent as f32) * 255.0 / 100.0;
    let mut grouped = vec![false; items.len()];
    let mut found = Vec::new();
    for k in 0..items.len() {
        if p.cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        if grouped[k] {
            continue;
        }
        let (mean, i, bands, sig) = &items[k];
        let mut group = vec![paths[*i].clone()];
        for (n, (other_mean, j, other_bands, other)) in items.iter().enumerate().skip(k + 1) {
            if other_mean - mean > reach {
                break;
            }
            let apart: f32 = bands
                .iter()
                .zip(other_bands)
                .map(|(a, b)| (a - b).abs())
                .sum();
            // A little room for rounding: this only rules out pairs that cannot match.
            let near = apart <= 4.0 * reach + 0.01;
            if !grouped[n] && near && likeness(sig, other) >= percent as f32 {
                grouped[n] = true;
                group.push(paths[*j].clone());
            }
        }
        if group.len() > 1 {
            found.push(group);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The band means never rule out a pair that is alike enough.
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
        }
    }

    #[test]
    fn finds_duplicates() {
        // Similarity goes through the colour settings, which another test changes.
        let _held = crate::COLOUR_TEST.lock().unwrap_or_else(|e| e.into_inner());
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
            [[a, c, b]]
        );
        assert_eq!(p.done.load(Ordering::Relaxed), 4);
        // A JPEG's size from its frame header: width first.
        let jpeg = dir.join("e.jpg");
        image::RgbImage::new(40, 20).save(&jpeg).unwrap();
        assert_eq!(jpeg_size(&jpeg), Some((40, 20)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
