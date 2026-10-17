//! Lightroom Classic catalogs (`.lrcat`, an SQLite database), read once through the `sqlite3`
//! program: the collections, the keyword tree, the stars and each image's keywords, for Glimt's
//! own files. Only columns that have been the same for many Lightroom versions are read. Also
//! File → Import Lightroom Catalog's dialog, what it will do (`Catalog::plan`) and the import.

use crate::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One query, so the catalog is read in one go: a row for each root folder (r), image (i),
/// collection (c), image in a collection (m), keyword (k) and keyword of an image (w), with five
/// columns each.
const QUERY: &str = "\
SELECT 'r', id_local, absolutePath, '', '' FROM AgLibraryRootFolder
UNION ALL SELECT 'i', i.id_local, f.rootFolder,
    f.pathFromRoot || fi.baseName || CASE WHEN fi.extension = '' THEN '' ELSE '.' || fi.extension END,
    CAST(coalesce(i.rating, 0) AS INTEGER)
    FROM Adobe_images i JOIN AgLibraryFile fi ON fi.id_local = i.rootFile
    JOIN AgLibraryFolder f ON f.id_local = fi.folder
UNION ALL SELECT 'c', id_local, coalesce(parent, 0), coalesce(name, ''), coalesce(creationId, '')
    FROM AgLibraryCollection
UNION ALL SELECT 'm', collection, image, '', '' FROM AgLibraryCollectionImage
UNION ALL SELECT 'k', id_local, coalesce(parent, 0), coalesce(name, ''), '' FROM AgLibraryKeyword
UNION ALL SELECT 'w', image, tag, '', '' FROM AgLibraryKeywordImage;";

/// Between the columns and after the rows: neither is in a file name or keyword.
const COLUMN: u8 = 0x1f;
const ROW: u8 = 0x1e;

#[derive(Default)]
pub struct Catalog {
    /// The root folders as the catalog names them, with a slash at the end.
    pub roots: Vec<(i64, String)>,
    /// Image: (root folder, path below it, stars).
    images: HashMap<i64, (i64, String, u8)>,
    /// Collection: (parent, name, kind).
    collections: HashMap<i64, (i64, String, String)>,
    /// (collection, image)
    members: Vec<(i64, i64)>,
    /// Keyword: (parent, name).
    keywords: HashMap<i64, (i64, String)>,
    /// (image, keyword)
    tagged: Vec<(i64, i64)>,
}

/// Read `file`; an error says why in words for the user.
pub fn read(file: &Path) -> Result<Catalog, String> {
    if file.with_extension("lrcat.lock").exists() {
        return Err("Lightroom has the catalog open: close Lightroom first".into());
    }
    let out = crate::external::command("sqlite3")
        .args([
            "-readonly",
            "-batch",
            "-noheader",
            "-separator",
            "\x1f",
            "-newline",
            "\x1e",
        ])
        .arg(file)
        .arg(QUERY)
        .output()
        .map_err(|e| format!("{NEEDS}: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        if missing(out.status.code(), &why) {
            return Err(format!("{NEEDS}, which is not installed"));
        }
        return Err(format!(
            "Not a Lightroom Classic catalog Glimt can read: {}",
            why.trim()
        ));
    }
    Ok(parse(&out.stdout))
}

const NEEDS: &str = "Reading a Lightroom catalog needs the sqlite3 program";

/// Whether sqlite3 is not there: in a Flatpak, flatpak-spawn starts but cannot run it on the
/// host, and a shell says 127.
fn missing(code: Option<i32>, stderr: &str) -> bool {
    code == Some(127) || stderr.contains("Failed to execute child process")
}

fn parse(text: &[u8]) -> Catalog {
    let mut c = Catalog::default();
    for row in text.split(|&b| b == ROW) {
        let cols: Vec<String> = (row.split(|&b| b == COLUMN))
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let [kind, id, a, b, d] = &cols[..] else {
            continue;
        };
        let (Ok(id), n) = (id.parse::<i64>(), a.parse::<i64>()) else {
            continue;
        };
        match (kind.as_str(), n) {
            ("r", _) => c.roots.push((id, a.clone())),
            ("i", Ok(n)) => drop(
                c.images
                    .insert(id, (n, b.clone(), d.parse().unwrap_or(0).min(5))),
            ),
            ("c", Ok(n)) => drop(c.collections.insert(id, (n, b.clone(), d.clone()))),
            ("m", Ok(n)) => c.members.push((id, n)),
            ("k", Ok(n)) => drop(c.keywords.insert(id, (n, b.clone()))),
            ("w", Ok(n)) => c.tagged.push((id, n)),
            _ => {}
        }
    }
    c
}

impl Catalog {
    /// The path of each image, its root folder replaced by `now`, the folder where the user
    /// says it is now (in the order of `roots`).
    pub fn paths(&self, now: &[PathBuf]) -> HashMap<i64, PathBuf> {
        // A root folder the user gave no place is left out.
        let roots: HashMap<i64, &PathBuf> = (self.roots.iter().map(|r| r.0))
            .zip(now)
            .filter(|r| !r.1.as_os_str().is_empty())
            .collect();
        // Only a path below the root: one with .. or an absolute one, in a catalog from
        // elsewhere, would put sidecars outside the folder the user chose.
        let below_root = |below: &String| {
            (Path::new(below).components()).all(|c| matches!(c, std::path::Component::Normal(_)))
        };
        (self.images.iter())
            .filter(|(_, (_, below, _))| below_root(below))
            .filter_map(|(&id, (root, below, _))| Some((id, roots.get(root)?.join(below))))
            .collect()
    }

    /// The plain collections (not smart ones, sets or publish services), named with the sets
    /// they are in ("Travel - Norway"), and their images at `paths` (from `Catalog::paths`).
    pub fn collections(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(String, Vec<PathBuf>)> {
        let mut images: HashMap<i64, Vec<PathBuf>> = HashMap::new();
        for (coll, image) in &self.members {
            if let Some(p) = paths.get(image) {
                images.entry(*coll).or_default().push(p.clone());
            }
        }
        let mut all: Vec<(String, Vec<PathBuf>)> = (self.collections.iter())
            .filter(|(_, c)| c.2 == "com.adobe.ag.library.collection")
            .map(|(id, _)| {
                let names = chain(*id, |i| {
                    self.collections.get(&i).map(|c| (c.0, c.1.as_str()))
                });
                let name = names.join(" - ").replace('/', "-");
                let name = name.trim_start_matches('.').to_owned();
                (name, images.remove(id).unwrap_or_default())
            })
            .filter(|(name, images)| !name.trim().is_empty() && !images.is_empty())
            .collect();
        all.sort();
        all
    }

    /// The keywords as keyword tree paths ("Places|Norway|Oslo").
    pub fn keyword_paths(&self) -> Vec<String> {
        (self.keywords.keys())
            .filter_map(|&id| self.keyword_path(id))
            .collect()
    }

    fn keyword_path(&self, id: i64) -> Option<String> {
        let names = chain(id, |i| self.keywords.get(&i).map(|k| (k.0, k.1.as_str())));
        (!names.is_empty()).then(|| names.join("|"))
    }

    /// The keywords of each image, as keyword tree paths, at `paths` (from `Catalog::paths`).
    /// One list for each file: the keywords of its virtual copies too, each once.
    pub fn image_keywords(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, Vec<String>)> {
        let mut per_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (image, keyword) in &self.tagged {
            let (Some(path), Some(tag)) = (paths.get(image), self.keyword_path(*keyword)) else {
                continue;
            };
            let tags = per_file.entry(path.clone()).or_default();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        let mut all: Vec<(PathBuf, Vec<String>)> = per_file.into_iter().collect();
        all.iter_mut().for_each(|(_, tags)| tags.sort());
        all.sort();
        all
    }

    /// The images with stars, at `paths` (from `Catalog::paths`). One value for each file: a
    /// virtual copy is an image of its own on the same file, and the most stars of them count.
    #[cfg_attr(not(feature = "marks"), allow(dead_code))]
    pub fn stars(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, u8)> {
        let mut best: HashMap<PathBuf, u8> = HashMap::new();
        for (id, i) in self.images.iter().filter(|(_, i)| i.2 > 0) {
            if let Some(p) = paths.get(id) {
                let n = best.entry(p.clone()).or_default();
                *n = (*n).max(i.2);
            }
        }
        best.into_iter().collect()
    }

    /// What importing `catalog` with its root folders `now` would do. Made once, when Next is
    /// clicked: it goes through every image of the self.
    // shortcut: made in the window's thread, a short pause for a catalog of some 100,000 images;
    // in a thread with the busy pointer when that is too long.
    pub(crate) fn plan(&self, now: &[String], stars_here: impl Fn(&Path) -> u8) -> Plan {
        let now: Vec<PathBuf> = now.iter().map(|s| root_now(s)).collect();
        let paths = self.paths(&now);
        let collections = self.collections(&paths);
        let there: HashSet<String> = collections::list().into_iter().map(|c| c.0).collect();
        let existing = (collections.iter())
            .filter(|c| there.contains(&c.0))
            .map(|c| c.0.clone())
            .collect();
        #[cfg(feature = "marks")]
        let stars = self.stars(&paths);
        #[cfg(not(feature = "marks"))]
        let stars = Vec::new();
        #[cfg(feature = "marks")]
        let differ = (stars.iter())
            .filter(|(p, n)| ![0, *n].contains(&stars_here(p)))
            .count();
        #[cfg(not(feature = "marks"))]
        let differ = {
            let _ = stars_here;
            0
        };
        Plan {
            collections,
            existing,
            keywords: self.keyword_paths(),
            image_keywords: self.image_keywords(&paths),
            stars,
            differ,
            take_lightroom: false,
        }
    }
}

/// The names from the top down to `id`, through `parent_and_name`; the nameless root that
/// Lightroom keeps keywords under is left out. A loop in a damaged catalog ends after 32 steps.
fn chain<'a>(mut id: i64, parent_and_name: impl Fn(i64) -> Option<(i64, &'a str)>) -> Vec<String> {
    let mut names = Vec::new();
    for _ in 0..32 {
        let Some((parent, name)) = parent_and_name(id) else {
            break;
        };
        if !name.is_empty() {
            names.push(name.replace('|', "/"));
        }
        id = parent;
    }
    names.reverse();
    names
}

/// The folder typed for a root folder: `~/` is the home folder, and a relative path or nothing
/// is no folder (empty), as a collection keeps absolute paths only.
pub fn root_now(typed: &str) -> PathBuf {
    let typed = typed.trim();
    let path = match typed.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(typed),
    };
    if path.is_absolute() {
        path
    } else {
        PathBuf::new()
    }
}

/// Where each root folder is now: the same place if it is there, else empty for the user to
/// fill in.
pub fn roots_now(catalog: &Catalog) -> Vec<String> {
    (catalog.roots.iter())
        .map(|(_, r)| {
            if Path::new(r).is_dir() {
                r.clone()
            } else {
                String::new()
            }
        })
        .collect()
}

/// A Lightroom catalog read, the folders where its root folders are now, and once Next is
/// clicked what importing it will do.
pub(crate) struct Import {
    pub(crate) catalog: Catalog,
    pub(crate) now: Vec<String>,
    pub(crate) plan: Option<Plan>,
}

pub(crate) struct Plan {
    collections: Vec<(String, Vec<PathBuf>)>,
    /// The names of those that are there already.
    existing: Vec<String>,
    keywords: Vec<String>,
    stars: Vec<(PathBuf, u8)>,
    /// Each image's keywords, as keyword tree paths.
    image_keywords: Vec<(PathBuf, Vec<String>)>,
    /// How many images have other stars in Glimt, and whether Lightroom's replace them.
    differ: usize,
    take_lightroom: bool,
}

// ---- Import Lightroom Catalog: the dialog, the plan and the import ----

impl crate::App {
    /// Read a Lightroom catalog in the background; the import dialog opens when it is read.
    pub(crate) fn read_lightroom(&mut self, ctx: &egui::Context, file: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let (busy, ctx) = (self.busy.clone(), ctx.clone());
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = tx.send(read(&file));
            busy.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint();
        });
        self.lightroom_rx = Some(rx);
    }

    /// File → Import Lightroom Catalog: where the catalog's root folders are now; then what the
    /// import will do, with the collections that are there already and the stars that differ;
    /// then Import.
    pub(crate) fn lightroom_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut lr) = self.lightroom.take() else {
            return;
        };
        let (mut next, mut back, mut go) = (false, false, false);
        let modal = egui::Modal::new(egui::Id::new("lightroom")).show(ctx, |ui| {
            ui.strong("Import from Lightroom");
            match &mut lr.plan {
                None => {
                    ui.label("Where the catalog's folders are now (empty: left out):");
                    egui::Grid::new("lightroom roots").show(ui, |ui| {
                        for ((_, was), now) in lr.catalog.roots.iter().zip(&mut lr.now) {
                            ui.label(was);
                            // A size of its own: in the grid the field would be as narrow as
                            // its text.
                            let height = ui.spacing().interact_size.y;
                            ui.add_sized([360.0, height], egui::TextEdit::singleline(now));
                            // A few folders, only while the dialog is open.
                            ui.label(if root_now(now).is_dir() { "✓" } else { "" });
                            ui.end_row();
                        }
                    });
                    ui.weak(
                        "Collections are added to Glimt's, keywords to the keyword tree and \
                         stars to the marks. Smart collections are left out.",
                    );
                    ui.horizontal(|ui| {
                        next = ui.button("Next").clicked();
                        if ui.button("Cancel (Esc)").clicked() {
                            ui.close();
                        }
                    });
                }
                Some(plan) => {
                    ui.label(format!(
                        "{}, {} and the stars of {}.",
                        count(plan.collections.len(), "collection"),
                        count(plan.keywords.len(), "keyword"),
                        count(plan.stars.len(), "image"),
                    ));
                    if !plan.existing.is_empty() {
                        let mut names = plan.existing[..plan.existing.len().min(5)].join(", ");
                        if plan.existing.len() > 5 {
                            names += &format!(" and {} more", plan.existing.len() - 5);
                        }
                        ui.label(format!(
                            "There already, so they get only the images they lack: {names}."
                        ));
                    }
                    if !plan.image_keywords.is_empty() {
                        let what = format!(
                            "The keywords of each image, into its XMP sidecar ({}); those it \
                             has stay",
                            count(plan.image_keywords.len(), "image")
                        );
                        ui.checkbox(&mut self.config.lightroom_keywords, what);
                    }
                    if plan.differ > 0 {
                        ui.label(format!(
                            "{} other stars in Glimt:",
                            match plan.differ {
                                1 => "1 image has".to_owned(),
                                n => format!("{} have", count(n, "image")),
                            }
                        ));
                        ui.radio_value(&mut plan.take_lightroom, false, "Keep Glimt's stars");
                        ui.radio_value(&mut plan.take_lightroom, true, "Take Lightroom's stars");
                    }
                    ui.horizontal(|ui| {
                        go = ui.button("Import").clicked();
                        back = ui.button("Back").clicked();
                        if ui.button("Cancel (Esc)").clicked() {
                            ui.close();
                        }
                    });
                }
            }
        });
        if next {
            #[cfg(feature = "marks")]
            let stars_here = |p: &Path| self.marks.get(p).1;
            #[cfg(not(feature = "marks"))]
            let stars_here = |_: &Path| 0;
            lr.plan = Some(lr.catalog.plan(&lr.now, stars_here));
        }
        if back {
            lr.plan = None;
        }
        match lr.plan {
            Some(plan) if go => self.import_lightroom(ctx, plan),
            _ if !modal.should_close() => self.lightroom = Some(lr),
            _ => {}
        }
    }

    /// The keyword tree and stars here; the collections' files, and the stars' XMP when they
    /// go there, in the background.
    pub(crate) fn import_lightroom(&mut self, ctx: &egui::Context, plan: Plan) {
        let Plan {
            collections: all,
            keywords,
            stars,
            take_lightroom,
            image_keywords,
            ..
        } = plan;
        // The choice is kept for the next import.
        self.config.save();
        let image_keywords: Vec<(PathBuf, Vec<String>)> = match self.config.lightroom_keywords {
            true => image_keywords,
            false => Vec::new(),
        };
        // "exif:" searches here read those images' keywords again.
        for (path, _) in &image_keywords {
            if let Ok(name) = path.strip_prefix(&self.dir) {
                self.exif_stale.insert(name.as_os_str().to_owned());
            }
        }
        let mut done = vec![count(keywords.len(), "keyword")];
        self.change_keyword_tree(keywords, None);
        #[cfg(feature = "marks")]
        let stars: Vec<(PathBuf, u8)> = (stars.into_iter())
            .filter(|(p, _)| take_lightroom || self.marks.get(p).1 == 0)
            .collect();
        #[cfg(feature = "marks")]
        {
            for n in 1..=5 {
                let paths: Vec<PathBuf> = (stars.iter().filter(|s| s.1 == n))
                    .map(|s| s.0.clone())
                    .collect();
                if !paths.is_empty() {
                    self.marks.rate(&paths, n);
                }
            }
            done.push(format!("the stars of {}", count(stars.len(), "image")));
        }
        #[cfg(not(feature = "marks"))]
        let _ = (&stars, take_lightroom);
        let xmp = cfg!(feature = "marks") && self.config.xmp_stars;
        if !image_keywords.is_empty() {
            done.push(format!(
                "the keywords of {}",
                count(image_keywords.len(), "image")
            ));
        }
        // The stars (as stars set by hand go there with this setting) and the keywords to the
        // sidecars, through the writer that keeps them in order with what is set meanwhile.
        let mut sidecars: HashMap<PathBuf, (u8, Vec<String>)> = HashMap::new();
        if xmp {
            for (path, n) in &stars {
                sidecars.entry(path.clone()).or_default().0 = *n;
            }
        }
        for (path, tags) in image_keywords {
            sidecars.entry(path).or_default().1 = tags;
        }
        let paths: Vec<PathBuf> = sidecars.keys().cloned().collect();
        self.write_sidecars(
            ctx,
            "Importing",
            paths,
            move |path| {
                // Not for photos gone since: no sidecar beside nothing.
                let Some((n, tags)) = sidecars.get(path).filter(|_| path.is_file()) else {
                    return true;
                };
                #[cfg(feature = "marks")]
                let stars = *n == 0 || marks::write_xmp(path, *n).is_ok();
                #[cfg(not(feature = "marks"))]
                let stars = *n == 0;
                stars && (tags.is_empty() || meta::add_tags(path, tags).is_ok())
            },
            |failed, job| match () {
                _ if failed > 0 => Some(format!(
                    "Lightroom's stars or keywords not written to the sidecars of {}",
                    count(failed, "image")
                )),
                _ if job.stopped() => Some(job.stopped_text()),
                _ => None,
            },
        );
        // The collections: one of the same name gets the images it lacks, nothing is lost.
        self.background(ctx, move || {
            let dir = collections::dir();
            if let Some(dir) = &dir {
                let _ = std::fs::create_dir_all(dir);
            }
            let failed: Vec<&str> = (all.iter())
                .filter(|(name, paths)| {
                    (dir.as_ref()).is_none_or(|d| {
                        collections::append(&d.join(format!("{name}.gqv")), paths).is_err()
                    })
                })
                .map(|(name, _)| name.as_str())
                .collect();
            done.insert(0, count(all.len() - failed.len(), "collection"));
            let mut msg = format!("Imported from Lightroom: {}", done.join(", "));
            if !failed.is_empty() {
                msg += &format!("; not saved: {}", failed.join(", "));
            }
            if dir.is_none() && !all.is_empty() {
                msg += " (no data folder: $HOME is not set)";
            }
            Some(msg)
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// A catalog as sqlite3 writes it: a root folder on a Mac, two images, a collection in a
    /// set, a smart collection, and keywords under Lightroom's nameless root.
    fn sample() -> Vec<u8> {
        let rows = [
            "r\x1f1\x1f/Volumes/Photos/\x1f\x1f",
            "i\x1f10\x1f1\x1f2024/Oslo/a.jpg\x1f4",
            "i\x1f11\x1f1\x1f2024/b.CR3\x1f0",
            "c\x1f20\x1f0\x1fTravel\x1fcom.adobe.ag.library.group",
            "c\x1f21\x1f20\x1fNorway\x1fcom.adobe.ag.library.collection",
            "c\x1f22\x1f0\x1fFive stars\x1fcom.adobe.ag.library.smart_collection",
            "m\x1f21\x1f10\x1f\x1f",
            "m\x1f21\x1f11\x1f\x1f",
            "m\x1f22\x1f10\x1f\x1f",
            "k\x1f30\x1f0\x1f\x1f",
            "k\x1f31\x1f30\x1fPlaces\x1f",
            "k\x1f32\x1f31\x1fOslo\x1f",
            "w\x1f10\x1f32\x1f\x1f",
            "w\x1f11\x1f31\x1f\x1f",
            "w\x1f12\x1f32\x1f\x1f",
            "w\x1f99\x1f32\x1f\x1f",
        ];
        rows.join("\x1e").into_bytes()
    }

    #[test]
    fn reads_collections_keywords_and_stars() {
        let c = super::parse(&sample());
        assert_eq!(c.roots, [(1, "/Volumes/Photos/".to_owned())]);
        let now = [PathBuf::from("/media/disk")];
        let a = PathBuf::from("/media/disk/2024/Oslo/a.jpg");
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        let mut colls = c.collections(&c.paths(&now));
        colls[0].1.sort();
        assert_eq!(colls, [("Travel - Norway".to_owned(), vec![a.clone(), b])]);
        let mut keywords = c.keyword_paths();
        keywords.sort();
        assert_eq!(keywords, ["Places", "Places|Oslo"]);
        // Paths that would leave the root folder: left out.
        let mut odd = sample();
        odd.extend(
            b"\x1ei\x1f13\x1f1\x1f../../home/u/x.jpg\x1f0\x1ei\x1f14\x1f1\x1f/etc/y.jpg\x1f0",
        );
        let paths = super::parse(&odd).paths(&now);
        assert!(paths.contains_key(&10) && !paths.contains_key(&13) && !paths.contains_key(&14));
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 4)]);
        // Each image's keywords as tree paths; one of an image not in the catalog left out.
        let tags = |c: &super::Catalog| c.image_keywords(&c.paths(&now));
        let tag = |p: &PathBuf, t: &[&str]| (p.clone(), t.iter().map(|s| s.to_string()).collect());
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        assert_eq!(tags(&c), [tag(&a, &["Places|Oslo"]), tag(&b, &["Places"])]);
        // A virtual copy with more stars: one value for the file, the most.
        let mut copy = sample();
        copy.extend(b"\x1ei\x1f12\x1f1\x1f2024/Oslo/a.jpg\x1f5");
        let c = super::parse(&copy);
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 5)]);
        // Its keyword too, once for the file.
        assert_eq!(tags(&c)[0], tag(&a, &["Places|Oslo"]));
        assert_eq!(
            super::root_now(" /media/disk "),
            PathBuf::from("/media/disk")
        );
        assert_eq!(super::root_now("Pictures"), PathBuf::new());
    }

    /// sqlite3 not on the host of a Flatpak, or not in PATH, is told from a catalog it cannot read.
    #[test]
    fn tells_sqlite3_missing() {
        let spawn = "Portal call failed: Failed to start command: Failed to execute child \
                     process \u{201c}sqlite3\u{201d} (No such file or directory)";
        assert!(super::missing(Some(255), spawn));
        assert!(super::missing(Some(127), ""));
        assert!(!super::missing(Some(1), "Error: file is not a database"));
    }

    /// sqlite3 output broken in thousands of ways: read without a panic or an endless loop.
    #[test]
    fn survives_broken_output() {
        let now = [PathBuf::from("/x")];
        for text in crate::broken(&sample()) {
            let c = super::parse(&text);
            let paths = c.paths(&now);
            c.collections(&paths);
            c.keyword_paths();
            c.stars(&paths);
            c.image_keywords(&paths);
        }
    }
}
